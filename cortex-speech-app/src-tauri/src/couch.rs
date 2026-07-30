//! couch.rs — "Couch Review": a LAN-only, token-gated phone review page served by the desktop app.
//!
//! The owner starts it from Settings, opens the shown URL on a phone on the SAME Wi-Fi, and reviews
//! clips (listen → correct → save) from the couch. Decisions land in the same database through the
//! same functions the desktop review uses (`record_human_decision_by` + whole-row upsert), so a phone
//! review is indistinguishable from a desktop one.
//!
//! MULTI-REVIEWER. The owner names the people who will review; each gets their OWN token, and therefore
//! their own URL, identity, and undo history. Three properties make more than one reviewer safe — each
//! exists because the single-reviewer design silently broke it:
//!
//!   1. **Attribution.** Every decision records WHO made it (`speech_segments.reviewed_by`, Migration
//!      v43). One shared token made every label anonymous, so the corpus could not answer "who labelled
//!      this?".
//!   2. **Per-reviewer undo.** The undo stack is keyed by reviewer. One shared stack meant B's undo
//!      silently retracted A's last decision — a data-loss bug, not a UI wart.
//!   3. **Leases.** A clip served to one reviewer is leased to them for [`LEASE_TTL`]; another reviewer's
//!      queue skips it, and a decision on someone else's live lease is REFUSED (409). Without this two
//!      phones are handed the same clip and race each other's verdicts. Leases expire, so a reviewer who
//!      closes the page never strands work.
//!
//! Privacy stance (voice is biometric — GDPR Art. 9):
//!   * OFF by default; started explicitly by the owner. Once started, the session is REMEMBERED
//!     across app restarts (couch_session.json, tokens DPAPI-protected at rest, bound to this
//!     library) and resumed at launch — closing the app does not revoke anything, because closing is
//!     not an access decision. Pressing STOP is: it deletes the remembered session and every link
//!     dies at once, provably surviving a restart. Per-reviewer revoke exists for one lost phone.
//!   * Nothing is ever published or relayed by THIS code — no cloud, no tunneling here. (A
//!     router/firewall still defines the actual reachable network; Windows prompts for the inbound
//!     allow on first start.)
//!   * EVERY data endpoint — including audio bytes — requires a per-reviewer random token (244 bits).
//!     No token, no data. A token is a DURABLE credential until Stop/revoke: anyone holding the link
//!     can review, which is the honest cost of links that survive the desktop
//!     (docs/REMOTE_PUBLIC_LINKS_PLAN.md states it rather than hiding it).
//!   * Read + review only: the API can list the queue, stream one clip's audio, record a decision,
//!     and undo it. No export, no settings, no keys, no file paths are reachable.
//!
//! Transport here is plain HTTP. That is honest for a home LAN or a WireGuard tailnet, where the token
//! never crosses a network the owner does not control — and it is why this must NOT be naively
//! port-forwarded: there is no TLS in this server. The one sanctioned public path is DEVICE-TERMINATED
//! TLS in front of it (Tailscale Serve/Funnel — TLS ends on this PC, relays proxy ciphertext), per
//! docs/REMOTE_PUBLIC_LINKS_PLAN.md phase 5.

use crate::db::{Database, SpeechSegment};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The single global server handle (at most one couch session at a time).
static COUCH: Mutex<Option<CouchHandle>> = Mutex::new(None);

/// Fixed default port — memorable, and stable across restarts so the phone bookmark keeps working.
const COUCH_PORT: u16 = 8737;
/// Cap on request bodies (a decision payload is < 1 KB; 256 KB leaves room for very long transcripts).
const MAX_BODY_BYTES: usize = 256 * 1024;
/// How long a served clip stays reserved for the reviewer it was handed to. Long enough to listen,
/// correct and save without the clip being pulled away mid-edit; short enough that a reviewer who closes
/// the page does not strand work for the rest of the session.
const LEASE_TTL: Duration = Duration::from_secs(15 * 60);
/// Clips handed out per queue request. Small batches keep several reviewers moving through DIFFERENT
/// work instead of the first one leasing the whole backlog; the page refetches as it drains.
const QUEUE_BATCH: usize = 25;
/// How many pre-decision snapshots to keep per reviewer for the ↩ button.
///
/// Every decision pushed a FULL `SpeechSegment` clone and nothing ever trimmed it, so a 116-clip
/// session retained 116 whole rows for the life of the process — and this process is meant to run for
/// weeks, healed by the watchdog, across many sessions. Unbounded growth in an always-on server is a
/// leak whether or not any single session notices it.
///
/// 20 is far past use: the page offers ONE ↩ that pops the last decision, and each undo puts the clip
/// straight back in the queue, so walking back twenty is already beyond anything a reviewer does. The
/// cap drops the OLDEST entries — never the most recent, which is the only one anyone reaches for.
const UNDO_DEPTH: usize = 20;
/// Upper bound on named reviewers — each gets a server thread and its own DB connection.
const MAX_REVIEWERS: usize = 8;
/// One clip in every [`SPOT_CHECK_EVERY`] served is a SPOT CHECK: a clip whose correct answer is
/// already known, handed over with its raw (wrong) draft (docs/REMOTE_REVIEW_PLAN.md §2.1).
///
/// Rate is a real trade-off. Too rare and a reviewer can coast for hours before being measured; too
/// frequent and honest reviewers spend their session re-doing work the corpus already has. One in
/// eight puts ~3 checks in every batch — enough to notice a blind-accepter within one sitting, while
/// 7/8 of the reviewer's effort still goes to work that needs doing.
const SPOT_CHECK_EVERY: usize = 8;
/// Longest accepted reviewer name. Names are stored on every row they decide, not shown in a URL.
const MAX_REVIEWER_NAME: usize = 40;
/// Bounded wait for the listening socket to actually come free after `stop()`. See
/// [`release_listener`] for why a wait is needed at all. Worst case ~1.2 s, and only on the failing
/// path — the normal case returns on the first attempt.
const LISTENER_RELEASE_ATTEMPTS: u32 = 12;
const LISTENER_RELEASE_DELAY: Duration = Duration::from_millis(50);
const LISTENER_RELEASE_CONNECT_TIMEOUT: Duration = Duration::from_millis(50);
/// Accept-loop poll interval. `unblock()` is what actually wakes the threads on stop; this bounds the
/// shutdown of any thread it does not reach, so `stop()` can never hang on `join()` (the single-thread
/// version of this hazard was caught live — see `stop`).
const ACCEPT_POLL: Duration = Duration::from_millis(250);

/// Per-reviewer request budget (P1.6). Every desktop IPC command is throttled; these HTTP routes were
/// the one unthrottled way into the database, so a phone stuck in a reload loop could hammer it
/// unbounded. 120/min sustained with a burst of 60 is far above any human — a reviewer averages a
/// handful per clip (queue, audio, decision, a renewal) — while still stopping a runaway page.
///
/// Keyed by REVIEWER, so one person's misbehaving tab cannot starve anyone else. Applied only AFTER
/// the token resolves: an unauthenticated request is rejected before it touches the database, and the
/// 244-bit token space puts guessing out of reach, so throttling that path would buy nothing.
static COUCH_RATE_LIMITER: crate::throttle::GlobalRateLimiter =
    crate::throttle::GlobalRateLimiter::new_with_burst(120, 60);

/// Per-session state shared by the accept threads. One mutex: the critical sections are map lookups,
/// held for microseconds, while the slow work (DB writes, WAV encoding) happens outside it.
#[derive(Default)]
struct CouchState {
    /// Pre-decision row snapshots, keyed by reviewer name — so undo is per-person and can never
    /// retract someone else's work.
    undo: HashMap<String, Vec<(String, SpeechSegment)>>,
    /// segment id -> (reviewer who holds it, when it was granted). Entries older than [`LEASE_TTL`]
    /// are ignored and pruned, so a lease needs no explicit release on disconnect.
    leases: HashMap<String, (String, Instant)>,
    /// token -> reviewer name. The token is the credential; the name is what lands in the database.
    ///
    /// Lives HERE, in the state every accept thread shares, rather than in a snapshot handed to each
    /// thread at spawn. With per-thread `Arc<HashMap>` copies, revoking a reviewer would have removed
    /// them from the owner's view while every serving thread kept honouring the token forever — a
    /// revoke that revokes nothing. One shared map means removal takes effect on the next request.
    reviewers: HashMap<String, String>,
    /// (segment id, reviewer) pairs this session actually SERVED as spot checks.
    ///
    /// A spot check must be identified by why the clip was HANDED OUT, never by its current state.
    /// The obvious test — "does this row already have a human answer?" — is wrong in a way that
    /// silently breaks ordinary reviewing: a reviewer's own edit MAKES the row human-verified, so
    /// their next submit on it (a retry, or a genuine correction) would be graded as a test and
    /// never written to the corpus. Recording what was served removes the ambiguity entirely.
    ///
    /// Persisted into the remembered session whenever a batch serves new checks, so a check
    /// outstanding across an app restart is still recognized and SCORED when its answer arrives.
    spot_checks: HashSet<(String, String)>,
    /// Where to re-save the remembered session when the served-set grows: (data_dir, db_path).
    /// None outside a durable session (tests, no data dir) — then nothing is persisted, as before.
    session_store: Option<(PathBuf, String)>,
}

impl CouchState {
    /// Who currently holds `segment_id`, ignoring (and pruning) an expired lease.
    fn holder(&mut self, segment_id: &str, now: Instant) -> Option<&str> {
        let expired =
            matches!(self.leases.get(segment_id), Some((_, granted)) if now.duration_since(*granted) >= LEASE_TTL);
        if expired {
            self.leases.remove(segment_id);
            return None;
        }
        self.leases.get(segment_id).map(|(who, _)| who.as_str())
    }
}

struct CouchHandle {
    shutdown: Arc<AtomicBool>,
    /// Shared with the accept-loop threads so `stop()` can `unblock()` them. A bare TcpStream connect
    /// does NOT wake tiny_http (it only yields COMPLETE HTTP requests), so unblock() is the only
    /// non-hanging shutdown — the live end-to-end test caught stop() deadlocking on join() without it.
    server: Arc<tiny_http::Server>,
    port: u16,
    /// The one shared state the accept threads also hold — so `status_of` and `revoke` read and write
    /// the SAME token map the request path authenticates against.
    state: Arc<Mutex<CouchState>>,
    joins: Vec<std::thread::JoinHandle<()>>,
    /// Where the remembered session lives, so `stop()` — the explicit revoke — can delete it. Carried
    /// on the handle because `stop()` has no other context, and forgetting to clear the file would
    /// leave "Stop" issuing working links again on the next launch.
    data_dir: Option<PathBuf>,
}

/// One reviewer's private way in. Two people never share a link, so two people never share an identity.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CouchReviewer {
    /// The name recorded on every row this person decides.
    pub name: String,
    /// Full URL (with this reviewer's token) to open on their phone.
    pub url: String,
    /// The same page over the owner's Tailscale tailnet (works from ANY network, end-to-end encrypted
    /// WireGuard between devices in the tailnet — never the public internet). None when this machine
    /// has no Tailscale interface. The transport is Tailscale's; the couch token gate still applies.
    pub tailscale_url: Option<String>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CouchStatus {
    pub running: bool,
    /// One entry per named reviewer, each with its own token/URL. Empty when stopped.
    pub reviewers: Vec<CouchReviewer>,
}

impl CouchStatus {
    fn stopped() -> Self {
        CouchStatus { running: false, reviewers: Vec::new() }
    }
}

/// Normalize the owner's reviewer list into distinct, storable names.
///
/// A name is written onto every row that person decides, so it must be a plain label: trimmed,
/// non-empty, bounded, and free of control characters (which would corrupt a CSV/JSONL export of the
/// provenance column). Duplicates are folded case-insensitively — two tokens both writing "Sara" would
/// look like one reviewer in the data while behaving as two, which is exactly the ambiguity attribution
/// exists to remove. An empty list means the owner alone, reviewing under the default name.
fn normalize_reviewers(names: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in names {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if name.chars().count() > MAX_REVIEWER_NAME {
            return Err(format!("Reviewer name '{name}' is longer than {MAX_REVIEWER_NAME} characters"));
        }
        if name.chars().any(|c| c.is_control()) {
            return Err("Reviewer names must not contain control characters".to_string());
        }
        if out.iter().any(|existing| existing.eq_ignore_ascii_case(name)) {
            return Err(format!("Duplicate reviewer name '{name}' — each reviewer needs a distinct name"));
        }
        out.push(name.to_string());
    }
    if out.is_empty() {
        out.push(DEFAULT_REVIEWER.to_string());
    }
    if out.len() > MAX_REVIEWERS {
        return Err(format!("At most {MAX_REVIEWERS} reviewers can review at once (got {})", out.len()));
    }
    Ok(out)
}

/// The name used when the owner starts the server without naming anyone.
const DEFAULT_REVIEWER: &str = "owner";

/// Where a started session is remembered, so a link keeps working after the app is closed and
/// reopened (`{data_dir}/couch_session.json`).
const SESSION_FILE: &str = "couch_session.json";

/// One remembered session: the reviewer names and the tokens their links carry.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SavedSession {
    /// token -> reviewer name, the same map `CouchState::reviewers` holds at runtime.
    reviewers: HashMap<String, String>,
    /// The database the session was serving. A session remembered against a DIFFERENT library must
    /// not silently resume: the links would hand out clips from a corpus the owner did not intend.
    db_path: String,
    /// (segment id, reviewer) pairs served as SPOT CHECKS and not yet answered (phase 4 of
    /// docs/REMOTE_PUBLIC_LINKS_PLAN.md). Durable sessions make restarts routine, and a check served
    /// before a restart used to become unanswerable after it: the in-memory served-set was gone, so
    /// the submit fell into the already-reviewed 409 — score silently lost, reviewer shown a jarring
    /// error for doing exactly what they were asked. `serde(default)` so a pre-phase-4 session file
    /// still loads (missing field = empty set).
    #[serde(default)]
    spot_checks: Vec<(String, String)>,
}

/// Tokens SURVIVE closing the app, and do not survive pressing Stop.
///
/// The original design regenerated every token on every start, which is airtight and unusable: a
/// reviewer's link died whenever the app was restarted, so using the phone at all meant coming back
/// to the desktop for a fresh URL. What the owner needs is to open the page from any device, review,
/// close it, and come back later without touching the PC.
///
/// The distinction that makes this safe is between the two ways a session ends. Closing the app is
/// not a decision about access — nothing calls `stop()` on exit — so the session is remembered.
/// Pressing **Stop** IS that decision, and it deletes this file, so "stopping revokes every link"
/// stays literally true.
///
/// Tokens are DPAPI-protected at rest (same treatment as API keys), so the file is useless if copied
/// off this machine. They are still long-lived credentials, which is the honest cost of a durable
/// link: anyone holding one can review until the owner presses Stop or revokes that reviewer.
fn session_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SESSION_FILE)
}

/// Returns Err when the session could NOT be written. Most callers treat that as advisory — a link
/// that merely forgets itself is recoverable by pressing Start again. `revoke` is the exception and
/// must surface it: a revoke that lives only in memory is silently undone by the next restart, which
/// is the opposite of what the owner just asked for.
fn save_session(
    data_dir: &Path,
    reviewers: &HashMap<String, String>,
    db_path: &str,
    spot_checks: &HashSet<(String, String)>,
) -> Result<(), String> {
    let protected: HashMap<String, String> = reviewers
        .iter()
        .filter_map(|(token, name)| crate::dpapi::protect(token).ok().map(|t| (t, name.clone())))
        .collect();
    if protected.len() != reviewers.len() {
        // Refuse to write a PARTIAL map: a session that resumes with only some links working is
        // worse than one that asks the owner to press Start again, because the missing reviewer sees
        // a dead link and has no way to tell it apart from a bug.
        tracing::warn!("Couch Review session not remembered: a token could not be protected at rest");
        return Err("a token could not be protected at rest".to_string());
    }
    let saved = SavedSession {
        reviewers: protected,
        db_path: db_path.to_string(),
        // Plaintext by design: the pair (segment id, reviewer name) is which clips were handed out
        // as checks — worth protecting from casual reading no more than the queue itself, and DPAPI
        // per entry would make every batch serve cost a round of CryptProtectData for no threat.
        spot_checks: spot_checks.iter().cloned().collect(),
    };
    let path = session_path(data_dir);
    let Ok(json) = serde_json::to_string_pretty(&saved) else {
        return Err("session could not be serialised".to_string());
    };
    // Written via a temp file + atomic replace, like every other state file here: a half-written
    // session would resume with a truncated token map and silently issue links that do not work.
    // UNIQUE temp name per write. save_session is called from any accept thread (one per reviewer)
    // with no lock held — deliberately, since DPAPI plus file IO has no business inside the request
    // mutex — so two of them could previously be inside `std::fs::write` on the SAME
    // `couch_session.json.tmp` at once. Interleaved create/truncate and write there can promote a
    // spliced file, and a session file that will not parse is unrecoverable: load_session gives up
    // silently, resume returns None, and every link already sent is dead. Cheap insurance against a
    // narrow window rather than a proof that the window cannot be hit.
    static SESSION_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "json.tmp.{}.{}",
        std::process::id(),
        SESSION_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = std::fs::write(&tmp, json.as_bytes()).and_then(|()| crate::atomic_file::replace_file(&tmp, &path)) {
        tracing::warn!("Couch Review session not remembered: {e}");
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

fn clear_session(data_dir: &Path) {
    let path = session_path(data_dir);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("Couch Review session file not removed ({}): {e}", path.display());
        }
    }
}

/// What `load_session` recovers: the token->name map and the served-but-unanswered spot checks.
type RememberedSession = (HashMap<String, String>, HashSet<(String, String)>);

/// Read back a remembered session, or None if there is none / it is unreadable / it belongs to a
/// different library.
fn load_session(data_dir: &Path, db_path: &str) -> Option<RememberedSession> {
    let raw = std::fs::read_to_string(session_path(data_dir)).ok()?;
    let saved: SavedSession = serde_json::from_str(&raw).ok()?;
    if saved.db_path != db_path {
        tracing::warn!("Couch Review session ignored: it was started against a different library");
        return None;
    }
    let mut out = HashMap::new();
    for (protected, name) in saved.reviewers {
        // One unreadable token invalidates the whole session rather than half-resuming it: DPAPI
        // fails as a unit (different user/machine), so a partial read means the file is not ours.
        let token = crate::dpapi::unprotect(&protected).ok()?;
        out.insert(token, name);
    }
    (!out.is_empty()).then_some((out, saved.spot_checks.into_iter().collect()))
}

/// Start the couch server for `reviewers` (idempotent: returns the existing session if already running,
/// WITHOUT re-tokenizing — a running session's links must not be invalidated by a status refresh).
pub fn start(db_path: String, reviewers: Vec<String>, data_dir: Option<PathBuf>) -> Result<CouchStatus, String> {
    start_on_port(db_path, reviewers, COUCH_PORT, data_dir)
}

/// Bring back the session the owner last started, if the app was closed without pressing Stop.
///
/// This is what makes the phone usable on the owner's terms: start Couch Review once, then open the
/// same link from any device whenever, close the app in between, and never come back to the desktop
/// for a fresh URL. Returns None when there is nothing remembered, which is the normal first-run and
/// post-Stop state.
pub fn resume(db_path: String, data_dir: &Path) -> Option<CouchStatus> {
    resume_on_port(db_path, data_dir, COUCH_PORT)
}

/// `resume` with the port injected, for the same reason `start_on_port` exists: the resurrection a
/// revoke has to survive happens HERE, in the roster rebuilt from the remembered file, so the test
/// must drive this exact path — and it cannot bind 8737 while the owner's own server is using it.
fn resume_on_port(db_path: String, data_dir: &Path, port: u16) -> Option<CouchStatus> {
    let (remembered, _) = load_session(data_dir, &db_path)?;
    let mut names: Vec<String> = remembered.values().cloned().collect();
    names.sort();
    match start_on_port(db_path, names, port, Some(data_dir.to_path_buf())) {
        Ok(status) => {
            tracing::info!(
                "Couch Review resumed for {} reviewer(s) — previous links still work",
                status.reviewers.len()
            );
            Some(status)
        }
        Err(e) => {
            // Never fatal: a port already in use, or a restore in flight, must not stop the app from
            // opening. The owner can press Start themselves and gets a real error there.
            tracing::warn!("Couch Review could not resume: {e}");
            None
        }
    }
}

/// `start` with the port injected, so the test can drive the REAL start/stop path without squatting
/// the production port.
///
/// This is not test scaffolding for its own sake. The one test covering `start()`'s wiring bound
/// 8737 directly, which meant the project's own gate could not run while Couch Review was running —
/// `cargo test` failed with "os error 10048" for an entirely healthy app, during exactly the daily
/// use the gate exists to protect. Skipping the test when the port was busy was the wrong answer: it
/// would restore the blind spot the test was written to remove (mutation testing found `start()`
/// wholly untested). Injecting the port keeps every assertion and removes the collision.
///
/// The production value is not weakened by being a parameter: `start()` is the only non-test caller
/// and passes `COUCH_PORT`, which `the_session_constants_are_pinned_to_their_real_values` pins to
/// 8737 — so a mutant that changed the constant still fails.
fn start_on_port(
    db_path: String,
    reviewers: Vec<String>,
    port: u16,
    data_dir: Option<PathBuf>,
) -> Result<CouchStatus, String> {
    let mut guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    // P1.3b: don't stand up the phone-review server (a background DB writer on a submit) while a DB
    // restore is reserved. Checked UNDER the COUCH lock — the SAME lock is_running() (the restore fence)
    // reads and that `*guard = Some(handle)` registers under — so the check+register and the fence read
    // are mutex-serialized (airtight, like try_start_import): a restore reserved concurrently either sees
    // the server registered (fence refuses) or is seen here (this refuses). An already-running server is
    // also caught by the fence.
    if crate::commands::restore_pending() {
        return Err(crate::commands::RESTORE_IN_PROGRESS_MSG.to_string());
    }
    if let Some(h) = guard.as_ref() {
        return Ok(status_of(h));
    }
    if db_path == ":memory:" {
        return Err("Couch Review needs a file-backed database".to_string());
    }
    let names = normalize_reviewers(&reviewers)?;

    // One random token PER REVIEWER (2× UUIDv4 = 244 random bits each). Never logged — the token IS
    // the identity, so sharing one would erase the attribution.
    //
    // REUSED from the remembered session when the same person is still on the list, so a link the
    // owner already sent keeps working after the app is closed and reopened. A name that was not in
    // the saved session gets a fresh token, and a name that has been dropped simply stops existing.
    let (remembered, served_checks) =
        data_dir.as_deref().and_then(|dir| load_session(dir, &db_path)).unwrap_or_default();
    let tokens: HashMap<String, String> = names
        .iter()
        .map(|name| {
            let existing = remembered.iter().find(|(_, n)| n == &name).map(|(t, _)| t.clone());
            let token = existing
                .unwrap_or_else(|| format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()));
            (token, name.clone())
        })
        .collect();

    // PRE-FLIGHT the library BEFORE binding anything. Each accept thread opens its own connection, and
    // a failure there could only `return` — leaving the socket bound with nobody accepting. That is the
    // worst available state: connections land in the listen backlog and are never answered, so every
    // phone hangs instead of failing fast, and the watchdog sees an unreachable port that restarting
    // cannot fix. Checking here turns it into an honest error on Start, and never binds a port this
    // server cannot serve.
    if let Err(e) = Database::open(&db_path) {
        return Err(format!("Couch Review cannot open the library: {e}"));
    }

    let server = Arc::new(
        tiny_http::Server::http(("0.0.0.0", port))
            .map_err(|e| format!("Couch server could not bind port {port}: {e}"))?,
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    // Rehydrate the served spot-check set for names still on the roster, so a check outstanding
    // across the restart is recognized and scored when its answer arrives instead of 409ing.
    let spot_checks: HashSet<(String, String)> =
        served_checks.into_iter().filter(|(_, name)| names.contains(name)).collect();
    let session_store = data_dir.as_ref().map(|dir| (dir.clone(), db_path.clone()));
    let state =
        Arc::new(Mutex::new(CouchState { reviewers: tokens, spot_checks, session_store, ..CouchState::default() }));
    // One accept thread per reviewer. tiny_http hands a request to whichever thread is free, so a
    // reviewer downloading a clip no longer blocks another reviewer's save — with a single reviewer this
    // is exactly the previous one-thread server. Each thread owns its own WAL connection (SQLite
    // serializes the writes; `busy_timeout` is already set at open).
    let mut joins = Vec::with_capacity(names.len());
    for index in 0..names.len() {
        match spawn_server_loop(index, server.clone(), db_path.clone(), state.clone(), shutdown.clone()) {
            Ok(join) => joins.push(join),
            Err(e) => {
                // Tear down the threads that DID start before giving up. Returning early with them
                // running would leave orphans serving live tokens on port 8737 with no handle in COUCH —
                // unstoppable from Settings, and blocking every later start with a bind failure.
                shutdown.store(true, Ordering::SeqCst);
                server.unblock();
                for join in joins {
                    let _ = join.join();
                }
                return Err(e);
            }
        }
    }
    // Read the port back off the socket rather than trusting the argument: a caller may pass 0 to
    // mean "any free port", and the issued URLs must carry the port that was ACTUALLY bound.
    let bound = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(port);
    let handle = CouchHandle { shutdown, server, port: bound, state, joins, data_dir: data_dir.clone() };
    let status = status_of(&handle);
    // Remember it only once the server is actually serving. Saving earlier would leave a session file
    // pointing at a start that failed, and the next launch would resume links to nothing.
    if let Some(dir) = data_dir.as_deref() {
        let (reviewers, checks) = {
            let st = lock_state(&handle.state);
            (st.reviewers.clone(), st.spot_checks.clone())
        };
        // Advisory here: a session that fails to persist still serves every link it just issued, and
        // save_session has already logged why. Only `revoke` treats this as an error worth raising.
        let _ = save_session(dir, &reviewers, &db_path, &checks);
    }
    *guard = Some(handle);
    tracing::info!(
        "Couch Review started on port {bound} for {} reviewer(s) (token gated; stop from Settings)",
        names.len()
    );
    Ok(status)
}

/// Stop the couch server and drop every session token (revoking all reviewer links at once).
pub fn stop() -> Result<CouchStatus, String> {
    let mut guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut h) = guard.take() {
        h.shutdown.store(true, Ordering::SeqCst);
        // unblock() makes the accept loops stop yielding requests so the threads exit. (A bare TCP
        // connect does NOT wake tiny_http — it yields only complete HTTP requests — so without this,
        // join() deadlocked; caught by the live end-to-end test.) The loops additionally poll with
        // `recv_timeout`, so a thread unblock() fails to reach still exits within ACCEPT_POLL rather
        // than wedging stop() forever — the same deadlock class, now impossible for any thread count.
        h.server.unblock();
        for join in h.joins.drain(..) {
            let _ = join.join();
        }
        let port = h.port;
        // STOP IS THE REVOKE. Closing the app remembers the session so a link keeps working; pressing
        // Stop is the owner deciding it should not. Forgetting this line would make "stopping revokes
        // every link" a lie the moment the app reopened.
        if let Some(dir) = h.data_dir.as_deref() {
            clear_session(dir);
        }
        // Drop the Server BEFORE waking it. Dropping is what sets tiny_http's internal close flag, so
        // the order decides whether the wake-up connection means "exit" or "serve me".
        drop(h);
        release_listener(port);
        tracing::info!("Couch Review stopped");
    }
    Ok(CouchStatus::stopped())
}

/// Make sure the listening socket is really gone, and not just scheduled to be.
///
/// tiny_http parks a private accept thread in a blocking `accept()`, and that thread — not the
/// `Server` value — is what holds the socket. `Server::drop` sets a close flag and then tries to wake
/// the thread by connecting to its own listening address. That address is `0.0.0.0`, and on Windows
/// connecting to `0.0.0.0` fails outright: it is a wildcard for BINDING, not a destination. So the
/// wake never lands, the thread stays parked, and port 8737 stays bound for as long as the process
/// lives — measured still LISTENing 120 s after `stop()` returned.
///
/// The user-visible cost was total: Settings -> Stop -> Start answered "os error 10048" and remote
/// review stayed dead until the whole app was restarted. Worse, the port kept accepting TCP the whole
/// time with nobody left to answer, so anyone who opened an old link simply hung.
///
/// Connecting to loopback performs the wake tiny_http intended. The bind is what PROVES it worked: a
/// successful connect proves nothing, since the OS accepts into the backlog of a listener that is on
/// its way out.
fn release_listener(port: u16) {
    let wake = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..LISTENER_RELEASE_ATTEMPTS {
        if let Ok(stream) = std::net::TcpStream::connect_timeout(&wake, LISTENER_RELEASE_CONNECT_TIMEOUT) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return; // the probe listener drops here, leaving the port free for the next start()
        }
        std::thread::sleep(LISTENER_RELEASE_DELAY);
    }
    // Not fatal, and deliberately not a panic: the session IS stopped either way. Say so plainly so a
    // later bind failure reads as this, rather than as a mystery.
    tracing::warn!("Couch Review port {port} was still bound after stop; starting again may need the app reopened");
}

/// Revoke ONE reviewer's link without disturbing anyone else (docs/REMOTE_REVIEW_PLAN.md §3.7).
///
/// Before this, removing one person meant stopping the server — which regenerates every token and
/// forces the owner to re-issue links to reviewers who did nothing wrong. Dropping the token from the
/// map is total and immediate: `handle_request` resolves the reviewer FROM the token, so an
/// unrecognised token has no identity and every route answers 401 on the next request.
///
/// Their completed work is untouched, and deliberately so: `reviewed_by`, their spot-check scores and
/// their audit trail are a record of what happened, not a permission that can be withdrawn.
///
/// Refuses the LAST reviewer — a running server nobody can reach is a worse state than a stopped one,
/// and `stop()` is the honest way to express that intent.
/// The revocation itself, separated from the global handle so it is DIRECTLY testable.
///
/// Keeping this inside `revoke()` meant the only way to cover it was to reimplement it in a test —
/// which is exactly what happened, and that test then PASSED against a deliberately broken revoke.
/// A guarantee proven only by a copy of the code is not proven at all.
fn revoke_in(state: &Mutex<CouchState>, name: &str) -> Result<(), String> {
    let mut st = lock_state(state);
    let Some(token) = st.reviewers.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(t, _)| t.clone()) else {
        return Err(format!("No reviewer named '{name}' in this session"));
    };
    if st.reviewers.len() <= 1 {
        return Err("That is the only reviewer — stop Couch Review instead".to_string());
    }
    st.reviewers.remove(&token);
    // Their held clips go back to the pool at once; leaving them leased would strand real work behind
    // someone who can no longer reach the server.
    st.leases.retain(|_, (who, _)| !who.eq_ignore_ascii_case(name));

    // PERSIST, or say so. Dropping the token from the in-memory map denies the lost phone right now,
    // but `couch_session.json` is what `resume()` rebuilds the roster from — and `start_on_port`
    // deliberately RE-ISSUES the remembered token for a remembered name, so a memory-only revoke is
    // undone by the next launch, handing the revoked phone back its original working link. The
    // watchdog relaunches this app unattended every 5 minutes, so that restart is not hypothetical.
    //
    // Snapshot under the lock and write outside it, like the batch-serve path: save_session does DPAPI
    // plus file IO, which has no business inside the request-path mutex.
    let persist =
        st.session_store.clone().map(|(dir, db_path)| (dir, db_path, st.reviewers.clone(), st.spot_checks.clone()));
    drop(st);
    if let Some((dir, db_path, reviewers, checks)) = persist {
        // Access is already denied in memory at this point, so this is not a rollback — it is the
        // difference between "revoked" and "revoked until the next restart", and the owner revoking a
        // LOST PHONE has to be told which one they got.
        save_session(&dir, &reviewers, &db_path, &checks).map_err(|e| {
            format!("Access denied now, but it will come back when the app restarts — the session could not be saved ({e}). Press Stop to revoke every link for certain.")
        })?;
    }
    Ok(())
}

pub fn revoke(name: &str) -> Result<CouchStatus, String> {
    let mut guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    let Some(handle) = guard.as_mut() else {
        return Err("Couch Review is not running".to_string());
    };
    revoke_in(&handle.state, name)?;
    tracing::info!("Couch Review access revoked for one reviewer");
    Ok(status_of(handle))
}

pub fn status() -> CouchStatus {
    let guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    guard.as_ref().map(status_of).unwrap_or_else(CouchStatus::stopped)
}

/// Whether the Couch Review server is currently running. A running server can WRITE to the DB (a phone
/// submit persists a decision/verdict via insert_segment_full on its own connection), so the restore
/// fence must refuse while it is up — cheaper than `status()` (no URL/IP computation). R3.
pub fn is_running() -> bool {
    COUCH.lock().unwrap_or_else(|p| p.into_inner()).is_some()
}

fn status_of(h: &CouchHandle) -> CouchStatus {
    // Resolve the interfaces ONCE for the whole list rather than per reviewer: each `lan_ip()` call
    // opens a socket, and every reviewer's URL differs only in its token.
    let lan = lan_ip();
    let tailscale = tailscale_ip();
    let mut reviewers: Vec<CouchReviewer> = lock_state(&h.state)
        .reviewers
        .iter()
        // `#t=`, not `?t=`: a fragment never leaves the browser, so a link pasted into a chat app
        // hands its preview bot the empty shell rather than a live credential. The page claims the
        // fragment via POST /api/claim into the HttpOnly cookie on first open.
        .map(|(token, name)| CouchReviewer {
            name: name.clone(),
            url: format!("http://{lan}:{}/#t={token}", h.port),
            tailscale_url: tailscale.as_ref().map(|ip| format!("http://{ip}:{}/#t={token}", h.port)),
        })
        .collect();
    // HashMap iteration order is randomized per process, so sort for a stable Settings list — otherwise
    // the URLs visibly reshuffle on every status poll and the owner cannot tell whose link is whose.
    reviewers.sort_by(|a, b| a.name.cmp(&b.name));
    CouchStatus { running: true, reviewers }
}

/// Best-effort LAN IPv4 of this machine. UDP `connect` only SELECTS the outbound interface via the
/// routing table — for UDP no packet is transmitted until a send(), and none is ever sent here, so
/// this stays consistent with the zero-egress posture. Falls back to the hostname (LAN mDNS/NetBIOS
/// usually resolves it) and finally localhost.
fn lan_ip() -> String {
    if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if sock.connect(("8.8.8.8", 80)).is_ok() {
            if let Ok(addr) = sock.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    std::env::var("COMPUTERNAME").map(|h| format!("{h}.local")).unwrap_or_else(|_| "127.0.0.1".into())
}

/// This machine's Tailscale IPv4 (CGNAT 100.64.0.0/10), if a tailnet is up. Same UDP-connect
/// interface-selection trick as `lan_ip` — routing to Tailscale's MagicDNS anycast IP (100.100.100.100)
/// resolves via the tailscale interface when one exists; no packet is ever sent. Verified against the
/// CGNAT range so a bogus route can never masquerade as a tailnet address.
fn tailscale_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect(("100.100.100.100", 80)).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    match ip {
        std::net::IpAddr::V4(v4) if (v4.octets()[0] == 100) && (64..128).contains(&v4.octets()[1]) => {
            Some(v4.to_string())
        }
        _ => None,
    }
}

fn spawn_server_loop(
    index: usize,
    server: Arc<tiny_http::Server>,
    db_path: String,
    state: Arc<Mutex<CouchState>>,
    shutdown: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name(format!("couch-review-{index}"))
        .spawn(move || {
            // This thread's own WAL connection. Per-thread rather than shared so two reviewers' requests
            // never interleave on one rusqlite connection; SQLite serializes the writes between them.
            let db = match Database::open(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    tracing::error!("Couch Review db open failed: {e}");
                    // Never leave the listener bound and mute. A thread that cannot serve must take
                    // the whole server down with it — a port that accepts and never answers hangs
                    // every phone and defeats the watchdog, which cannot tell it from a wedge.
                    shutdown.store(true, Ordering::SeqCst);
                    server.unblock();
                    return;
                }
            };
            while !shutdown.load(Ordering::SeqCst) {
                // recv_timeout, not the blocking iterator: it lets EVERY thread observe `shutdown`
                // within ACCEPT_POLL even if unblock() only wakes one of them, so stop()/join() is
                // bounded no matter how many reviewers are running.
                let mut request = match server.recv_timeout(ACCEPT_POLL) {
                    Ok(Some(request)) => request,
                    // None = timed out (poll again) or unblocked (the shutdown check catches it).
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!("Couch Review accept error: {e}");
                        continue;
                    }
                };
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let (response, set_cookie) = handle_request(&mut request, &db, &state);
                let _ = respond_with_cookie(request, response, set_cookie);
            }
        })
        .map_err(|e| format!("Couch Review server thread failed to spawn: {e}"))
}

/// (status, content_type, body)
type Reply = (u16, &'static str, Vec<u8>);

/// A reply that also plants the session cookie. Used only for the page itself: the token arrives in
/// the URL exactly once, and from then on the cookie carries it.
///
/// `HttpOnly` is the point — the SERVER sets it, so page JavaScript can never read the token back
/// out. Setting it from JS would have been simpler and strictly worse. No `Secure` flag, and that is
/// honest rather than an oversight: this is plain HTTP on a LAN or tailnet, and marking a cookie
/// Secure there would stop it being sent at all. `SameSite=Strict` keeps it off cross-site requests.
fn page_reply_with_cookie(token: &str, body: Vec<u8>) -> (Reply, Option<String>) {
    (
        (200, "text/html; charset=utf-8", body),
        Some(format!("{COUCH_COOKIE}={token}; Path=/; Max-Age=604800; SameSite=Strict; HttpOnly")),
    )
}

/// The fragment bootstrap (docs/REMOTE_PUBLIC_LINKS_PLAN.md phase 2): the page reads `#t=` out of
/// `location.hash` — which the browser never transmits — and presents it here ONCE, in a POST body.
/// The reply moves it into the HttpOnly cookie and the page strips the fragment. From that moment the
/// token never appears in any URL, log line, or preview-bot fetch.
///
/// Deliberately NOT single-use: chat-app preview bots fetch a pasted link within seconds, before the
/// human taps it. A one-shot claim would be burned by the bot and 401 the actual reviewer. The bot
/// never sees the fragment at all, which is the whole protection — repeat claims by the same holder
/// are simply the reviewer opening their link again.
fn api_claim(body: &[u8], state: &Mutex<CouchState>) -> (Reply, Option<String>) {
    #[derive(serde::Deserialize)]
    struct ClaimBody {
        token: String,
    }
    let Ok(parsed) = serde_json::from_slice::<ClaimBody>(body) else {
        return (err_reply(400, "bad json"), None);
    };
    let Some(reviewer) = lock_state(state).reviewers.get(&parsed.token).cloned() else {
        // Same answer as every other bad credential: no hint whether the token was close.
        return (err_reply(401, "unauthorized"), None);
    };
    (
        (200, "application/json", serde_json::json!({ "ok": true, "reviewer": reviewer }).to_string().into_bytes()),
        Some(format!("{COUCH_COOKIE}={}; Path=/; Max-Age=604800; SameSite=Strict; HttpOnly", parsed.token)),
    )
}

fn json_reply(status: u16, value: serde_json::Value) -> Reply {
    (status, "application/json", value.to_string().into_bytes())
}
fn err_reply(status: u16, msg: &str) -> Reply {
    (status, "text/plain; charset=utf-8", msg.as_bytes().to_vec())
}

fn respond_with_cookie(
    request: tiny_http::Request,
    (status, ctype, body): Reply,
    set_cookie: Option<String>,
) -> std::io::Result<()> {
    // `with_chunked_threshold(usize::MAX)` is load-bearing, not a tuning knob. tiny_http defaults to
    // 32 KB: any larger body is sent `Transfer-Encoding: chunked` with NO `Content-Length`. Every clip
    // is 300-500 KB, so every clip crossed it — and a browser given an audio stream of unknown length
    // reports `duration = Infinity` and `seekable.end = Infinity`. Measured on the live server: the
    // phone's progress bar showed no total time and tap-to-seek computed `fraction * Infinity`, so
    // seeking silently did nothing. The body is always a fully-materialised Vec, so its length is
    // ALWAYS known — there is never a reason to chunk it away.
    let mut response = tiny_http::Response::from_data(body).with_status_code(status).with_chunked_threshold(usize::MAX);
    if let Some(cookie) = set_cookie {
        if let Ok(header) = tiny_http::Header::from_bytes(&b"Set-Cookie"[..], cookie.as_bytes()) {
            response = response.with_header(header);
        }
    }
    // The content types here are static ASCII literals, so this never fails in practice — but the
    // panic policy forbids expect(); a (hypothetical) bad header just means the response ships
    // without a Content-Type rather than killing the server thread.
    if let Ok(header) = tiny_http::Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()) {
        response = response.with_header(header);
    }
    request.respond(response)
}

/// The cookie the server sets so the token stops travelling in the URL (P3.8).
const COUCH_COOKIE: &str = "cortex_couch";

/// Resolve the session token from a request: the `?t=` query FIRST (how a shared link always
/// arrives), then the `Cookie` header (how every request after the first one carries it).
///
/// Why bother: a token in the query string lands in browser history, in the phone's address bar for
/// anyone glancing over, and in the logs of anything that ever proxies the request. The page strips
/// `?t=` from the visible URL after the first load, and the cookie carries it from then on —
/// including for `<audio src>`, which cannot send a custom header but does send cookies.
fn token_from_request(request: &tiny_http::Request) -> Option<String> {
    if let Some(t) = token_from_url(request.url()) {
        return Some(t.to_string());
    }
    let cookies = request.headers().iter().find(|h| h.field.equiv("Cookie"))?;
    cookies.value.as_str().split(';').find_map(|kv| {
        let (name, value) = kv.split_once('=')?;
        (name.trim() == COUCH_COOKIE).then(|| value.trim().to_string())
    })
}

/// Extract `t` from a raw request URL's query string.
///
/// An EMPTY `t=` counts as ABSENT, not as a token that happens to be the empty string. That
/// distinction is the whole reason a returning reviewer can get back in: the page strips `?t=` from
/// the URL after the first load (P3.8), so on a return visit its `token` variable is `""` and every
/// request goes out as `?t=`. Treating that as a supplied-but-wrong token made the query shadow the
/// perfectly good session cookie sitting in the same request, so the server answered 401 while
/// holding the credential that would have let them in — the cookie mechanism could never actually be
/// used for the one job it was added to do.
///
/// A NON-empty wrong token still fails, and must: someone presenting an explicit credential is
/// making a claim, and silently falling back to a cookie there would let a revoked link keep working.
fn token_from_url(url: &str) -> Option<&str> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("t=")).filter(|t| !t.is_empty())
}

fn handle_request(
    request: &mut tiny_http::Request,
    db: &Database,
    state: &Mutex<CouchState>,
) -> (Reply, Option<String>) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = request.method().clone();

    // EVERY DATA route is token-gated. Two routes are deliberately reachable without a credential,
    // and the distinction is the Phase-2 design (docs/REMOTE_PUBLIC_LINKS_PLAN.md):
    //
    //   GET /            — the static shell. It embeds no clip data, no reviewer names, no counts;
    //                      everything meaningful lives behind /api/*. Serving it openly is what lets
    //                      the token move into the URL FRAGMENT (#t=), which the browser never sends
    //                      to any server — so a link pasted into WhatsApp/Telegram gives their
    //                      link-preview bots the empty shell instead of a live credential to
    //                      biometric audio. "Every route gated" was the right posture when the token
    //                      rode the query string; once it must not, gating the shell protects
    //                      nothing and forces the token back onto the wire.
    //   POST /api/claim  — the fragment bootstrap: the page presents the token ONCE in a request
    //                      body, and the reply moves it into the HttpOnly cookie.
    //
    // The token still RESOLVES the reviewer everywhere else: an unknown token has no identity, so
    // there is no path on which a decision can be written without a name attached. Resolved from the
    // SHARED map on every request, so a revoked token stops working immediately.
    let authenticated = token_from_request(request)
        .and_then(|token| lock_state(state).reviewers.get(&token).cloned().map(|name| (token, name)));

    if let (tiny_http::Method::Get, "/") = (&method, path.as_str()) {
        let shell = include_str!("../assets/couch.html").as_bytes().to_vec();
        return match authenticated {
            // Authenticated page load: RE-plant the cookie. This is the sliding expiry — an active
            // reviewer's cookie renews on every visit, so a bookmark used weekly never dies while
            // the session lives. (A fixed Max-Age from first claim would expire mid-engagement.)
            Some((token, _)) => page_reply_with_cookie(&token, shell),
            // Unauthenticated: the bare shell, NO cookie. This is what a preview bot receives.
            None => ((200, "text/html; charset=utf-8", shell), None),
        };
    }
    // Static assets for install-to-home-screen (phase 6), public for the same reason the shell is:
    // an icon and a manifest carry nothing. The manifest's start_url is "/" and NEVER a token — an
    // installed app must ride the cookie, or a revoked token would be frozen into someone's phone.
    if let (tiny_http::Method::Get, "/icon.png") = (&method, path.as_str()) {
        return ((200, "image/png", include_bytes!("../assets/couch-icon.png").to_vec()), None);
    }
    if let (tiny_http::Method::Get, "/manifest.json") = (&method, path.as_str()) {
        let manifest = serde_json::json!({
            "name": "Cortex Review",
            "short_name": "Cortex",
            "start_url": "/",
            "display": "standalone",
            "background_color": "#0b1220",
            "theme_color": "#0b1220",
            "icons": [{ "src": "/icon.png", "sizes": "512x512", "type": "image/png" }]
        });
        return ((200, "application/manifest+json", manifest.to_string().into_bytes()), None);
    }
    if let (tiny_http::Method::Post, "/api/claim") = (&method, path.as_str()) {
        return match read_body(request) {
            Ok(body) => api_claim(&body, state),
            Err(e) => (err_reply(400, &e), None),
        };
    }

    let Some((token, reviewer)) = authenticated else {
        return (err_reply(401, "unauthorized"), None);
    };
    let _ = token;
    let reviewer = reviewer.as_str();
    if let Err(e) = COUCH_RATE_LIMITER.check(reviewer) {
        return ((429, "text/plain; charset=utf-8", e.into_bytes()), None);
    }

    let reply = match (method, path.as_str()) {
        (tiny_http::Method::Get, "/api/queue") => api_queue(db, reviewer, state),
        (tiny_http::Method::Get, p) if p.starts_with("/api/audio/") => {
            api_audio(db, p.trim_start_matches("/api/audio/"))
        }
        (tiny_http::Method::Post, "/api/decision") => match read_body(request) {
            Ok(body) => api_decision(db, &body, reviewer, state),
            Err(e) => err_reply(400, &e),
        },
        (tiny_http::Method::Post, "/api/renew") => match read_body(request) {
            Ok(body) => api_renew(&body, reviewer, state),
            Err(e) => err_reply(400, &e),
        },
        (tiny_http::Method::Post, "/api/undo") => api_undo(db, reviewer, state),
        _ => err_reply(404, "not found"),
    };
    (reply, None)
}

/// Lock the shared state, recovering from a poisoned mutex rather than killing the accept thread. The
/// guarded data is two plain maps with no cross-field invariant, so a panic mid-update cannot leave them
/// inconsistent — and refusing to serve every later request would be a far worse outcome than proceeding.
fn lock_state(state: &Mutex<CouchState>) -> std::sync::MutexGuard<'_, CouchState> {
    state.lock().unwrap_or_else(|p| p.into_inner())
}

fn read_body(request: &mut tiny_http::Request) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    request
        .as_reader()
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("body read: {e}"))?;
    if body.len() > MAX_BODY_BYTES {
        return Err("body too large".to_string());
    }
    Ok(body)
}

/// The review text shown/edited on the phone: same precedence as the desktop editor
/// (human-curated annotated transcript when present, else the raw draft).
fn review_text(seg: &SpeechSegment) -> String {
    seg.annotated_transcript.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| seg.raw_transcript.clone())
}

/// Pending (unverified) clips, oldest first — the same "work that needs doing" the desktop queue leads
/// with — MINUS anything another reviewer currently holds, and leased to this reviewer on the way out.
///
/// The lease is what makes two phones safe: without it both are handed the same head-of-queue clips and
/// race to decide them (duplicated effort at best, one reviewer's verdict silently overwriting the
/// other's at worst). Batches are small so the first reviewer to load cannot lease the entire backlog,
/// and leases expire so a closed browser tab never strands work.
fn api_queue(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    let segments = match db.get_segments(Some(false)) {
        Ok(s) => s,
        Err(e) => return err_reply(500, &e.to_string()),
    };
    let now = Instant::now();
    let mut guard = lock_state(state);
    let mut queue: Vec<serde_json::Value> = Vec::new();
    let mut held_by_others = 0usize;
    for s in segments.iter() {
        // Someone else's live lease: skip it, but COUNT it. Our own is renewed below (a reviewer
        // reloading the page must get their own in-progress work back, not a fresh batch that
        // abandons it).
        if guard.holder(&s.id, now).is_some_and(|who| who != reviewer) {
            held_by_others += 1;
            continue;
        }
        if queue.len() >= QUEUE_BATCH {
            continue; // keep counting what is left, but hand out no more this round
        }
        guard.leases.insert(s.id.clone(), (reviewer.to_string(), now));
        queue.push(serde_json::json!({
            "id": s.id,
            "text": review_text(s),
            "durationMs": s.duration_ms,
            "speakerId": s.speaker_id,
        }));
    }
    // Drop leases that expired while their holder was away, so the map cannot grow without bound across
    // a long session. Cheap: the pending queue is the only thing that can be leased.
    guard.leases.retain(|_, (_, granted)| now.duration_since(*granted) < LEASE_TTL);
    drop(guard);

    // Salt the batch with spot checks (P2.1). They are NOT leased: two reviewers meeting the same
    // known-answer clip is the point — independent measurement — not a collision. They also carry no
    // marker of any kind in the payload, because a reviewer who can spot the test is not being tested.
    if !queue.is_empty() {
        // CEILING, not floor. `len / EVERY` silently gives ZERO checks to any batch under EVERY
        // items — so a reviewer arriving late to a nearly-drained queue, or sharing a small backlog,
        // would never be measured at all. Rounding up guarantees at least one check in every non-empty
        // batch while holding the ~1-in-8 ratio wherever the batch is large enough for it to mean
        // something. A reviewer must not be able to coast just because their batches came out short.
        let wanted = queue.len().div_ceil(SPOT_CHECK_EVERY);
        match db.list_spot_check_candidates(wanted, reviewer) {
            Ok(candidates) => {
                let mut guard = lock_state(state);
                let mut grew = false;
                for (idx, (seg, _)) in candidates.into_iter().enumerate() {
                    grew |= guard.spot_checks.insert((seg.id.clone(), reviewer.to_string()));
                    // Interleave rather than append: a run of traps at the tail of every batch is a
                    // pattern a reviewer would learn within a session.
                    let at = ((idx + 1) * SPOT_CHECK_EVERY).min(queue.len());
                    queue.insert(
                        at,
                        serde_json::json!({
                            "id": seg.id,
                            // The RAW draft — the known-wrong one. Serving the corrected text would
                            // make the check unpassable-by-failing: there would be nothing to catch.
                            "text": seg.raw_transcript,
                            "durationMs": seg.duration_ms,
                            "speakerId": seg.speaker_id,
                        }),
                    );
                }
                // Persist the grown served-set into the remembered session (phase 4): a check served
                // now may be answered AFTER an app restart, and it must still be scored then rather
                // than 409ing the reviewer. Snapshot under the lock, write outside it — save_session
                // does DPAPI + file IO, which has no business inside the request-path mutex.
                let persist = if grew {
                    guard.session_store.clone().map(|s| (s, guard.reviewers.clone(), guard.spot_checks.clone()))
                } else {
                    None
                };
                drop(guard);
                if let Some(((dir, db_path), reviewers, checks)) = persist {
                    let _ = save_session(&dir, &reviewers, &db_path, &checks);
                }
            }
            // A spot check is a quality measure, never a reason to stop a reviewer working.
            Err(e) => tracing::warn!("Couch Review spot-check selection failed: {e}"),
        }
    }
    // Two things travel with the queue, and both exist to stop the page from misleading its reviewer:
    //
    //   `reviewer` — WHO the server is recording these decisions as. Attribution a reviewer cannot see
    //   is attribution they cannot correct.
    //
    //   `heldByOthers` — how much pending work is currently leased elsewhere. Whoever loads first can
    //   take up to QUEUE_BATCH clips (ponytail: a fixed batch, not a fair-share scheduler — leases
    //   expire and batches drain, so the imbalance is self-correcting). On a backlog smaller than one
    //   batch that leaves a second reviewer with an EMPTY queue, and "🎉 Queue reviewed" would then be a
    //   flat lie. This count lets the page say "someone else has them" instead.
    json_reply(200, serde_json::json!({ "reviewer": reviewer, "items": queue, "heldByOthers": held_by_others }))
}

fn api_audio(db: &Database, raw_id: &str) -> Reply {
    let id = raw_id.split('?').next().unwrap_or(raw_id);
    if crate::validation::input::validate_identifier(id).is_err() {
        return err_reply(400, "bad id");
    }
    let seg = match db.get_segment_by_id(id) {
        Ok(Some(seg)) => seg,
        Ok(None) => return err_reply(404, "no such segment"),
        Err(e) => return err_reply(500, &e.to_string()),
    };
    match crate::agentic::segment_audio_as_wav_bytes(&seg) {
        Ok(bytes) => (200, "audio/wav", bytes),
        Err(e) => err_reply(500, &e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct RenewBody {
    id: String,
}

/// Extend this reviewer's hold on the clip they still have open (P1.3).
///
/// A 15-minute lease can genuinely expire mid-clip on a hard piece of audio. Without renewal the
/// reviewer discovers it only at save time, as a 409, with their correction already typed and now
/// unsaveable — the exact work-destroying moment leases exist to prevent. The page heartbeats while a
/// clip is open, so an ACTIVE reviewer keeps their clip indefinitely while an idle one still releases
/// it on schedule.
///
/// Reclaiming an unheld clip is allowed and deliberate: if the lease lapsed but nobody else took it,
/// the reviewer who still has it open should keep it.
fn api_renew(body: &[u8], reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    let parsed: RenewBody = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => return err_reply(400, &format!("bad json: {e}")),
    };
    if crate::validation::input::validate_identifier(&parsed.id).is_err() {
        return err_reply(400, "bad id");
    }
    let now = Instant::now();
    let mut guard = lock_state(state);
    if guard.holder(&parsed.id, now).is_some_and(|who| who != reviewer) {
        // Someone else took it while this reviewer was away. Telling them NOW — rather than at save —
        // is the whole point: they can still copy their correction before it is refused.
        return err_reply(409, "another reviewer is working on this clip");
    }
    // Refresh the reviewer's WHOLE batch, not just the clip on screen.
    //
    // api_queue leases up to QUEUE_BATCH clips in one shot, every one stamped with the same instant,
    // so they all expire together — while the page only ever heartbeats `queue[i]`. That gave the
    // reviewer LEASE_TTL to finish the entire batch (36 seconds per clip at 25), and real
    // listen-and-correct on Sorani audio is nowhere near that. Everything they had not yet reached
    // silently fell out from under them, another reviewer's next fetch picked it up, and the first
    // reviewer's eventual save was refused 409 with their correction already typed. A renew request
    // is proof this person is working; their claim on the rest of their batch is exactly as live as
    // their claim on the clip in front of them.
    for (_, (who, granted)) in guard.leases.iter_mut() {
        if who == reviewer {
            *granted = now;
        }
    }
    guard.leases.insert(parsed.id.clone(), (reviewer.to_string(), now));
    json_reply(200, serde_json::json!({ "ok": true, "ttlSeconds": LEASE_TTL.as_secs() }))
}

/// Whether this reviewer's decision is ALREADY STORED on the row (P1.2).
///
/// A phone on the edge of Wi-Fi drops requests. If the write lands but the response is lost, the page
/// retries — and without this check the retry is recorded as a SECOND human decision: another undo
/// entry, and for an edit another DPO learning pair distilled from a correction the human made once.
/// Treating an identical re-submit as already-done makes retry safe, which is what lets the page
/// retry at all.
///
/// Deliberately narrow. It matches only when the SAME reviewer's stored decision has the SAME
/// outcome; change one character and it is a genuine re-review that must be recorded. Text is
/// NFC-compared because the write path canonicalizes, so a decomposed phone-IME paste would otherwise
/// never match the value it just stored.
///
/// This deliberately does NOT look at `verified`, and that is the whole point. One phone decision is
/// TWO non-atomic writes: `record_human_decision_by` (which sets human_decision / verdict_transcript /
/// reviewed_by) and then the row upsert (which sets verified / annotated_transcript). Keying the guard
/// on `verified` meant a mid-submit failure — write one committed, write two refused — left a row the
/// guard could not recognise, so the outbox replay ran the ENTIRE write a second time: a duplicate
/// agent_examples pair into the DPO export, a correction_memory hit_count bumped as though it were an
/// independent cross-segment confirmation, and that memory then graded against the very edit that
/// created it. `verified` is an artefact of the second write; the identity of the decision lives in
/// the first. Callers pair this with `prev.verified` to tell a COMPLETE repeat from a half-written one.
fn is_repeat_of_stored_decision(prev: &SpeechSegment, reviewer: &str, decision: &str, text: Option<&str>) -> bool {
    if prev.reviewed_by.as_deref() != Some(reviewer) {
        return false;
    }
    match (decision, text) {
        ("reject", _) => prev.human_decision.as_deref() == Some("reject"),
        // A repeat of an accept can arrive classified as either: the first submit may have been an
        // "edit", after which the stored text IS the review text, so the retry re-classifies as
        // "accept". Both are the same human act.
        //
        // Matched against EITHER column holding the human's text. After a complete submit that is
        // `annotated_transcript`; after a half-written one only `verdict_transcript` exists, because
        // the upsert that would have copied it across is exactly the write that failed.
        (_, Some(t)) => {
            let want = crate::db::to_nfc(t);
            matches!(prev.human_decision.as_deref(), Some("accept") | Some("edit"))
                && [prev.annotated_transcript.as_deref(), prev.verdict_transcript.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|stored| crate::db::to_nfc(stored) == want)
        }
        _ => false,
    }
}

#[derive(serde::Deserialize)]
struct DecisionBody {
    id: String,
    /// "accept" | "edit" | "bad"
    action: String,
    #[serde(default)]
    text: String,
    /// Who the PAGE believed it was when this decision was made.
    ///
    /// Only ever set on a replay out of the phone's outbox, and checked against the cookie identity
    /// rather than trusted. The outbox lives in localStorage, which is per-ORIGIN, not per-reviewer:
    /// two reviewers using the same browser (or one person handed a colleague's link) share it, so a
    /// decision queued while offline by Sara could be flushed under Hemn's cookie and be recorded,
    /// permanently, as Hemn's judgement of a clip he never heard. Attribution is the one thing a
    /// review corpus cannot be wrong about.
    #[serde(default)]
    reviewer: Option<String>,
}

/// Record one phone decision through the exact desktop path: `record_human_decision_by` FIRST (its
/// validation aborts before anything is committed), then the whole-row upsert built from the FRESH
/// db row — never from the payload — so only `annotated_transcript`/`verified` can change. The decision
/// is attributed to `reviewer`, and refused outright on a clip another reviewer currently holds.
fn api_decision(db: &Database, body: &[u8], reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    let parsed: DecisionBody = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => return err_reply(400, &format!("bad json: {e}")),
    };
    if crate::validation::input::validate_identifier(&parsed.id).is_err() {
        return err_reply(400, "bad id");
    }
    if crate::validation::input::validate_text(&parsed.text, 100_000, "Transcript").is_err() {
        return err_reply(400, "text too large");
    }
    // ATTRIBUTION FENCE. A queued decision names its author; the cookie names who is asking now. When
    // they disagree the decision belongs to somebody else and must not be written under this name —
    // refuse it and let the page hold it for the reviewer who actually made it. Absent field = a live
    // submit from a page that has no reason to claim anything, which is the ordinary path.
    if let Some(claimed) = parsed.reviewer.as_deref() {
        if !claimed.eq_ignore_ascii_case(reviewer) {
            return err_reply(409, &format!("this decision was made by {claimed}, not {reviewer}"));
        }
    }
    let prev = match db.get_segment_by_id(&parsed.id) {
        Ok(Some(seg)) => seg,
        Ok(None) => return err_reply(404, "no such segment"),
        Err(e) => return err_reply(500, &e.to_string()),
    };

    let (decision, text): (&str, Option<String>) = match parsed.action.as_str() {
        "accept" | "edit" => {
            let text = parsed.text.trim().to_string();
            if text.is_empty() {
                return err_reply(400, "empty transcript");
            }
            // Same guard as the desktop editor: a placeholder draft ("[Pending WSL 7B ASR]" /
            // "[ASR unavailable…]") must never be verified as gold.
            if text.starts_with('[') && text.ends_with(']') {
                return err_reply(400, "placeholder transcript cannot be verified");
            }
            let is_edit = text != review_text(&prev).trim();
            (if is_edit { "edit" } else { "accept" }, Some(text))
        }
        "bad" => ("reject", None),
        other => return err_reply(400, &format!("unknown action '{other}'")),
    };

    // SPOT CHECK (P2.1): this clip already has a human answer, so the reviewer was being measured, not
    // asked to do new work. Record the score and stop — the corpus row is left completely untouched,
    // because a mechanism that grades reviewers must never be able to alter the data it grades against.
    //
    // The reply is byte-identical to a normal success. A reviewer who could tell a test from real work
    // would simply be careful on the tests, which measures nothing.
    // A spot check is BY DEFINITION a clip that already carries a human answer to grade against, and
    // `prev.verified` is what makes it one. The served-set is never pruned and now SURVIVES RESTARTS,
    // so a pair could outlive the answer key: the owner un-verifies the clip at the desktop (or
    // re-transcribes it, which also clears `verified`), api_queue then serves it as ordinary pending
    // work — nothing filters served checks out of the work queue — and the reviewer transcribes it for
    // real. Grading that against a key that no longer exists recorded a bogus score, returned a reply
    // deliberately indistinguishable from success, and wrote NOTHING to the corpus. The clip stayed
    // unverified, came back in the next batch, and was swallowed again every single time.
    let was_served_as_check = {
        let mut guard = lock_state(state);
        let key = (parsed.id.clone(), reviewer.to_string());
        if guard.spot_checks.contains(&key) && !prev.verified {
            // No longer an answer key. Drop the stale pair so it cannot swallow this clip again, and
            // treat the submit as what it is: real work.
            guard.spot_checks.remove(&key);
            tracing::info!("Couch Review: stale spot check for {} dropped — the clip is no longer verified", parsed.id);
            false
        } else {
            guard.spot_checks.contains(&key)
        }
    };
    // AUDIT TRAIL (v45), written for every accepted submit including spot checks — a reviewer's
    // throughput must count the work they actually did, and a check is real work to them. Best-effort
    // and logged on failure: losing the RECORD of a decision must never cost the DECISION.
    let audit = |db: &Database, action: &str| {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if let Err(e) = db.record_review_event(&parsed.id, reviewer, action, "couch", now_ms) {
            tracing::warn!("Couch Review audit event not recorded for {}: {e}", parsed.id);
        }
    };
    if was_served_as_check {
        let expected = crate::quality::human_verified_text(&prev).unwrap_or_default().to_string();
        let submitted = text.as_deref().unwrap_or_default();
        if let Err(e) = db.record_spot_check(&parsed.id, reviewer, decision, submitted, &expected) {
            // Losing a score must not cost the reviewer their place in the queue: answer normally and
            // let the missing row show up as a smaller `checks` count rather than as a failed save.
            tracing::warn!("Couch Review spot-check not recorded for {}: {e}", parsed.id);
        }
        audit(db, decision);
        return json_reply(200, serde_json::json!({ "ok": true }));
    }

    // A dropped RESPONSE (not a dropped request) means the write landed and the page never heard so.
    // Answer the retry with the success it already earned, before taking a lease or pushing an undo
    // entry for a decision that is already recorded.
    //
    // `already_recorded` without `verified` is the HALF-WRITTEN state: write one committed and the row
    // upsert did not. That is not a duplicate to be waved through — the clip is still unverified, so it
    // would be served as pending work forever — and it is not new work either, because replaying write
    // one would double the learning pair. It is an interrupted write to be FINISHED, below.
    let already_recorded = is_repeat_of_stored_decision(&prev, reviewer, decision, text.as_deref());
    if already_recorded && prev.verified {
        return json_reply(200, serde_json::json!({ "ok": true, "duplicate": true }));
    }

    // THE LATE-SUBMIT GUARD. The lease is RELEASED the moment a clip is decided — correctly, since it
    // is no longer pending work — which leaves an already-decided clip unprotected by the collision
    // check below. A stale page could then submit onto it minutes later and silently replace another
    // human's verdict, `reviewed_by` and all: precisely the destruction leases exist to prevent, just
    // arriving late instead of concurrently. A decided clip never legitimately reaches a phone queue
    // (the queue serves unverified rows only), so refusing costs nothing real.
    //
    // The SAME reviewer is exempt: correcting your own decision is a genuine re-review, and the
    // retry case above has already been answered.
    if prev.verified && prev.reviewed_by.as_deref() != Some(reviewer) {
        return match prev.reviewed_by.as_deref() {
            Some(other) => err_reply(409, &format!("already reviewed by {other}")),
            None => err_reply(409, "already reviewed at the desktop"),
        };
    }

    // THE COLLISION GUARD, placed here — after every validation, immediately before the write.
    //
    // Check and claim happen under ONE lock so they are atomic. Refusing is the honest outcome: the two
    // reviewers judged the clip independently, and silently keeping whichever submit landed second would
    // destroy the other's verdict with nobody aware. Claiming (not just checking) matters because a
    // submit can arrive for an UNLEASED clip — a page kept open across a restart — and a bare
    // check-then-write would let two such submits both pass and both write.
    //
    // It sits after validation so a REJECTED request (bad action, placeholder text, missing row) cannot
    // leave a 15-minute lease on a clip it never decided, locking other reviewers out of it.
    {
        let now = Instant::now();
        let mut guard = lock_state(state);
        if guard.holder(&parsed.id, now).is_some_and(|who| who != reviewer) {
            return err_reply(409, "another reviewer is working on this clip");
        }
        guard.leases.insert(parsed.id.clone(), (reviewer.to_string(), now));
        // No undo entry when finishing a half-written decision: `prev` is the HALF-DECIDED row, not the
        // pre-decision one, so storing it would make the undo button restore a corrupt snapshot. The
        // entry pushed by the original attempt is the true one and is deliberately left in place.
        if !already_recorded {
            let stack = guard.undo.entry(reviewer.to_string()).or_default();
            stack.push((prev.id.clone(), prev.clone()));
            // Bounded HERE, at the only site that grows it: the pushes in api_undo are restorations of
            // an entry just popped, so they cannot exceed what the stack already held.
            let overflow = stack.len().saturating_sub(UNDO_DEPTH);
            if overflow > 0 {
                stack.drain(0..overflow);
            }
        }
    }
    // Skipped entirely when the decision is already on the row: this is the write that mints the DPO
    // pair and the correction memories, and running it twice for one human act is the corruption the
    // half-written state used to cause.
    if !already_recorded {
        if let Err(e) =
            crate::jury::record_human_decision_by(db, &parsed.id, decision, text.as_deref(), None, Some(reviewer))
        {
            let mut guard = lock_state(state);
            guard.undo.entry(reviewer.to_string()).or_default().pop();
            // The lease is deliberately KEPT. Handing the clip back the instant a write fails looks
            // tidy, but the reviewer still has this clip open with their correction typed and now
            // queued in the outbox — and a freed clip is picked up by the next reviewer's batch within
            // seconds. Their replay then loses the race and is refused 409, destroying work that was
            // never in conflict with anyone. An untouched lease expires on its own in LEASE_TTL, which
            // is the right cost for a transient database error.
            return err_reply(500, &e.to_string());
        }
    }
    // Fresh row AFTER the (potentially slow) decision call — the same stale-spread rationale as the
    // desktop submit(); a background writer may have updated other columns in the meantime.
    let mut updated = match db.get_segment_by_id(&parsed.id) {
        Ok(Some(seg)) => seg,
        Ok(None) => return err_reply(404, "segment vanished"),
        Err(e) => return err_reply(500, &e.to_string()),
    };
    if let Some(t) = text {
        updated.annotated_transcript = Some(t);
    }
    updated.verified = true; // "reviewed" — a 'reject' decision keeps it out of exports
    if let Err(e) = db.insert_segment(&updated) {
        return err_reply(500, &e.to_string());
    }
    // Decided, so the lease has served its purpose — release it rather than waiting out the TTL. (The
    // clip also leaves the pending queue, so this is housekeeping, not correctness.)
    lock_state(state).leases.remove(&parsed.id);
    audit(db, decision);
    json_reply(200, serde_json::json!({ "ok": true }))
}

/// Undo the LAST phone decision: retract the learning pair the decision produced, then restore the
/// pre-decision row LOSSLESSLY. Returns the undone segment id.
///
/// `clear_human_decision` is kept ONLY for its side effect of deleting the DPO/few-shot agent_examples
/// row the edit produced (a retracted edit left in the trainable pool permanently teaches the model a
/// fix the human took back). The row-restore itself MUST go through `insert_segment_full`, not
/// `insert_segment`: `prev` is the exact pre-decision snapshot, and a clip served to the couch queue can
/// carry jury state (a jury-ESCALATED clip is unverified, so it is in `get_segments(false)`) — verdict,
/// verdict_transcript, rationale, evidence_json, agent_confidence, escalated, is_gold. `insert_segment`
/// writes a 17-column subset that OMITS every one of those, so it would leave them at whatever
/// `clear_human_decision` set (jury verdict/evidence NULLed, is_gold from the undone accept still set) —
/// silently dropping the pre-decision jury verdict on undo. `insert_segment_full` rewrites the whole row
/// (including created_at) back to `prev`, so undo is a true inverse.
/// Undo pops from THIS reviewer's stack only. A single shared stack meant one reviewer's ↩ retracted
/// whichever decision happened to be last — usually someone else's, with no indication to either of them.
fn api_undo(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    let popped = lock_state(state).undo.get_mut(reviewer).and_then(|stack| stack.pop());
    let Some((id, prev)) = popped else {
        return err_reply(409, "nothing to undo");
    };
    // STALENESS FENCE. `prev` is a whole-row snapshot taken at decision time and never refreshed, and
    // the restore below rewrites EVERY column from it. If anyone touched the clip in between — the
    // owner correcting it at the desktop, most obviously, since a decided clip is visible and editable
    // there — undo silently destroys that work and reverts the row to a state the other person never
    // agreed to. api_decision guards this exact class by re-reading the fresh row before it writes;
    // the undo path simply did not. Refuse instead, and keep the entry so nothing is lost either way.
    match db.get_segment_by_id(&id) {
        Ok(Some(fresh)) if fresh.reviewed_by.as_deref() != Some(reviewer) => {
            let owner = fresh.reviewed_by.clone();
            lock_state(state).undo.entry(reviewer.to_string()).or_default().push((id, prev));
            return match owner {
                Some(other) => {
                    err_reply(409, &format!("{other} has reviewed this clip since — undo would erase their work"))
                }
                None => {
                    err_reply(409, "this clip has been changed at the desktop since — undo would erase that change")
                }
            };
        }
        Ok(Some(_)) => {}
        Ok(None) => {
            lock_state(state).undo.entry(reviewer.to_string()).or_default().push((id, prev));
            return err_reply(404, "no such segment");
        }
        Err(e) => {
            lock_state(state).undo.entry(reviewer.to_string()).or_default().push((id, prev));
            return err_reply(500, &e.to_string());
        }
    }
    if let Err(e) = db.clear_human_decision(&id) {
        lock_state(state).undo.entry(reviewer.to_string()).or_default().push((id, prev));
        return err_reply(500, &e.to_string());
    }
    if let Err(e) = db.insert_segment_full(&prev) {
        // The snapshot is the ONLY copy of the pre-decision row and clear_human_decision has already
        // committed, so dropping it here left the clip permanently half-undone: unverified columns
        // cleared, the retracted edit still sitting in annotated_transcript, and nothing anywhere able
        // to put it back. Push it back so a retry — or the next reviewer's ↩ — can still complete it.
        lock_state(state).undo.entry(reviewer.to_string()).or_default().push((id, prev));
        return err_reply(500, &e.to_string());
    }
    // The clip is pending again and this reviewer is about to see it — hold it for them so a second
    // reviewer's queue cannot grab the clip out from under the person mid-correction.
    lock_state(state).leases.insert(id.clone(), (reviewer.to_string(), Instant::now()));
    json_reply(200, serde_json::json!({ "id": id }))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that drive the real `start`/`stop` path share the ONE global `COUCH` singleton, and
    /// cargo runs tests in parallel — two such tests steal each other's server mid-assertion.
    /// Reproduced 3-for-3 the moment a second start/stop test existed; each passes alone. Any test
    /// touching the global handle must hold this first. `unwrap_or_else(into_inner)`: a poisoned
    /// lock from one failed test must not cascade every later one into a meaningless panic.
    static GLOBAL_SESSION_LOCK: Mutex<()> = Mutex::new(());

    fn test_db(dir: &std::path::Path) -> (Database, String) {
        let path = dir.join("couch-test.db").to_string_lossy().to_string();
        let db = Database::open(&path).unwrap();
        db.initialize().unwrap();
        (db, path)
    }

    fn seg(id: &str, raw: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.into(),
            audio_path: "/audio/a.wav".into(),
            raw_transcript: raw.into(),
            duration_ms: 1500,
            ..SpeechSegment::default()
        }
    }

    #[test]
    fn tailscale_ip_is_none_or_a_cgnat_address() {
        // Env-dependent by nature (a tailnet may or may not be up on the test machine), but the
        // contract is deterministic: the helper returns None, or an address inside 100.64.0.0/10 —
        // never a LAN/public IP masquerading as a tailnet URL.
        if let Some(ip) = tailscale_ip() {
            let first: u8 = ip.split('.').next().unwrap().parse().unwrap();
            let second: u8 = ip.split('.').nth(1).unwrap().parse().unwrap();
            assert_eq!(first, 100, "{ip} not in CGNAT range");
            assert!((64..128).contains(&second), "{ip} not in CGNAT range");
        }
    }

    #[test]
    fn token_from_url_parses_only_the_t_param() {
        assert_eq!(token_from_url("/?t=abc"), Some("abc"));
        assert_eq!(token_from_url("/api/queue?x=1&t=abc"), Some("abc"));
        assert_eq!(token_from_url("/api/queue"), None);
        assert_eq!(token_from_url("/api/queue?token=abc"), None);
        // AN EMPTY `t=` IS ABSENT, NOT A TOKEN. This is what locked a returning reviewer out: the
        // page strips `?t=` from the URL after the first load, so on a return visit its token is ""
        // and every request goes out as `?t=`. Read as a supplied-but-wrong credential, the query
        // shadowed the valid session cookie in the SAME request and the server answered 401 while
        // holding what would have let them in. Measured on the wire before the fix:
        //   cookie, no t=     -> 200
        //   cookie + empty t= -> 401
        assert_eq!(token_from_url("/api/queue?t="), None, "an empty t= must fall through to the cookie");
        assert_eq!(token_from_url("/api/queue?t=&x=1"), None);
        // ...but a non-empty WRONG token must still be honoured as a claim and fail. Falling back to
        // the cookie there would let a revoked link keep working by simply carrying a stale cookie.
        assert_eq!(token_from_url("/api/queue?t=wrong"), Some("wrong"));
    }

    #[test]
    fn review_text_prefers_annotated_over_raw() {
        let mut s = seg("s1", "raw");
        assert_eq!(review_text(&s), "raw");
        s.annotated_transcript = Some("  ".into());
        assert_eq!(review_text(&s), "raw", "blank annotated falls back to raw");
        s.annotated_transcript = Some("curated".into());
        assert_eq!(review_text(&s), "curated");
    }

    /// Fresh shared state for a test server session.
    fn state() -> Mutex<CouchState> {
        Mutex::new(CouchState::default())
    }

    /// The ids `reviewer` is handed by a queue request (and, as a side effect, leases).
    fn queue_ids(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Vec<String> {
        let (code, _, body) = api_queue(db, reviewer, state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["reviewer"], reviewer, "the queue names the reviewer it was served to");
        payload["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn decision_accept_edit_bad_and_undo_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        db.insert_segment(&seg("s2", "دەق دوو")).unwrap();
        let state = state();

        // EDIT: corrected text becomes the annotated gold; row is verified; decision recorded.
        let body = serde_json::json!({"id": "s1", "action": "edit", "text": "دەق یەک ڕاستکراوە"});
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(row.verified);
        assert_eq!(row.annotated_transcript.as_deref(), Some("دەق یەک ڕاستکراوە"));
        assert_eq!(row.reviewed_by.as_deref(), Some("Sara"), "a phone decision is attributed to its reviewer");

        // BAD: reject decision, still marked reviewed.
        let body = serde_json::json!({"id": "s2", "action": "bad"});
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);
        assert!(db.get_segment_by_id("s2").unwrap().unwrap().verified);

        // Queue now empty (both reviewed).
        assert!(queue_ids(&db, "Sara", &state).is_empty(), "both clips reviewed -> empty queue");

        // UNDO restores the LAST decision (s2): unverified again, decision cleared.
        let (code, _, body) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        let undone: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(undone["id"], "s2");
        let row = db.get_segment_by_id("s2").unwrap().unwrap();
        assert!(!row.verified, "undo must reopen the clip");
        assert_eq!(row.reviewed_by, None, "undo retracts the attribution with the decision");

        // Second undo pops s1; third has nothing left.
        let (code, _, _) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        let (code, _, _) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "empty undo stack is a 409, not a phantom undo");
    }

    #[test]
    fn an_interrupted_decision_is_finished_on_replay_and_never_recorded_twice() {
        // One phone decision is TWO non-atomic writes: record_human_decision_by, then the row upsert.
        // A failure between them (SQLITE_BUSY against a desktop batch, SQLITE_FULL) used to be
        // unrecoverable in two separate ways at once, and this pins both.
        //
        // Made deterministic with the validation asymmetry the failure exploits: write one is a plain
        // UPDATE that never calls validate_segment, write two goes through insert_segment which does —
        // and which rejects a UNC audio_path. Flipping that column forces exactly the half-written row
        // a transient fault produces, with no sleeps and no I/O races.
        let tmp = tempfile::tempdir().unwrap();
        let (db, path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        let state = state();
        let fix = "دەق یەک ڕاستکراوە";
        let body = serde_json::json!({"id": "s1", "action": "edit", "text": fix});

        let side = rusqlite::Connection::open(&path).unwrap();
        let pairs = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = 's1'", [], |r| r.get(0)).unwrap()
        };
        let memory_hits = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COALESCE(SUM(hit_count), 0) FROM correction_memory", [], |r| r.get(0)).unwrap()
        };

        side.execute(
            "UPDATE speech_segments SET audio_path = ?1 WHERE id = 's1'",
            rusqlite::params![r"\\evil\share\a.wav"],
        )
        .unwrap();
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 500, "the row upsert must fail here — that is the state under test");

        let half = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(!half.verified, "write two did NOT land");
        assert_eq!(half.reviewed_by.as_deref(), Some("Sara"), "write one DID land — this is the half-written row");
        let pairs_after_failure = pairs(&side);
        let hits_after_failure = memory_hits(&side);
        assert!(
            pairs_after_failure >= 1,
            "the failing submit still minted the learning pair; nothing to test otherwise"
        );

        // The clip stays leased across the failure. (This is the write-TWO branch, which always held
        // the lease; the write-ONE branch that used to release it is covered by the SQLITE_BUSY test
        // below. Pinned here anyway so the two failure paths cannot drift apart.)
        let held = { lock_state(&state).holder("s1", Instant::now()).map(str::to_string) };
        assert_eq!(held.as_deref(), Some("Sara"), "a failed write must not release the reviewer's lease");

        // The outbox replays. The row is writable again (the real-world analogue: the DB is no longer busy).
        side.execute("UPDATE speech_segments SET audio_path = '/audio/a.wav' WHERE id = 's1'", []).unwrap();
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "the replay must succeed, not 409 or 500");

        let done = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(done.verified, "the replay FINISHES the interrupted write");
        assert_eq!(done.annotated_transcript.as_deref(), Some(fix));

        // DEFECT 2 (the dedup key): the guard keyed on `verified`, which only write two sets, so it
        // could not recognise its own half-written row and replayed the ENTIRE decision — a second DPO
        // pair from one human edit, and a correction_memory hit_count bumped as though a second,
        // independent segment had confirmed it.
        assert_eq!(pairs(&side), pairs_after_failure, "a replay must not mint a second learning pair");
        assert_eq!(memory_hits(&side), hits_after_failure, "a replay must not bump correction_memory hit_count");
    }

    #[test]
    fn undo_must_refuse_rather_than_erase_work_done_since_the_decision() {
        // The undo snapshot is a WHOLE-ROW copy taken at decision time and never refreshed, and the
        // restore rewrites every column from it — deliberately, so a pre-decision jury verdict is not
        // lost. The cost is that anything done to the clip in between is erased, and a decided clip is
        // exactly what the owner sees and edits at the desktop. api_decision already re-reads the fresh
        // row before writing for precisely this reason; undo did not.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        let state = state();

        let body = serde_json::json!({"id": "s1", "action": "edit", "text": "ڕاستکراوەی سارا"});
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        // The owner corrects the same clip at the desktop afterwards.
        crate::jury::record_human_decision_by(&db, "s1", "edit", Some("ڕاستکراوەی خاوەن"), None, Some("Owner"))
            .unwrap();

        // Sara now taps undo. Her snapshot predates the owner's edit entirely.
        let (code, _, msg) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "undo must not silently revert someone else's later work");
        assert!(String::from_utf8_lossy(&msg).contains("Owner"), "the refusal must name who would lose work: {msg:?}");
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(row.reviewed_by.as_deref(), Some("Owner"), "the owner's edit must survive the refused undo");
        assert_eq!(row.verdict_transcript.as_deref(), Some("ڕاستکراوەی خاوەن"));

        // And the entry is KEPT, not consumed by the refusal — a refused undo must not silently
        // become "nothing to undo" on the next press.
        let (code, _, _) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "still refused, and still present rather than swallowed");
    }

    #[test]
    fn a_clip_that_stopped_being_an_answer_key_is_reviewed_for_real_not_swallowed() {
        // The served-check set is never pruned and now survives restarts, so a pair can outlive the
        // answer key it was created against. Un-verifying the clip at the desktop (or re-transcribing
        // it) makes api_queue serve it as ordinary pending work again — nothing filters served checks
        // out of the work queue — and the old code then graded the reviewer's REAL transcription
        // against a key that no longer existed: a bogus score, a reply byte-identical to success, and
        // nothing written to the corpus. The clip stayed pending and was swallowed again every batch.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let mut gold = seg("g1", "دەقی خاو");
        gold.annotated_transcript = Some("دەقی ڕاست".into());
        gold.verified = true;
        db.insert_segment(&gold).unwrap();
        let state = state();

        // It was handed to Sara as a check while it was still verified.
        lock_state(&state).spot_checks.insert(("g1".to_string(), "Sara".to_string()));

        // The owner then un-verifies it, so it is pending work again and no longer an answer key.
        let mut reopened = db.get_segment_by_id("g1").unwrap().unwrap();
        reopened.verified = false;
        db.insert_segment(&reopened).unwrap();

        let body = serde_json::json!({"id": "g1", "action": "edit", "text": "ڕاستکراوەی سارا"});
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        let row = db.get_segment_by_id("g1").unwrap().unwrap();
        assert!(row.verified, "the review must actually LAND, not be graded and discarded");
        assert_eq!(row.annotated_transcript.as_deref(), Some("ڕاستکراوەی سارا"));
        assert_eq!(row.reviewed_by.as_deref(), Some("Sara"), "real work must be attributed");
        assert!(
            !lock_state(&state).spot_checks.contains(&("g1".to_string(), "Sara".to_string())),
            "the stale pair must be dropped so it cannot swallow this clip again"
        );
    }

    #[test]
    fn renewing_keeps_the_whole_batch_not_just_the_clip_on_screen() {
        // api_queue leases up to QUEUE_BATCH clips in ONE shot, all stamped with the same instant, so
        // they expire together — while the page heartbeats only the clip in front of the reviewer.
        // That gave them one LEASE_TTL to finish the entire batch (36s per clip at 25), and real
        // listen-and-correct on Sorani audio is nowhere near that. Everything they had not yet reached
        // fell out from under them mid-session, another reviewer's fetch took it, and their eventual
        // save was refused 409 with the correction already typed.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        for n in 0..3 {
            db.insert_segment(&seg(&format!("s{n}"), "دەق")).unwrap();
        }
        let state = state();
        let served = queue_ids(&db, "Sara", &state);
        assert_eq!(served.len(), 3, "all three leased to Sara in one batch");

        // Age every lease to the brink of expiry, as a real session does while the reviewer works.
        {
            let mut guard = lock_state(&state);
            let old = Instant::now() - (LEASE_TTL - Duration::from_secs(1));
            for (_, (_, granted)) in guard.leases.iter_mut() {
                *granted = old;
            }
        }
        // The reviewer is on the FIRST clip and the heartbeat fires for it.
        let body = serde_json::json!({"id": "s0"});
        let (code, _, _) = api_renew(body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        // Past the point the batch would have lapsed, the tail must still be hers.
        let later = Instant::now() + Duration::from_secs(5);
        let mut guard = lock_state(&state);
        for id in ["s0", "s1", "s2"] {
            assert_eq!(
                guard.holder(id, later).map(str::to_string).as_deref(),
                Some("Sara"),
                "{id} must still be held — an active reviewer keeps their whole batch"
            );
        }
    }

    #[test]
    fn concurrent_session_saves_never_leave_an_unparseable_file_or_stray_temps() {
        // save_session runs from any accept thread with no lock held, so several can be writing at
        // once. They used to share one fixed temp filename. A session file that does not parse is
        // unrecoverable — load_session gives up silently, resume returns None, and every link already
        // sent is dead — so the file must be either the old snapshot or a new one, never a splice.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let db_path = "some/library.db".to_string();

        let mut writers = Vec::new();
        for n in 0..8 {
            let dir = dir.clone();
            let db_path = db_path.clone();
            writers.push(std::thread::spawn(move || {
                let mut reviewers = HashMap::new();
                // Different sizes per thread, so a spliced file would leave a longer tail behind a
                // shorter body — the exact corruption shape a shared temp name can produce.
                for k in 0..=n {
                    reviewers.insert(format!("token-{n}-{k}-{}", "x".repeat(n * 8)), format!("Reviewer{n}{k}"));
                }
                for _ in 0..10 {
                    let _ = save_session(&dir, &reviewers, &db_path, &HashSet::new());
                }
            }));
        }
        for w in writers {
            w.join().unwrap();
        }

        let raw = std::fs::read_to_string(session_path(&dir)).expect("session file exists");
        let saved: SavedSession = serde_json::from_str(&raw).expect("session file must always parse");
        assert!(!saved.reviewers.is_empty(), "a promoted session must carry a real roster");
        assert_eq!(saved.db_path, db_path);

        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp files must be renamed away, not accumulate: {strays:?}");
    }

    #[test]
    fn a_busy_database_must_not_hand_the_clip_to_the_next_reviewer() {
        // The write-ONE failure branch, driven by the real trigger rather than a stand-in: another
        // connection holds the write lock, so `record_human_decision_by` blocks and then fails
        // SQLITE_BUSY — exactly what a desktop batch or import does to a phone submit.
        //
        // It used to release the lease on that failure ("nothing was written, so hand the clip
        // straight back"), which reads as tidy and is the opposite of safe: the reviewer still has the
        // clip open with their correction typed and now queued in the outbox, and a freed clip is
        // handed to the next reviewer's batch within seconds. Their replay then loses a race that
        // never needed to happen and is refused 409, and 409 is the branch that discards the queued
        // decision. A transient database error silently became permanent work loss.
        //
        // Costs one busy_timeout (10s, db.rs) by construction — the wait IS the mechanism under test.
        let tmp = tempfile::tempdir().unwrap();
        let (db, path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        let state = state();

        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").expect("take the write lock");

        let body = serde_json::json!({"id": "s1", "action": "edit", "text": "ڕاستکراوە"});
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 500, "a locked database must surface as a server error, not a verdict");

        let held = { lock_state(&state).holder("s1", Instant::now()).map(str::to_string) };
        assert_eq!(
            held.as_deref(),
            Some("Sara"),
            "the reviewer still has this clip open and their decision queued — releasing it here is what \
             let the next batch steal it and turn a transient DB error into lost work"
        );
        // Nothing landed, so the row is untouched and the reviewer's replay is a genuinely new write.
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(!row.verified);
        assert_eq!(row.reviewed_by, None);
        blocker.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn a_queued_decision_is_never_recorded_under_a_different_reviewers_name() {
        // The outbox lives in localStorage, which is per-ORIGIN and not per-reviewer. Two people using
        // one phone — or one person opening a colleague's link — share it, so a decision Sara queued
        // while offline could flush under Hemn's cookie and be recorded, permanently, as Hemn's
        // judgement of a clip he never heard. Attribution is the one thing a review corpus cannot be
        // wrong about, and it was decided by whoever happened to be logged in at flush time.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        db.insert_segment(&seg("s2", "دەق دوو")).unwrap();
        let state = state();

        let sara_work = serde_json::json!({"id": "s1", "action": "edit", "text": "ڕاستکراوە", "reviewer": "Sara"});
        let (code, _, msg) = api_decision(&db, sara_work.to_string().as_bytes(), "Hemn", &state);
        assert_eq!(code, 409, "Sara's decision must not be written under Hemn's name");
        assert!(String::from_utf8_lossy(&msg).contains("Sara"), "the refusal must name the real author: {msg:?}");
        let untouched = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(!untouched.verified, "a mis-attributed decision must write NOTHING");
        assert_eq!(untouched.reviewed_by, None);

        // The very same queued item, flushed on Sara's own link, lands normally — it was never invalid,
        // only in the wrong hands.
        let (code, _, _) = api_decision(&db, sara_work.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);
        assert_eq!(db.get_segment_by_id("s1").unwrap().unwrap().reviewed_by.as_deref(), Some("Sara"));

        // A live submit carries no claim at all, and that ordinary path is unchanged.
        let live = serde_json::json!({"id": "s2", "action": "bad"});
        let (code, _, _) = api_decision(&db, live.to_string().as_bytes(), "Hemn", &state);
        assert_eq!(code, 200, "a submit with no reviewer field is the normal live path");
        assert_eq!(db.get_segment_by_id("s2").unwrap().unwrap().reviewed_by.as_deref(), Some("Hemn"));
    }

    #[test]
    fn undo_is_per_reviewer_and_can_never_retract_someone_elses_decision() {
        // DATA LOSS, and the single most dangerous consequence of adding a second reviewer to the
        // original design: ONE shared undo stack meant reviewer B's ↩ popped whichever decision was last
        // globally — usually A's, on a clip B had never seen, with no indication to either of them. The
        // stack is keyed by reviewer, so B's undo reaches only B's own work.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("a1", "هی سارا")).unwrap();
        db.insert_segment(&seg("b1", "هی هێمن")).unwrap();
        let state = state();

        let accept = |id: &str| serde_json::json!({"id": id, "action": "accept", "text": "هەر وایە"}).to_string();
        assert_eq!(api_decision(&db, accept("a1").as_bytes(), "Sara", &state).0, 200);
        assert_eq!(api_decision(&db, accept("b1").as_bytes(), "Hemn", &state).0, 200);

        // Hemn undoes. Only b1 may move — a1 is Sara's and she has not asked for it back.
        let (code, _, body) = api_undo(&db, "Hemn", &state);
        assert_eq!(code, 200);
        let undone: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(undone["id"], "b1", "Hemn's undo must reach Hemn's own last decision");

        let sara_row = db.get_segment_by_id("a1").unwrap().unwrap();
        assert!(sara_row.verified, "Sara's decision must survive Hemn's undo");
        assert_eq!(sara_row.reviewed_by.as_deref(), Some("Sara"));
        assert!(!db.get_segment_by_id("b1").unwrap().unwrap().verified, "Hemn's own clip reopened");

        // A reviewer with nothing of their own gets 409, not a phantom undo of someone else's work.
        assert_eq!(api_undo(&db, "Hemn", &state).0, 409, "Hemn's stack is empty — his one decision is undone");
        assert_eq!(api_undo(&db, "Nobody", &state).0, 409, "a reviewer who has decided nothing can undo nothing");
        assert!(db.get_segment_by_id("a1").unwrap().unwrap().verified, "and Sara's row is STILL untouched");
    }

    #[test]
    fn a_clip_served_to_one_reviewer_is_not_handed_to_another() {
        // Without leasing, every reviewer is served the same head of the queue: two people transcribe the
        // same clip (wasted work) and race to save it (one verdict silently overwrites the other). The
        // lease partitions the backlog, and a reviewer reloading gets their OWN batch back rather than
        // abandoning work in progress.
        //
        // Sized deliberately ABOVE QUEUE_BATCH, because that is the interesting case: the first reviewer
        // takes a full batch and the second must still find real work rather than an apparently empty
        // queue. (The below-a-batch case is covered by the honesty assertion at the end.)
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let total = QUEUE_BATCH + 5;
        for i in 0..total {
            db.insert_segment(&seg(&format!("c{i:03}"), "دەق")).unwrap();
        }
        let state = state();

        let sara = queue_ids(&db, "Sara", &state);
        let hemn = queue_ids(&db, "Hemn", &state);
        assert_eq!(sara.len(), QUEUE_BATCH, "the first reviewer takes one batch, not the whole backlog");
        assert_eq!(hemn.len(), total - QUEUE_BATCH, "the second reviewer gets the rest");
        for id in &sara {
            assert!(!hemn.contains(id), "clip {id} was handed to BOTH reviewers");
        }

        // Reloading returns the same batch — a refresh must not abandon a half-finished correction.
        assert_eq!(queue_ids(&db, "Sara", &state), sara, "a reload keeps the reviewer's own leased clips");

        // HONESTY: with everything leased out, a third reviewer's queue is empty — but the work is NOT
        // done, and the page must not be able to claim it is. The count of clips held elsewhere is what
        // lets it say "someone else has them" instead of "🎉 Queue reviewed".
        let (code, _, body) = api_queue(&db, "Ali", &state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(payload["items"].as_array().unwrap().is_empty(), "nothing is free for a third reviewer");
        assert_eq!(payload["heldByOthers"], total, "but all {total} pending clips are reported as held");
    }

    #[test]
    fn deciding_a_clip_another_reviewer_holds_is_refused() {
        // The page could have been loaded before the clip was leased elsewhere. Accepting the late submit
        // would overwrite a verdict the other reviewer reached independently — destroying one of two
        // honest human judgements with nobody aware. Refusing (409) is the only outcome that loses nothing.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("x1", "دەق")).unwrap();
        let state = state();

        assert_eq!(queue_ids(&db, "Sara", &state), vec!["x1"], "Sara leases the only clip");
        let body = serde_json::json!({"id": "x1", "action": "accept", "text": "دەق"}).to_string();
        let (code, _, msg) = api_decision(&db, body.as_bytes(), "Hemn", &state);
        assert_eq!(code, 409, "Hemn must not decide a clip Sara holds");
        assert!(String::from_utf8_lossy(&msg).contains("another reviewer"), "the refusal says why");
        let row = db.get_segment_by_id("x1").unwrap().unwrap();
        assert!(!row.verified, "the refused decision wrote NOTHING");
        assert_eq!(row.reviewed_by, None);

        // The holder decides it normally.
        assert_eq!(api_decision(&db, body.as_bytes(), "Sara", &state).0, 200);
        assert_eq!(db.get_segment_by_id("x1").unwrap().unwrap().reviewed_by.as_deref(), Some("Sara"));
    }

    #[test]
    fn an_unqueued_submit_claims_the_clip_but_a_rejected_one_leaves_no_lease() {
        // Two halves of the same guard, both about WHERE the check+claim sits in api_decision.
        //
        // (a) A submit for a clip nobody leased — a page kept open across a restart — must CLAIM it, not
        //     merely check it. Check-then-write without a claim lets two such submits both pass and both
        //     write, which is the very race the lease exists to prevent.
        // (b) A REJECTED submit must leave no lease behind. If the claim happened before validation, a
        //     single malformed request would lock a clip away from every other reviewer for the full
        //     15-minute TTL — a denial of service by typo.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("u1", "دەق")).unwrap();
        db.insert_segment(&seg("u2", "دەق")).unwrap();
        let state = state();

        // (a) Sara submits for u1 without ever calling /api/queue.
        let ok = serde_json::json!({"id": "u1", "action": "accept", "text": "دەق"}).to_string();
        assert_eq!(api_decision(&db, ok.as_bytes(), "Sara", &state).0, 200);
        assert_eq!(
            lock_state(&state).leases.get("u1").map(|(who, _)| who.clone()),
            None,
            "a DECIDED clip releases its lease immediately — it is no longer pending work"
        );

        // (b) every rejected shape, none of which may strand a lease on u2.
        for bad in [
            serde_json::json!({"id": "u2", "action": "accept", "text": "[Pending WSL 7B ASR]"}),
            serde_json::json!({"id": "u2", "action": "edit", "text": ""}),
            serde_json::json!({"id": "u2", "action": "explode", "text": "x"}),
        ] {
            let (code, _, _) = api_decision(&db, bad.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 400, "body: {bad}");
            assert!(
                !lock_state(&state).leases.contains_key("u2"),
                "a rejected request left a lease on u2, locking it away from every other reviewer: {bad}"
            );
        }
        // So the clip is still free for someone else to pick up and decide.
        assert_eq!(queue_ids(&db, "Hemn", &state), vec!["u2"], "u2 is still available to another reviewer");
    }

    #[test]
    fn an_expired_lease_returns_the_clip_to_everyone_else() {
        // A reviewer who closes the page mid-batch must not strand that work for the rest of the session.
        // Leases carry a TTL and are ignored (and pruned) once past it, so the clip flows back into the
        // next reviewer's queue with no explicit release and no disconnect detection.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("stale", "دەق")).unwrap();
        let state = state();

        assert_eq!(queue_ids(&db, "Sara", &state), vec!["stale"]);
        assert!(queue_ids(&db, "Hemn", &state).is_empty(), "while the lease is live, Hemn sees nothing");

        // Backdate Sara's lease past the TTL — she walked away.
        let expired = Instant::now().checked_sub(LEASE_TTL).expect("a monotonic clock at least TTL old");
        lock_state(&state).leases.insert("stale".into(), ("Sara".into(), expired));

        assert_eq!(queue_ids(&db, "Hemn", &state), vec!["stale"], "the abandoned clip returns to the pool");
        assert_eq!(
            lock_state(&state).leases.get("stale").map(|(who, _)| who.clone()),
            Some("Hemn".to_string()),
            "and is now held by whoever picked it up"
        );
    }

    #[test]
    fn a_retried_submit_is_recorded_once_but_a_real_re_review_still_lands() {
        // P1.2. A phone on the edge of Wi-Fi loses RESPONSES, not just requests: the write commits,
        // the page hears nothing, and it retries. Without idempotency that retry is a SECOND human
        // decision — a second undo entry, and for an edit a second DPO learning pair distilled from a
        // correction the human made once. This is what makes retrying safe enough to do at all.
        //
        // The narrowness matters as much as the idempotency: change one character and it must be a
        // genuine re-review that IS recorded, or a reviewer could never correct their own mistake.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("r1", "دەق")).unwrap();
        db.insert_segment(&seg("r2", "دەق")).unwrap();
        let state = state();

        let edit = serde_json::json!({"id": "r1", "action": "edit", "text": "دەقی ڕاستکراوە"}).to_string();
        assert_eq!(api_decision(&db, edit.as_bytes(), "Sara", &state).0, 200);
        let pairs = |id: &str| -> i64 {
            db.connection()
                .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(pairs("r1"), 1, "the edit produced exactly one learning pair");

        // The identical resubmit — the retry — must be answered as already-done.
        let (code, _, body) = api_decision(&db, edit.as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a retry must succeed, not error");
        let reply: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply["duplicate"], true, "and must say it was already recorded");
        assert_eq!(pairs("r1"), 1, "the retry must NOT distil a second learning pair");
        assert_eq!(
            lock_state(&state).undo.get("Sara").map(Vec::len),
            Some(1),
            "the retry must NOT push a second undo entry — one human act, one undo"
        );

        // A GENUINE re-review (different text) is a new decision and must land.
        let redo = serde_json::json!({"id": "r1", "action": "edit", "text": "دەقی دیسان ڕاستکراوە"}).to_string();
        assert_eq!(api_decision(&db, redo.as_bytes(), "Sara", &state).0, 200);
        assert_eq!(
            db.get_segment_by_id("r1").unwrap().unwrap().annotated_transcript.as_deref(),
            Some("دەقی دیسان ڕاستکراوە"),
            "changing the text is a real re-review and must be recorded"
        );

        // A REJECT retry must also be absorbed. Only the edit path was covered, so deleting the
        // ("reject", _) arm of is_repeat_of_stored_decision changed nothing any test could see —
        // and a re-sent reject would have been recorded as a second human decision.
        let rej = serde_json::json!({"id": "r2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, rej.as_bytes(), "Sara", &state).0, 200);
        let before = lock_state(&state).undo.get("Sara").map(Vec::len).unwrap_or(0);
        let (code, _, body) = api_decision(&db, rej.as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a re-sent reject must succeed");
        let reply: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply["duplicate"], true, "and be recognised as already recorded");
        assert_eq!(
            lock_state(&state).undo.get("Sara").map(Vec::len).unwrap_or(0),
            before,
            "a retried reject must not push a second undo entry"
        );

        // Someone ELSE's identical submit is never a "retry" — and it must not be allowed through as a
        // fresh decision either. Writing this assertion is what exposed the LATE-SUBMIT gap: the lease
        // is released the instant a clip is decided, so an already-decided clip had NO protection, and
        // a stale page could silently replace another human's verdict minutes later.
        let reject = serde_json::json!({"id": "r2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, reject.as_bytes(), "Sara", &state).0, 200);
        let (code, _, msg) = api_decision(&db, reject.as_bytes(), "Hemn", &state);
        assert_eq!(code, 409, "a decided clip must not be re-decided by a different reviewer");
        assert!(
            String::from_utf8_lossy(&msg).contains("already reviewed by Sara"),
            "and the refusal must name whose verdict it is protecting: {}",
            String::from_utf8_lossy(&msg)
        );
        assert_eq!(
            db.get_segment_by_id("r2").unwrap().unwrap().reviewed_by.as_deref(),
            Some("Sara"),
            "Sara's attribution must survive the late submit untouched"
        );
    }

    #[test]
    fn renewing_keeps_an_active_reviewers_clip_but_never_steals_one() {
        // P1.3. A 15-minute lease can expire mid-clip on hard audio. Without renewal the reviewer
        // finds out at SAVE time, as a 409, with the correction already typed and now unsaveable —
        // the exact work-destroying moment leases exist to prevent.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("n1", "دەق")).unwrap();
        let state = state();
        let body = serde_json::json!({"id": "n1"}).to_string();

        assert_eq!(queue_ids(&db, "Sara", &state), vec!["n1"]);
        // Age her lease to the brink, then renew: the clip must stay hers rather than lapsing.
        let stale = Instant::now().checked_sub(LEASE_TTL - Duration::from_secs(1)).unwrap();
        lock_state(&state).leases.insert("n1".into(), ("Sara".into(), stale));
        let (code, _, reply) = api_renew(body.as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "an active reviewer may extend their own hold");
        let json: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(json["ttlSeconds"], LEASE_TTL.as_secs(), "the page is told the real TTL, not a guess");
        assert!(queue_ids(&db, "Hemn", &state).is_empty(), "the renewed clip is still Sara's");

        // Renewal is NOT a way to take someone else's clip.
        let (code, _, msg) = api_renew(body.as_bytes(), "Hemn", &state);
        assert_eq!(code, 409, "renewal must never steal a live lease");
        assert!(String::from_utf8_lossy(&msg).contains("another reviewer"));
        assert_eq!(
            lock_state(&state).leases.get("n1").map(|(who, _)| who.clone()),
            Some("Sara".to_string()),
            "and the refused renewal must not have moved the holder"
        );

        // Reclaiming a clip whose lease lapsed with nobody else taking it is allowed and deliberate.
        let expired = Instant::now().checked_sub(LEASE_TTL).unwrap();
        lock_state(&state).leases.insert("n1".into(), ("Sara".into(), expired));
        assert_eq!(api_renew(body.as_bytes(), "Sara", &state).0, 200, "she still has it open — give it back");
        assert_eq!(api_renew(b"not json", "Sara", &state).0, 400);
        assert_eq!(api_renew(serde_json::json!({"id": "../etc"}).to_string().as_bytes(), "Sara", &state).0, 400);
    }

    /// A GOLD clip with a known human answer whose RAW draft is wrong — the shape a spot check needs.
    /// `is_gold` matters: without it a peer's fresh correction would qualify as an answer key.
    fn gold_seg(db: &Database, id: &str, wrong_draft: &str, human_answer: &str) {
        let mut s = seg(id, wrong_draft);
        s.verified = true;
        s.is_gold = true;
        s.human_decision = Some("edit".into());
        s.verdict = Some("human_edit".into());
        s.verdict_transcript = Some(human_answer.into());
        db.insert_segment_full(&s).unwrap();
    }

    #[test]
    fn spot_checks_catch_a_blind_accepter_without_touching_the_corpus() {
        // P2.1 — the item the whole remote-review plan turns on. Once review is handed to other
        // people the dominant failure mode is a human tapping "accept" without listening, and nothing
        // in this repo measured that: every other gate asks whether the MACHINE is honest.
        //
        // The mechanism uses no synthetic data. A clip that a human already answered, served with its
        // RAW (known-wrong) draft, is a trap that separates listening from tapping by itself.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        gold_seg(&db, "g1", "دەقی هەڵە", "دەقی ڕاست");
        gold_seg(&db, "g2", "دەقی هەڵەی دوو", "دەقی ڕاستی دوو");
        let state = state();

        let candidates = db.list_spot_check_candidates(10, "Sara").unwrap();
        assert_eq!(candidates.len(), 2, "both clips have a human answer that differs from the raw draft");
        // A check is graded only because it was SERVED as one; mark both as served to each reviewer,
        // which is exactly what api_queue records when it salts a batch.
        for id in ["g1", "g2"] {
            for who in ["Sara", "Hemn"] {
                lock_state(&state).spot_checks.insert((id.to_string(), who.to_string()));
            }
        }

        // Sara LISTENS: she corrects the wrong draft to the known answer.
        let fix = serde_json::json!({"id": "g1", "action": "edit", "text": "دەقی ڕاست"}).to_string();
        assert_eq!(api_decision(&db, fix.as_bytes(), "Sara", &state).0, 200);
        // Hemn does NOT: he hands the wrong draft straight back.
        let blind = serde_json::json!({"id": "g1", "action": "accept", "text": "دەقی هەڵە"}).to_string();
        assert_eq!(api_decision(&db, blind.as_bytes(), "Hemn", &state).0, 200);

        // Re-queries every call: a snapshot taken once would be read as live after later decisions.
        let of = |who: &str| {
            db.spot_check_report().unwrap().into_iter().find(|r| r.reviewer == who).expect("reviewer scored")
        };
        let report = db.spot_check_report().unwrap();
        assert_eq!(of("Sara").noticed, 1, "correcting a wrong draft is noticing");
        assert_eq!(of("Sara").mean_cer, 0.0, "and her answer matched the known one exactly");
        assert_eq!(of("Hemn").noticed, 0, "handing the wrong draft back is NOT noticing");
        assert!(of("Hemn").mean_cer > 0.0, "and his answer is measurably wrong");
        assert_eq!(report[0].reviewer, "Hemn", "the report leads with the reviewer who may not be listening");

        // THE CORPUS IS UNTOUCHED. A mechanism that grades reviewers must never be able to alter the
        // data it grades against — otherwise a blind accept would overwrite the very answer key.
        let row = db.get_segment_by_id("g1").unwrap().unwrap();
        assert_eq!(row.verdict_transcript.as_deref(), Some("دەقی ڕاست"), "the known answer must survive");
        assert_eq!(row.reviewed_by, None, "a spot check is not a review — it must not claim attribution");
        assert_eq!(row.annotated_transcript, None, "and must not write the reviewer's text into the corpus");
        assert!(lock_state(&state).undo.get("Hemn").is_none_or(Vec::is_empty), "nor leave an undo entry");

        // A retry must not inflate a score: the row is keyed (segment, reviewer) and upserts.
        assert_eq!(api_decision(&db, blind.as_bytes(), "Hemn", &state).0, 200);
        assert_eq!(of("Hemn").checks, 1, "a retried spot check is still ONE check, not two");

        // Rejecting counts as noticing — judging a clip unusable is attention, not a blind accept.
        let reject = serde_json::json!({"id": "g2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, reject.as_bytes(), "Hemn", &state).0, 200);
        let hemn = of("Hemn");
        assert_eq!((hemn.checks, hemn.noticed), (2, 1), "the reject scored as noticed");
    }

    #[test]
    fn the_queue_hides_spot_checks_among_real_work_and_never_leases_them() {
        // Two properties, both load-bearing:
        //   * a reviewer who can SPOT the test is not being tested — so the payload carries no marker
        //     and the traps are interleaved, not appended in a run at the tail;
        //   * spot checks are deliberately NOT leased, because two reviewers meeting the same
        //     known-answer clip is independent measurement, not a collision to prevent.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        // More pending work than ONE batch, so the second reviewer still has real work to be salted —
        // a reviewer with nothing to do is correctly served nothing at all, checks included.
        for n in 0..QUEUE_BATCH + 5 {
            db.insert_segment(&seg(&format!("p{n:02}"), "کاری ڕاستەقینە")).unwrap();
        }
        gold_seg(&db, "gold-a", "دەقی هەڵە", "دەقی ڕاست");
        let state = state();

        let (code, _, body) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = payload["items"].as_array().unwrap();
        let ids: Vec<&str> = items.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"gold-a"), "the batch must contain a spot check: {ids:?}");
        assert_ne!(ids.last(), Some(&"gold-a"), "a trap always at the tail is a pattern, not a test");

        // Every item exposes exactly the same fields — nothing distinguishes a check from real work.
        let keys = |v: &serde_json::Value| {
            let mut k: Vec<&String> = v.as_object().unwrap().keys().collect();
            k.sort();
            k.into_iter().cloned().collect::<Vec<String>>()
        };
        let gold_item = items.iter().find(|s| s["id"] == "gold-a").unwrap();
        let real_item = items.iter().find(|s| s["id"] != "gold-a").unwrap();
        assert_eq!(keys(gold_item), keys(real_item), "a spot check must be indistinguishable from real work");
        assert_eq!(
            gold_item["text"], "دەقی هەڵە",
            "it is served with the WRONG draft — else there is nothing to catch"
        );

        // Not leased: Hemn must be able to receive the same check independently.
        assert!(
            !lock_state(&state).leases.contains_key("gold-a"),
            "spot checks must not be leased — two reviewers meeting one is the point"
        );
        let (_, _, body) = api_queue(&db, "Hemn", &state);
        let hemn: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let hemn_ids: Vec<&str> = hemn["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(hemn_ids.contains(&"gold-a"), "the same check must reach a second reviewer: {hemn_ids:?}");
    }

    #[test]
    fn a_runaway_page_is_throttled_per_reviewer() {
        // P1.6. Every desktop IPC command is rate-limited; these HTTP routes were the one unthrottled
        // path into the database. The budget is far above any human, so this asserts the LIMIT EXISTS
        // and is per-reviewer — one person's stuck tab must not be able to starve anyone else.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let state = Mutex::new(CouchState {
            reviewers: HashMap::from([
                ("t-a".to_string(), "Loop".to_string()),
                ("t-b".to_string(), "Calm".to_string()),
            ]),
            ..CouchState::default()
        });

        // Drain "Loop"'s bucket directly — the same limiter the request path calls.
        let mut throttled = false;
        for _ in 0..500 {
            if COUCH_RATE_LIMITER.check("Loop").is_err() {
                throttled = true;
                break;
            }
        }
        assert!(throttled, "an unbounded request loop must eventually be refused");

        // The throttled reviewer gets 429 through the real request path...
        let reply = route_for_test(&db, &state, "t-a");
        assert_eq!(reply.0, 429, "a throttled reviewer is refused with 429, not served");
        // ...while the other reviewer is completely unaffected.
        assert_eq!(route_for_test(&db, &state, "t-b").0, 200, "one bad tab must not starve anyone else");
    }

    /// Drive `handle_request`'s auth + throttle path without a live socket, by issuing a real request
    /// against a loopback server and handing it to the handler exactly as an accept thread would.
    fn route_for_test(db: &Database, state: &Mutex<CouchState>, token: &str) -> Reply {
        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/api/queue?t={token}");
        let client = std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(5)).build();
            let _ = agent.get(&url).call();
        });
        let mut request = server.recv().unwrap();
        let (reply, cookie) = handle_request(&mut request, db, state);
        let _ = respond_with_cookie(request, reply.clone(), cookie);
        let _ = client.join();
        reply
    }

    #[test]
    fn the_shell_is_public_the_data_is_not_and_a_fragment_claim_mints_the_cookie() {
        // Phase 2 of docs/REMOTE_PUBLIC_LINKS_PLAN.md, pinned over real HTTP. The threat this shape
        // exists for: a link pasted into WhatsApp/Telegram IS fetched by the platform's preview bot
        // within seconds. With `?t=` that fetch handed the platform a durable credential to
        // biometric audio. With `#t=` the bot's request carries no fragment — it must receive the
        // EMPTY shell and, critically, NO cookie.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەقی تاقیکردنەوە")).unwrap();
        drop(db);

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: HashMap::from([("goodtoken".to_string(), "Sara".to_string())]),
            ..CouchState::default()
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path, state, shutdown.clone()).unwrap();
        let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(10)).build();
        let base = format!("http://127.0.0.1:{port}");

        // THE PREVIEW BOT'S REQUEST: bare GET /, no credential of any kind.
        let bot = agent.get(&format!("{base}/")).call().expect("the shell is public");
        assert_eq!(bot.status(), 200);
        assert!(bot.header("set-cookie").is_none(), "an unauthenticated shell fetch must never mint a cookie");
        let shell = bot.into_string().unwrap();
        assert!(shell.contains("<html"), "it is the page shell");
        assert!(!shell.contains("goodtoken"), "and it embeds no credential");

        // The data stays gated exactly as before.
        let data = agent.get(&format!("{base}/api/queue")).call();
        assert!(matches!(data, Err(ureq::Error::Status(401, _))), "the shell being public must not open the data");

        // The install assets (phase 6) are public like the shell, carry no cookie, and the manifest's
        // start_url must NEVER embed a credential — an installed app rides the cookie, so revoking a
        // reviewer revokes their installed app too.
        let icon = agent.get(&format!("{base}/icon.png")).call().expect("icon is public");
        assert_eq!(icon.header("content-type"), Some("image/png"));
        assert!(icon.header("set-cookie").is_none());
        let manifest = agent.get(&format!("{base}/manifest.json")).call().expect("manifest is public");
        assert!(manifest.header("set-cookie").is_none());
        let manifest: serde_json::Value = manifest.into_json().unwrap();
        assert_eq!(manifest["start_url"], "/", "an installed app must never freeze a token into its start_url");

        // THE REVIEWER'S FIRST OPEN: the page presents the fragment token once, by POST body.
        let claim = agent
            .post(&format!("{base}/api/claim"))
            .set("content-type", "application/json")
            .send_string(r#"{"token":"goodtoken"}"#)
            .expect("a valid claim succeeds");
        let cookie = claim.header("set-cookie").expect("the claim mints the cookie").to_string();
        assert!(cookie.contains("HttpOnly"), "the minted cookie must stay unreadable to page JS");
        let cookie_pair = cookie.split(';').next().unwrap().to_string();

        // A wrong token gets the same flat refusal as everywhere else.
        let bad = agent
            .post(&format!("{base}/api/claim"))
            .set("content-type", "application/json")
            .send_string(r#"{"token":"guessed"}"#);
        assert!(matches!(bad, Err(ureq::Error::Status(401, _))), "claim must not be a token oracle");

        // The minted cookie is a working credential...
        let queue = agent.get(&format!("{base}/api/queue")).set("Cookie", &cookie_pair).call().unwrap();
        assert_eq!(queue.status(), 200);
        // ...and an authenticated page load RE-plants it (sliding expiry): a bookmark used weekly
        // must never die of a Max-Age counted from the first claim.
        let revisit = agent.get(&format!("{base}/")).set("Cookie", &cookie_pair).call().unwrap();
        assert!(
            revisit.header("set-cookie").is_some_and(|c| c.contains("Max-Age")),
            "an authenticated page load must refresh the cookie's lifetime"
        );

        shutdown.store(true, Ordering::SeqCst);
        server.unblock();
        let _ = join.join();
    }

    #[test]
    fn a_returning_reviewer_is_let_in_by_their_cookie_even_though_the_page_sends_an_empty_token() {
        // THE REPORTED BUG: "I close the browser on my iPhone and go back and it doesn't open."
        //
        // Every piece worked in isolation, which is why nothing caught it. The server accepted the
        // cookie in every form when asked directly; the browser faithfully sent the cookie on the
        // return visit; the page correctly stripped the token from the URL. What no test covered was
        // the COMBINATION the second visit actually produces: an empty `?t=` alongside a valid
        // cookie. The empty query won, and the reviewer was refused while holding a good credential.
        //
        // Driven over real HTTP against a real server rather than by calling token_from_request,
        // because the defect lives in how the two credentials interact on one request.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەقی تاقیکردنەوە")).unwrap();
        drop(db);

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: HashMap::from([("goodtoken".to_string(), "Sara".to_string())]),
            ..CouchState::default()
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path, state, shutdown.clone()).unwrap();

        let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(10)).build();
        let base = format!("http://127.0.0.1:{port}");
        let status = |url: String, cookie: Option<&str>| -> u16 {
            let mut req = agent.get(&url);
            if let Some(c) = cookie {
                req = req.set("Cookie", c);
            }
            req.call().map(|r| r.status()).unwrap_or_else(|e| match e {
                ureq::Error::Status(c, _) => c,
                other => panic!("transport error: {other}"),
            })
        };
        let cookie = format!("{COUCH_COOKIE}=goodtoken");

        // The first visit: the token is in the URL.
        assert_eq!(status(format!("{base}/api/queue?t=goodtoken"), None), 200);
        // The RETURN visit, exactly as the page issues it — empty t=, cookie present.
        assert_eq!(
            status(format!("{base}/api/queue?t="), Some(&cookie)),
            200,
            "a returning reviewer must be let in by their cookie; the page sends an empty t= because \
             it stripped the token from the URL on the first load"
        );
        // Cookie alone (no query at all) keeps working.
        assert_eq!(status(format!("{base}/api/queue"), Some(&cookie)), 200);
        // And the gate is NOT loosened: no credential, or an explicit WRONG one, still fails.
        assert_eq!(status(format!("{base}/api/queue?t="), None), 401, "an empty token alone is not a way in");
        assert_eq!(
            status(format!("{base}/api/queue?t=revoked"), Some(&cookie)),
            401,
            "an explicit wrong token must not silently fall back to a cookie — that would let a \
             revoked link keep working"
        );

        shutdown.store(true, Ordering::SeqCst);
        server.unblock();
        let _ = join.join();
    }

    #[test]
    fn a_large_body_is_sent_with_a_content_length_so_audio_has_a_finite_duration() {
        // REGRESSION, found on the live server and not by any test: tiny_http chunks any body at or
        // above its 32 KB default threshold, and a chunked response carries NO Content-Length. Clips
        // are 300-500 KB, so every single one arrived with an unknown length — and an <audio> element
        // fed a stream of unknown length reports `duration = Infinity`. The phone's progress bar had
        // no total time and tap-to-seek multiplied by Infinity, so seeking did nothing at all.
        //
        // Sized deliberately ABOVE the 32 KB default: at 1 KB this test passes even with the bug,
        // which is exactly why the bug survived a suite full of small JSON replies.
        const BIG: usize = 200 * 1024;
        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/");
        let client = std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(5)).build();
            let r = agent.get(&url).call().expect("large body is served");
            (r.header("content-length").map(str::to_string), r.header("transfer-encoding").map(str::to_string))
        });
        let request = server.recv().unwrap();
        let _ = respond_with_cookie(request, (200, "audio/wav", vec![0u8; BIG]), None);
        let (content_length, transfer_encoding) = client.join().expect("client thread");

        assert_eq!(
            content_length.as_deref(),
            Some(BIG.to_string().as_str()),
            "a body whose length is known must declare it, or the browser cannot compute a duration"
        );
        assert_eq!(transfer_encoding, None, "nothing here needs chunking — the body is already in memory");
    }

    #[test]
    fn revoking_one_reviewer_stops_only_their_token_and_frees_their_clips() {
        // P3.7. Before this, removing one person meant STOPPING the server, which regenerates every
        // token and forces the owner to re-issue links to reviewers who did nothing wrong.
        //
        // The first version of this was a NO-OP and the compiler could not see it: each accept thread
        // held an `Arc<HashMap>` SNAPSHOT of the tokens, so removing one from the owner's handle left
        // every serving thread honouring it forever — a revoke that revoked nothing, visible only in
        // the owner's UI. The token map now lives in the one shared state the threads authenticate
        // against, which is what this test pins.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("rv1", "دەق")).unwrap();
        let state = Mutex::new(CouchState {
            reviewers: HashMap::from([
                ("tok-sara".to_string(), "Sara".to_string()),
                ("tok-hemn".to_string(), "Hemn".to_string()),
            ]),
            ..CouchState::default()
        });

        // Sara holds the only clip.
        assert_eq!(queue_ids(&db, "Sara", &state), vec!["rv1"]);
        assert!(queue_ids(&db, "Hemn", &state).is_empty(), "Sara holds it, so Hemn sees nothing");

        // The REAL revocation path, not a reimplementation of it.
        assert!(revoke_in(&state, "sara").is_ok(), "revoke matches the name case-insensitively");
        assert!(revoke_in(&state, "Nobody").is_err(), "an unknown name is refused, not silently ignored");
        assert!(
            revoke_in(&state, "Hemn").is_err(),
            "revoking the LAST reviewer must be refused - a running server nobody can reach is worse              than a stopped one, and stop() is the honest way to say that"
        );

        // Her token no longer resolves to anyone, so every route refuses it...
        assert_eq!(route_for_test(&db, &state, "tok-sara").0, 401, "a revoked token must stop working at once");
        // ...while Hemn is entirely unaffected...
        assert_eq!(route_for_test(&db, &state, "tok-hemn").0, 200, "revoking one link must not touch another");
        // ...and the clip she was holding is back in the pool rather than stranded behind her.
        assert_eq!(queue_ids(&db, "Hemn", &state), vec!["rv1"], "a revoked reviewer's held work returns");
    }

    #[test]
    fn every_decision_lands_in_the_append_only_audit_trail() {
        // P2.2 + P2.3. Nothing in the schema answered "who decided this, and when": `decision_log`
        // gets no phone rows (the couch passes no timestamp) and has no reviewer column; `corrections`
        // records before/after only for EDITS whose audio identity resolves. This trail answers it.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        for id in ["e1", "e2"] {
            db.insert_segment(&seg(id, "دەق")).unwrap();
        }
        gold_seg(&db, "e-gold", "دەقی هەڵە", "دەقی ڕاست");
        let state = state();
        lock_state(&state).spot_checks.insert(("e-gold".to_string(), "Sara".to_string()));

        let edit = serde_json::json!({"id": "e1", "action": "edit", "text": "ڕاستکراوە"}).to_string();
        assert_eq!(api_decision(&db, edit.as_bytes(), "Sara", &state).0, 200);
        let reject = serde_json::json!({"id": "e2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, reject.as_bytes(), "Hemn", &state).0, 200);
        // A spot check is real work to the reviewer who did it, so it is audited too.
        let check = serde_json::json!({"id": "e-gold", "action": "accept", "text": "دەقی ڕاست"}).to_string();
        assert_eq!(api_decision(&db, check.as_bytes(), "Sara", &state).0, 200);

        let rows: Vec<(String, String, String)> = db
            .connection()
            .prepare("SELECT segment_id, reviewer, action FROM review_events ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rows.len(), 3, "one row per decision, spot check included: {rows:?}");
        assert_eq!(rows[0], ("e1".into(), "Sara".into(), "edit".into()));
        assert_eq!(rows[1], ("e2".into(), "Hemn".into(), "reject".into()));
        // "edit", not "accept", even though the page sent action:"accept". The server classifies by
        // what the human actually DID — the submitted text differs from the draft they were shown —
        // rather than by which button was tapped. An audit trail that recorded the button would
        // describe the UI, not the work.
        assert_eq!(rows[2], ("e-gold".into(), "Sara".into(), "edit".into()));

        // Throughput is per reviewer and counts DISTINCT clips, so a retry cannot inflate it.
        assert_eq!(api_decision(&db, edit.as_bytes(), "Sara", &state).0, 200); // retry: duplicate
        let t = db.reviewer_throughput().unwrap();
        let sara = t.iter().find(|r| r.reviewer == "Sara").expect("Sara has throughput");
        assert_eq!(sara.clips, 2, "distinct clips, not rows — a retry is not extra work");
        assert_eq!(t.iter().find(|r| r.reviewer == "Hemn").unwrap().clips, 1);
        assert_eq!(t[0].reviewer, "Sara", "busiest reviewer first");
    }

    #[test]
    fn start_issues_working_tokens_and_stop_takes_them_away() {
        let _serial = GLOBAL_SESSION_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // THE LAST GENUINELY UNTESTED LOGIC IN THIS FILE, and the mutation sweep is what proved it:
        // every other test builds `CouchState` by hand, so `start()`'s own wiring was never exercised.
        // Deleting `reviewers: tokens` from its struct literal — which the sweep does — leaves a server
        // that issues URLs nobody can use: every reviewer gets 401, and not one test noticed. Likewise
        // `is_running()`, the fence a DB restore consults before touching the library, could be
        // hardcoded either way undetected.
        //
        // Port 0 (ephemeral), NOT the production 8737. Binding 8737 here meant this gate could not run
        // while Couch Review was running — `cargo test` failed with "os error 10048" for a perfectly
        // healthy app, during exactly the daily use it exists to protect. Skipping when the port was
        // busy would have restored the blind spot; injecting the port keeps every assertion below and
        // removes the collision. `COUCH_PORT` itself stays pinned by
        // `the_session_constants_are_pinned_to_their_real_values`.
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("start-test.db").to_string_lossy().to_string();
        Database::open(&db_path).unwrap().initialize().unwrap();

        // An in-memory library is refused BEFORE any port is bound — the couch persists decisions, so a
        // database that evaporates on exit is not a thing it may serve.
        assert!(
            start_on_port(":memory:".to_string(), vec!["Sara".into()], 0, None).is_err(),
            "an in-memory library must be refused"
        );
        assert!(!is_running(), "a refused start must leave nothing running");

        let status =
            match start_on_port(db_path.clone(), vec!["Sara".into(), "Hemn".into()], 0, Some(tmp.path().to_path_buf()))
            {
                Ok(s) => s,
                Err(e) => panic!("start failed: {e}"),
            };
        assert!(is_running(), "the restore fence must see a running server");
        assert_eq!(status.reviewers.len(), 2, "one entry per named reviewer");

        // The issued URL must actually WORK — this is what the deleted-field mutant breaks.
        let sara = status.reviewers.iter().find(|r| r.name == "Sara").expect("Sara was issued a link");
        let token = sara.url.split("#t=").nth(1).expect("the URL carries a fragment token").to_string();
        let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(10)).build();
        // Read the port back out of the URL the owner is actually handed. That doubles as an assertion
        // that the issued link carries the port that was really bound — with an ephemeral port, a URL
        // built from the REQUESTED port would say ":0" and reach nothing.
        let bound_port: u16 = sara
            .url
            .split(':')
            .nth(2)
            .and_then(|rest| rest.split('/').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or_else(|| panic!("the issued URL must carry a real port: {}", sara.url));
        assert_ne!(bound_port, 0, "an issued link must never advertise port 0");
        let url = format!("http://127.0.0.1:{bound_port}/api/queue?t={token}");
        let code = agent.get(&url).call().map(|r| r.status()).unwrap_or_else(|e| match e {
            ureq::Error::Status(c, _) => c,
            other => panic!("the issued link did not reach the server: {other}"),
        });
        assert_eq!(code, 200, "a token issued by start() must authenticate against the running server");

        // Two reviewers were named, so each must have a DISTINCT token — one shared credential would
        // erase the attribution the whole design rests on.
        let hemn = status.reviewers.iter().find(|r| r.name == "Hemn").expect("Hemn was issued a link");
        assert_ne!(sara.url, hemn.url, "reviewers must never share a link");

        let stopped = stop().expect("stop succeeds");
        assert!(!stopped.running && stopped.reviewers.is_empty(), "stop reports a stopped server");
        assert!(!is_running(), "and the fence agrees");
        // The token dies with the session: stopping revokes every link at once.
        let after = agent.get(&url).call();
        assert!(
            matches!(after, Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Transport(_))),
            "a stopped server must not keep honouring its tokens: {after:?}"
        );

        // AND THE PORT MUST ACTUALLY BE FREE. This is the half that was missing, and the bug it hides
        // is not subtle: Settings -> Stop -> Start failed with "os error 10048", and stayed broken
        // until the whole app was restarted. tiny_http's `Server::drop` wakes its parked accept thread
        // by connecting to its own listening address — which is 0.0.0.0, and on Windows connecting to
        // 0.0.0.0 fails outright, so the wake never lands and the socket is held indefinitely.
        // Measured before the fix: still LISTENing 120 s after stop() returned.
        //
        // Asserted by RE-STARTING rather than by inspecting the socket: restarting is the thing the
        // owner actually does, and it is what was broken.
        //
        // Re-bound to the SAME port on purpose. Asking for an ephemeral port again would succeed even
        // with the leak present — the OS would simply hand out a different one — so the assertion would
        // pass while the bug lived. It must be the port stop() was supposed to release.
        let restarted = start_on_port(db_path.clone(), vec!["Sara".into()], bound_port, Some(tmp.path().to_path_buf()))
            .expect("stop must leave the port free to start again");
        assert!(restarted.running, "a stop/start cycle must produce a working server, not a bind error");
        stop().expect("second stop succeeds");
    }

    #[test]
    fn a_spot_check_served_before_a_restart_is_still_scored_after_it() {
        // Phase 4 of docs/REMOTE_PUBLIC_LINKS_PLAN.md, and the direct consequence of durable links:
        // restarts are now ROUTINE, so "the served-set is in memory" became a data-loss window. A
        // check served before a restart used to become unanswerable after it — the submit fell into
        // the already-reviewed 409 (couch.rs guards a verified clip against overwrite, correctly),
        // the score was never recorded, and the reviewer was shown an error for doing exactly what
        // they were asked. The honesty measurement silently thinned with every restart.
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let (db, db_path) = test_db(tmp.path());
        gold_seg(&db, "trap1", "دەقی هەڵە", "دەقی ڕاست");
        db.insert_segment(&seg("work1", "کاری ڕاستەقینە")).unwrap();

        // Session 1: the batch serves the trap; the served-set is persisted with the session.
        let state1 = Arc::new(Mutex::new(CouchState {
            reviewers: HashMap::from([("tok-sara".to_string(), "Sara".to_string())]),
            session_store: Some((data_dir.clone(), db_path.clone())),
            ..CouchState::default()
        }));
        let served = queue_ids(&db, "Sara", &state1);
        assert!(served.contains(&"trap1".to_string()), "the trap was handed out in session 1");
        // The durable file must now know about it — save_session was called by the serve path. (The
        // reviewers map is saved alongside; simulate what start_on_port persists at startup too.)

        // THE RESTART: a brand-new state built the way start_on_port builds it — from the file.
        let (remembered, rehydrated) = load_session(&data_dir, &db_path).expect("session file readable");
        assert_eq!(remembered.get("tok-sara").map(String::as_str), Some("Sara"), "the token survived");
        let state2 = Arc::new(Mutex::new(CouchState {
            reviewers: remembered,
            spot_checks: rehydrated,
            session_store: Some((data_dir, db_path)),
            ..CouchState::default()
        }));

        // The answer arrives AFTER the restart.
        let body = serde_json::json!({ "id": "trap1", "action": "edit", "text": "دەقی ڕاست" });
        let (code, _, msg) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state2);
        assert_eq!(
            code,
            200,
            "a check served before the restart must be answerable after it, got {code}: {}",
            String::from_utf8_lossy(&msg)
        );
        let report = db.spot_check_report().unwrap();
        assert_eq!(report.len(), 1, "and SCORED — the whole point of serving it");
        assert_eq!(report[0].reviewer, "Sara");
        assert_eq!(report[0].noticed, 1, "she corrected the planted error");
        // The answer key itself is untouched, exactly as in the no-restart path.
        let key = db.get_segment_by_id("trap1").unwrap().unwrap();
        assert_eq!(key.verdict_transcript.as_deref(), Some("دەقی ڕاست"), "grading never rewrites the key");
    }

    #[test]
    fn a_link_survives_closing_the_app_but_not_pressing_stop() {
        let _serial = GLOBAL_SESSION_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // What the owner asked for: open the page from any device whenever, review, close it, come
        // back later — without returning to the desktop for a fresh URL. Regenerating every token on
        // every start made that impossible, and it is the single thing that stopped remote review
        // being usable on their own terms.
        //
        // The distinction that keeps it safe is between the two ways a session ends. Closing the app
        // is not a decision about access — nothing calls stop() on exit — so the session is
        // remembered. Pressing Stop IS that decision, and must still revoke everything.
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let db_path = tmp.path().join("links.db").to_string_lossy().to_string();
        Database::open(&db_path).unwrap().initialize().unwrap();

        let first = start_on_port(db_path.clone(), vec!["Sara".into(), "Hemn".into()], 0, Some(data_dir.clone()))
            .expect("first start");
        let sara_link = first.reviewers.iter().find(|r| r.name == "Sara").unwrap().url.clone();
        let hemn_link = first.reviewers.iter().find(|r| r.name == "Hemn").unwrap().url.clone();
        let token_of = |url: &str| url.split("#t=").nth(1).unwrap().to_string();

        // CLOSING THE APP: no stop() call, the process simply goes away. Drop the handle the way an
        // exit would, without touching the remembered session.
        if let Some(mut h) = COUCH.lock().unwrap_or_else(|p| p.into_inner()).take() {
            h.shutdown.store(true, Ordering::SeqCst);
            h.server.unblock();
            for join in h.joins.drain(..) {
                let _ = join.join();
            }
            let port = h.port;
            drop(h);
            release_listener(port);
        }

        // Reopening issues the SAME tokens, so the links already sent still work.
        let again = start_on_port(db_path.clone(), vec!["Sara".into(), "Hemn".into()], 0, Some(data_dir.clone()))
            .expect("second start");
        let sara_again = again.reviewers.iter().find(|r| r.name == "Sara").unwrap().url.clone();
        assert_eq!(
            token_of(&sara_again),
            token_of(&sara_link),
            "closing and reopening the app must not invalidate a link the owner already sent"
        );
        assert_ne!(token_of(&sara_again), token_of(&hemn_link), "reviewers must still hold DIFFERENT tokens");

        // PRESSING STOP is the revoke, and it must outlive a restart: the remembered session is gone,
        // so the next start issues genuinely new tokens.
        stop().expect("stop succeeds");
        assert!(!session_path(&data_dir).exists(), "Stop must delete the remembered session, not just the server");
        let after_stop = start_on_port(db_path, vec!["Sara".into()], 0, Some(data_dir)).expect("start after stop");
        let sara_new = after_stop.reviewers.iter().find(|r| r.name == "Sara").unwrap().url.clone();
        assert_ne!(
            token_of(&sara_new),
            token_of(&sara_link),
            "a link revoked with Stop must NOT come back to life on the next launch"
        );
        stop().expect("final stop");
    }

    #[test]
    fn revoking_one_reviewer_must_outlive_a_restart_the_way_stop_does() {
        let _serial = GLOBAL_SESSION_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // The reason revoke exists is a LOST PHONE. Dropping the token from the in-memory map denied
        // it instantly, but nothing wrote that to couch_session.json — and `resume()` rebuilds the
        // roster from exactly that file, re-issuing the REMEMBERED token for a remembered name. So the
        // revoked phone got its original working link back at the next launch, with no owner action
        // and nothing in the UI to say so. The watchdog relaunches this app unattended every five
        // minutes, so "the next launch" is not a hypothetical the owner controls.
        //
        // Worse than plainly broken, it was NONDETERMINISTIC: the only other save_session call sites
        // are start and the batch-serve path, and the latter fires only when some OTHER reviewer's
        // next batch happens to serve a brand-new spot check. The same owner action sometimes stuck
        // and sometimes did not.
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let db_path = tmp.path().join("revoke.db").to_string_lossy().to_string();
        Database::open(&db_path).unwrap().initialize().unwrap();

        let started = start_on_port(db_path.clone(), vec!["Sara".into(), "Hemn".into()], 0, Some(data_dir.clone()))
            .expect("start");
        let token_of = |url: &str| url.split("#t=").nth(1).unwrap().to_string();
        let sara_token = token_of(&started.reviewers.iter().find(|r| r.name == "Sara").unwrap().url);
        let hemn_token = token_of(&started.reviewers.iter().find(|r| r.name == "Hemn").unwrap().url);

        // Sara loses her phone; the owner revokes her from Settings.
        revoke("Sara").expect("revoke succeeds");

        // CLOSING THE APP — not Stop. The session is deliberately remembered here (that is the whole
        // point of durable links), which is precisely why the revoke has to be remembered with it.
        if let Some(mut h) = COUCH.lock().unwrap_or_else(|p| p.into_inner()).take() {
            h.shutdown.store(true, Ordering::SeqCst);
            h.server.unblock();
            for join in h.joins.drain(..) {
                let _ = join.join();
            }
            let port = h.port;
            drop(h);
            release_listener(port);
        }

        // The production restart path: lib.rs calls resume(), which derives the roster from the file.
        let resumed = resume_on_port(db_path, &data_dir, 0).expect("resume brings the session back");
        let names: Vec<&str> = resumed.reviewers.iter().map(|r| r.name.as_str()).collect();
        assert!(!names.contains(&"Sara"), "a revoked reviewer must not be resurrected by resume: {names:?}");
        assert!(names.contains(&"Hemn"), "revoking one reviewer must not disturb the others: {names:?}");

        let live: Vec<String> = resumed.reviewers.iter().map(|r| token_of(&r.url)).collect();
        assert!(
            !live.contains(&sara_token),
            "the REVOKED token itself must never be served again — the lost phone still holds it"
        );
        assert!(live.contains(&hemn_token), "an untouched reviewer's link must still work after a restart");

        // And the file on disk — the thing a fresh process actually reads — must agree.
        let raw = std::fs::read_to_string(session_path(&data_dir)).expect("session file exists");
        let saved: SavedSession = serde_json::from_str(&raw).expect("session file parses");
        let remembered: Vec<&String> = saved.reviewers.values().collect();
        assert!(!remembered.iter().any(|n| *n == "Sara"), "the revoked reviewer must be gone from disk too");
        assert_eq!(remembered.len(), 1, "exactly one reviewer should remain remembered");

        stop().expect("final stop");
    }

    #[test]
    fn release_listener_gives_up_in_bounded_time_when_the_port_never_frees() {
        // `stop_couch_review` is a SYNC #[tauri::command], so it runs on the UI thread — and
        // release_listener retries. The happy path returns on the first attempt (measured 6-28 ms in
        // a live stop/start cycle), but the failing path is the one that decides whether the window
        // freezes, and "it should be about a second" is not a measurement.
        //
        // A LIVE listener the whole time: the bind probe can never succeed, so every attempt is
        // spent. This is the true ceiling, not a typical case.
        let held = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind a port to hold");
        let port = held.local_addr().unwrap().port();

        let started = Instant::now();
        release_listener(port);
        let elapsed = started.elapsed();
        drop(held);

        // Bounded, and bounded by the constants rather than by luck.
        let ceiling = (LISTENER_RELEASE_CONNECT_TIMEOUT + LISTENER_RELEASE_DELAY) * LISTENER_RELEASE_ATTEMPTS
            + Duration::from_secs(1); // scheduling slack on a loaded CI box
        assert!(elapsed < ceiling, "release_listener must give up in bounded time, took {elapsed:?}");
        // And the ceiling itself must stay small enough to sit on the UI thread without the window
        // visibly hanging. If a future change raises the retry budget past this, the command has to
        // move off the main thread first (see ASYNC_SLOW_COMMANDS in
        // scripts/test_command_main_thread_policy.py).
        assert!(
            (LISTENER_RELEASE_CONNECT_TIMEOUT + LISTENER_RELEASE_DELAY) * LISTENER_RELEASE_ATTEMPTS
                <= Duration::from_millis(1500),
            "the retry budget is on the UI thread; keep it under 1.5s or make stop_couch_review async"
        );
    }

    #[test]
    fn the_session_constants_are_pinned_to_their_real_values() {
        // Surfaced by the cargo-mutants sweep after couch.rs entered its scope: both of these were
        // written as arithmetic (`256 * 1024`, `15 * 60`) and NOTHING pinned the result, so mutating
        // `*` to `+` survived. Neither is cosmetic — each wrong value breaks reviewing in a way that
        // would look like a mysterious bug rather than a bad constant:
        //
        //   MAX_BODY_BYTES 256*1024 -> 256+1024 = 1,280 bytes. Every correction longer than a couple
        //   of sentences would be refused as "body too large", on a page whose whole purpose is
        //   submitting corrections. Sorani is multi-byte UTF-8, so 1,280 bytes is far less text than
        //   it sounds.
        //
        //   LEASE_TTL 15*60 -> 15+60 = 75 seconds. Leases would expire mid-clip constantly: reviewers
        //   would lose held work to each other while merely listening, and the page's renewal
        //   heartbeat (every 4 minutes) would fire long after the lease it exists to protect had gone.
        assert_eq!(MAX_BODY_BYTES, 262_144, "the body cap must be 256 KiB");
        assert_eq!(LEASE_TTL, Duration::from_secs(900), "a lease must last 15 minutes");
        // Pinned because `start()` now takes the port as an argument so the start/stop test can use an
        // ephemeral one. That flexibility must not reach production: reviewers are sent a link with
        // this port in it, and a firewall rule was granted for it. `start()` is the only non-test
        // caller and passes exactly this.
        assert_eq!(COUCH_PORT, 8737, "the port reviewers are sent, and the one the firewall allows");
        assert!(ACCEPT_POLL <= Duration::from_millis(250), "the accept poll bounds shutdown latency");
        // The heartbeat interval baked into the page (4 min) must stay comfortably inside the TTL, or
        // an ACTIVE reviewer's clip lapses before the renewal that was meant to save it.
        assert!(
            Duration::from_secs(4 * 60) * 2 < LEASE_TTL,
            "the page's 4-minute renewal heartbeat must have room to retry before the lease expires"
        );
        // A realistic long correction must fit the cap with room to spare, measured in BYTES of real
        // Sorani text rather than in characters.
        let long_correction = "ئەمە ڕستەیەکی دووبارەکراوەی کوردییە بۆ تاقیکردنەوەی قەبارە. ".repeat(40);
        assert!(
            long_correction.len() < MAX_BODY_BYTES / 4,
            "a long Sorani correction ({} bytes) must sit far inside the cap",
            long_correction.len()
        );
    }

    #[test]
    fn reviewer_names_are_validated_before_they_reach_a_provenance_column() {
        // These names are written onto every row the person decides, so they must be plain, distinct
        // labels. Two tokens both writing "Sara" would read as ONE reviewer in the data while behaving as
        // two — exactly the ambiguity attribution exists to remove, so it is refused rather than merged.
        assert_eq!(normalize_reviewers(&[]).unwrap(), vec![DEFAULT_REVIEWER], "no names = the owner alone");
        assert_eq!(
            normalize_reviewers(&["  ".into(), String::new()]).unwrap(),
            vec![DEFAULT_REVIEWER],
            "blank entries (a trailing comma in the Settings field) are not reviewers"
        );
        assert_eq!(
            normalize_reviewers(&[" Sara ".into(), "Hemn".into()]).unwrap(),
            vec!["Sara".to_string(), "Hemn".to_string()],
            "names are trimmed and kept in the owner's order"
        );
        assert!(normalize_reviewers(&["Sara".into(), "sara".into()]).is_err(), "case-folded duplicates refused");
        assert!(normalize_reviewers(&["a\nb".into()]).is_err(), "control characters would corrupt an export");
        assert!(normalize_reviewers(&["x".repeat(MAX_REVIEWER_NAME + 1)]).is_err(), "over-long name refused");
        let too_many: Vec<String> = (0..=MAX_REVIEWERS).map(|i| format!("r{i}")).collect();
        assert!(normalize_reviewers(&too_many).is_err(), "more reviewers than threads are refused, not silently cut");
        // THE LIMITS THEMSELVES, not just limit+1. Only the over-limit cases were pinned, so a `>` that
        // slipped to `>=` would reject a perfectly legal 40-character name, or an eighth reviewer, and
        // every test would still pass. Both survived the mutation sweep until these two lines existed.
        assert!(
            normalize_reviewers(&["x".repeat(MAX_REVIEWER_NAME)]).is_ok(),
            "a name of exactly the maximum length is legal"
        );
        let exactly_max: Vec<String> = (0..MAX_REVIEWERS).map(|i| format!("r{i}")).collect();
        assert_eq!(
            normalize_reviewers(&exactly_max).unwrap().len(),
            MAX_REVIEWERS,
            "exactly MAX_REVIEWERS is allowed — the cap is the last accepted value, not the first refused"
        );
    }

    #[test]
    fn a_blank_or_placeholder_submit_is_refused_and_leaves_the_draft_untouched() {
        // DATA LOSS, and this project's most-repeated bug: a save path that treats "" as a successful
        // transcript and writes it over a good draft. It has been found and fixed twice elsewhere in
        // the codebase. The couch path had the guards but NOTHING pinned them, so a refactor could
        // delete either one and every test would still pass — which is precisely how it came back the
        // second time.
        //
        // The 400 is only half the property. The half that matters is that the stored row is UNCHANGED:
        // a rejection that still wrote would be the actual bug, and would look like a working guard
        // from the client's side.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _p) = test_db(tmp.path());
        db.insert_segment(&seg("d1", "دەقێکی باش کە نابێت لەدەست بچێت")).unwrap();
        let state = state();

        let refused = |text: &str, action: &str| {
            let body = serde_json::json!({ "id": "d1", "action": action, "text": text });
            api_decision(&db, body.to_string().as_bytes(), "Sara", &state).0
        };

        // Empty, and whitespace-only — the trim happens before the check, so a page that sends "   "
        // after the reviewer clears the box must be refused just as firmly as one that sends "".
        for (text, why) in
            [("", "an empty transcript"), ("   ", "whitespace only"), ("\n\t ", "newlines and tabs only")]
        {
            for action in ["accept", "edit"] {
                assert_eq!(refused(text, action), 400, "{why} must be refused on '{action}'");
            }
        }
        // A placeholder draft is not a human answer either — verifying one would mint gold from a
        // string the ASR emitted to say it had nothing.
        for text in ["[Pending WSL 7B ASR]", "[ASR unavailable]"] {
            assert_eq!(refused(text, "accept"), 400, "a placeholder ({text}) must never be verifiable");
        }

        let after = db.get_segment_by_id("d1").unwrap().expect("the clip still exists");
        assert_eq!(after.raw_transcript, "دەقێکی باش کە نابێت لەدەست بچێت", "the good draft must survive");
        assert!(after.annotated_transcript.is_none(), "a refused submit must not write an annotation");
        assert!(!after.verified, "a refused submit must not mark the clip reviewed");
        assert!(after.human_decision.is_none(), "a refused submit must not record a decision");
    }

    #[test]
    fn undo_restores_a_pre_decision_jury_verdict_instead_of_clearing_it() {
        // DATA-LOSS (whole-row-clobber family): a jury-ESCALATED clip (verdict='escalated', escalated=true,
        // verified=false, human_decision=NULL) is served by the couch queue (get_segments(Some(false)) =
        // unverified). Phone-reviewing it then UNDOING must restore the pre-decision row EXACTLY — including
        // the jury verdict and the gold flag. The old undo ran clear_human_decision (nulls verdict/
        // rationale/evidence/agent_confidence) + insert_segment (a 17-col subset that OMITS every jury/
        // decision column AND is_gold), so undo CLEARED the jury's escalation verdict rather than restoring
        // it, and left is_gold=1 that the now-undone accept had set. The restore must go through
        // insert_segment_full so the row returns to its pre-decision snapshot losslessly.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _p) = test_db(tmp.path());

        // Persist the jury columns with insert_segment_full (insert_segment would drop them).
        let mut s = seg("esc1", "دەق یەک");
        s.verdict = Some("escalated".into());
        s.escalated = true;
        s.verified = false;
        s.is_gold = false;
        db.insert_segment_full(&s).unwrap();

        let state = state();
        let body = serde_json::json!({ "id": "esc1", "action": "accept", "text": "دەق یەک" });
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "the accept decision must succeed");
        assert!(db.get_segment_by_id("esc1").unwrap().unwrap().verified, "accept verifies the clip");

        let (code, _, _) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200, "undo must succeed");

        let restored = db.get_segment_by_id("esc1").unwrap().unwrap();
        assert_eq!(
            restored.verdict.as_deref(),
            Some("escalated"),
            "undo must RESTORE the pre-decision jury verdict, not clear it (got {:?})",
            restored.verdict
        );
        assert!(
            !restored.is_gold,
            "undo must not leave the gold flag the undone accept set (is_gold={})",
            restored.is_gold
        );
        assert!(!restored.verified, "undo reopens the clip for re-review");
        assert!(restored.human_decision.is_none(), "undo clears the human decision it recorded");
    }

    #[test]
    fn decision_rejects_placeholder_empty_unknown_and_bad_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق")).unwrap();
        let state = state();

        for (body, expect) in [
            (serde_json::json!({"id": "s1", "action": "edit", "text": ""}), 400u16),
            (serde_json::json!({"id": "s1", "action": "accept", "text": "[Pending WSL 7B ASR]"}), 400),
            (serde_json::json!({"id": "s1", "action": "explode", "text": "x"}), 400),
            (serde_json::json!({"id": "../etc", "action": "accept", "text": "x"}), 400),
            (serde_json::json!({"id": "missing", "action": "accept", "text": "x"}), 404),
        ] {
            let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, expect, "body: {body}");
        }
        assert!(lock_state(&state).undo.values().all(Vec::is_empty), "no failed decision may leave an undo entry");

        // Both brackets are required TOGETHER. With `||` in that guard, any transcript merely ENDING
        // in ']' — or merely STARTING with '[' — would be refused as a placeholder, silently blocking
        // legitimate text. Only "[...]" on both sides is a placeholder, so this half-bracketed text
        // must be ACCEPTED. (A rejects-only loop could never have caught this: the bug refuses MORE.)
        db.insert_segment(&seg("s2", "دەق")).unwrap();
        let half = serde_json::json!({"id": "s2", "action": "accept", "text": "[ناتەواو"}).to_string();
        assert_eq!(
            api_decision(&db, half.as_bytes(), "Sara", &state).0,
            200,
            "text with only ONE bracket is ordinary text, not a placeholder"
        );
        assert!(!db.get_segment_by_id("s1").unwrap().unwrap().verified, "row untouched by rejected requests");
    }

    #[test]
    fn the_undo_stack_is_bounded_and_keeps_the_newest_not_the_oldest() {
        // A long session used to retain one FULL row snapshot per decision, forever, in a process the
        // watchdog keeps alive for weeks across many sessions. Bounding it is only safe if the cap
        // discards the OLDEST — the ↩ button always reaches for the most recent decision, so trimming
        // the wrong end would break the one thing undo is for.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let total = UNDO_DEPTH + 7;
        for n in 0..total {
            db.insert_segment(&seg(&format!("u{n:03}"), "دەق")).unwrap();
        }
        let state = state();
        for n in 0..total {
            let body = serde_json::json!({ "id": format!("u{n:03}"), "action": "accept", "text": "دەق" });
            let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 200, "decision u{n:03} must land");
        }

        let depth = lock_state(&state).undo.get("Sara").map(|s| s.len()).unwrap_or(0);
        assert_eq!(depth, UNDO_DEPTH, "the stack must stop growing at the cap, not follow the session");

        // The MOST RECENT decision is still undoable — the cap trimmed the far end.
        let (code, _, body) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        let undone: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(undone["id"], format!("u{:03}", total - 1), "undo must retract the LAST decision");
        assert!(!db.get_segment_by_id(&format!("u{:03}", total - 1)).unwrap().unwrap().verified);

        // ...and the oldest, dropped by the cap, is simply beyond reach rather than corrupting anything.
        assert!(
            db.get_segment_by_id("u000").unwrap().unwrap().verified,
            "a trimmed-away entry leaves its decision intact; it is just no longer undoable"
        );
    }

    #[test]
    fn a_mid_session_server_restart_loses_no_work_and_double_decides_nothing() {
        // THE WATCHDOG CAN RESTART THIS APP AT ANY 5-MINUTE TICK — that is its job, and after the
        // probe/kill-loop fixes it will do so whenever the port is genuinely unreachable. So a real
        // session WILL sometimes be interrupted mid-batch, and the reviewer must not lose a decision or
        // have one recorded twice. Existing tests cover a restart between sessions (links survive, spot
        // checks stay scorable); none covers a restart DURING one, with a phone that keeps working from
        // the queue it already holds.
        //
        // Modelled honestly: threads down, server dropped, a FRESH CouchState on a new port against the
        // SAME database — so in-memory leases and undo are lost exactly as a process restart loses them,
        // while the corpus persists.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        const CLIPS: usize = 40;
        for n in 0..CLIPS {
            db.insert_segment(&seg(&format!("r{n:03}"), "دەقی سەرەتایی")).unwrap();
        }
        drop(db);

        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
        let mut decided: Vec<String> = Vec::new();

        // Boot a server, hand out ONE batch, and decide only part of it — the reviewer is mid-batch.
        let boot = |db_path: String| {
            let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
            let port = server.server_addr().to_ip().unwrap().port();
            let state = Arc::new(Mutex::new(CouchState {
                reviewers: [("tok".to_string(), "Hawzhin".to_string())].into_iter().collect(),
                ..CouchState::default()
            }));
            let shutdown = Arc::new(AtomicBool::new(false));
            let join = spawn_server_loop(0, server.clone(), db_path, state.clone(), shutdown.clone()).expect("spawn");
            (server, port, shutdown, join)
        };
        let submit = |port: u16, id: &str| -> u16 {
            let body = serde_json::json!({ "id": id, "action": "accept", "text": "دەقی سەرەتایی" }).to_string();
            match agent
                .post(&format!("http://127.0.0.1:{port}/api/decision?t=tok"))
                .set("content-type", "application/json")
                .send_string(&body)
            {
                Ok(r) => r.status(),
                Err(ureq::Error::Status(c, _)) => c,
                Err(e) => panic!("transport: {e}"),
            }
        };

        let (server, port, shutdown, join) = boot(db_path.clone());
        let queue: serde_json::Value =
            agent.get(&format!("http://127.0.0.1:{port}/api/queue?t=tok")).call().unwrap().into_json().unwrap();
        let held: Vec<String> =
            queue["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect();
        assert!(held.len() >= 10, "need a real batch to interrupt, got {}", held.len());
        for id in held.iter().take(5) {
            assert_eq!(submit(port, id), 200, "pre-restart decision {id} must land");
            decided.push(id.clone());
        }

        // THE RESTART, mid-batch, with the phone still holding the rest of its queue.
        shutdown.store(true, Ordering::SeqCst);
        server.unblock();
        let _ = join.join();
        drop(server);

        let (server2, port2, shutdown2, join2) = boot(db_path.clone());
        // The phone does NOT refetch — it works through the queue it already had, which is exactly what
        // a page mid-batch does. Every remaining clip must still be accepted.
        for id in held.iter().skip(5) {
            let code = submit(port2, id);
            assert_eq!(code, 200, "post-restart decision {id} must still land, got {code}");
            decided.push(id.clone());
        }
        // And a REPLAY of a pre-restart decision must be recognised as already-done, not recorded twice
        // — the retry guard reads the row, not process memory, so it has to survive the restart.
        let replay = submit(port2, &decided[0]);
        assert_eq!(replay, 200, "an outbox replay across a restart must be answered as already-done");

        shutdown2.store(true, Ordering::SeqCst);
        server2.unblock();
        let _ = join2.join();

        let db = Database::open(&db_path).unwrap();
        for id in &decided {
            let row = db.get_segment_by_id(id).unwrap().unwrap();
            assert!(row.verified, "{id} lost its decision across the restart");
            assert_eq!(row.reviewed_by.as_deref(), Some("Hawzhin"), "{id} lost its attribution");
        }
        // Exactly one learning pair per decided clip — the replay must not have minted a second.
        let side = rusqlite::Connection::open(&db_path).unwrap();
        let pairs: i64 = side
            .query_row(
                "SELECT COUNT(*) FROM agent_examples WHERE segment_id = ?1",
                rusqlite::params![&decided[0]],
                |r| r.get(0),
            )
            .unwrap();
        assert!(pairs <= 1, "the replayed decision minted {pairs} learning pairs; a retry must mint none");
    }

    #[test]
    fn one_reviewer_can_drain_a_backlog_larger_than_a_batch_to_genuine_zero() {
        // THE SHAPE OF A REAL SESSION, and nothing covered it. Every other soak caps its rounds (the
        // three-reviewer test stops at 4) or works a backlog smaller than QUEUE_BATCH, so "the queue
        // actually reaches zero" was assumed, never proven. The owner's library holds 116 pending —
        // five batches — and the two defects that shaped this surface both lived past the first batch:
        // the page never refetched (so a reviewer was told the corpus was finished at clip 25), and the
        // whole batch was leased on ONE timestamp so its tail expired underneath them mid-session.
        //
        // 130 clips over real HTTP against real threads and a real SQLite file, driven to genuine
        // exhaustion with an unbounded round loop, asserting the invariants a long session depends on:
        // every clip decided EXACTLY once, nothing served twice, nothing stranded, and the server
        // eventually answering with an empty queue of its own accord.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        const CLIPS: usize = 130;
        for n in 0..CLIPS {
            db.insert_segment(&seg(&format!("d{n:03}"), "دەقی سەرەتایی")).unwrap();
        }
        drop(db);

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: [("tok-solo".to_string(), "Hawzhin".to_string())].into_iter().collect(),
            ..CouchState::default()
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path.clone(), state.clone(), shutdown.clone())
            .expect("server thread spawns");

        let base = format!("http://127.0.0.1:{port}");
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
        let mut served: Vec<String> = Vec::new();
        let mut rounds = 0usize;
        let mut throttled = 0usize;
        // UNBOUNDED on purpose — a capped loop cannot tell "drained" from "gave up". The guard is a
        // generous ceiling that only trips if the server stops making progress, which is itself the bug.
        loop {
            rounds += 1;
            assert!(rounds < 100, "queue never drained after {rounds} rounds — {} clips served", served.len());
            // The limiter is keyed by REVIEWER and covers every endpoint, so the batch fetch is
            // throttled by the same bucket the submits drain — back off here too, or the drain dies
            // between batches instead of during one.
            let queue: serde_json::Value = loop {
                match agent.get(&format!("{base}/api/queue?t=tok-solo")).call() {
                    Ok(r) => break r.into_json().unwrap(),
                    Err(ureq::Error::Status(429, _)) => {
                        throttled += 1;
                        std::thread::sleep(Duration::from_millis(600));
                    }
                    Err(e) => panic!("queue fetch failed mid-session: {e}"),
                }
            };
            let items = queue["items"].as_array().cloned().unwrap_or_default();
            if items.is_empty() {
                break; // the server itself says there is nothing left
            }
            for item in items {
                let id = item["id"].as_str().unwrap().to_string();
                let body = serde_json::json!({ "id": id, "action": "accept", "text": item["text"] }).to_string();
                // THROTTLING IS PART OF THE TEST, not an obstacle to it. A machine-speed drain burns
                // the couch limiter's 60-token burst and then rides its 120/min refill, which is
                // exactly what a phone in a reload loop does — and the first run of this soak hit 429
                // at clip 73. A 429 means LATER, never NO, so the correct behaviour (client and test
                // alike) is to wait and re-submit the same decision, and the drain must still finish.
                let mut code = 0;
                for attempt in 0..40 {
                    code = match agent
                        .post(&format!("{base}/api/decision?t=tok-solo"))
                        .set("content-type", "application/json")
                        .send_string(&body)
                    {
                        Ok(r) => r.status(),
                        Err(ureq::Error::Status(c, _)) => c,
                        Err(e) => panic!("transport failure mid-session: {e}"),
                    };
                    if code != 429 {
                        break;
                    }
                    throttled += 1;
                    let _ = attempt;
                    std::thread::sleep(Duration::from_millis(600)); // ~1 token back at 120/min
                }
                assert_eq!(code, 200, "every submit in a solo session must eventually land; {id} got {code}");
                served.push(id);
            }
        }

        shutdown.store(true, Ordering::SeqCst);
        server.unblock();
        let _ = join.join();

        // Spot checks are served ALONGSIDE real work and are already-verified clips, so `served` may
        // exceed CLIPS — but no clip may appear twice, which is what a re-served or re-leased clip
        // would look like.
        eprintln!("[drain] {} clips over {rounds} rounds, {throttled} throttled retries", served.len());
        let unique: HashSet<&String> = served.iter().collect();
        assert_eq!(unique.len(), served.len(), "a clip was handed out twice across {rounds} rounds");
        assert!(rounds > CLIPS / QUEUE_BATCH, "expected multiple batches, only took {rounds} round(s)");

        // THE POINT: every pending clip is now decided. Not "most", not "the first batch".
        let db = Database::open(&db_path).unwrap();
        let undecided: Vec<String> =
            db.get_segments(Some(false)).unwrap().into_iter().filter(|s| s.id.starts_with('d')).map(|s| s.id).collect();
        assert!(undecided.is_empty(), "{} clip(s) stranded after a full drain: {undecided:?}", undecided.len());
        let decided = (0..CLIPS)
            .filter(|n| db.get_segment_by_id(&format!("d{n:03}")).unwrap().map(|s| s.verified).unwrap_or(false))
            .count();
        assert_eq!(decided, CLIPS, "every clip in the backlog must end verified");
    }

    #[test]
    fn three_reviewers_hammering_at_once_never_double_decide_or_lose_a_clip() {
        // P2.6 SOAK. Every other couch test drives one function at a time; this one runs THREE real
        // reviewers concurrently over real HTTP against real threads and a real SQLite file, and
        // checks the invariants that actually matter when the app is handed to a team:
        //
        //   * no clip is ever decided by two different people,
        //   * no clip is silently lost (everything handed out is accounted for),
        //   * a LOST RESPONSE cannot inflate the corpus. Every decision is submitted TWICE — the
        //     realistic phone failure, where the write lands and the reply does not — so the retry
        //     path is exercised under genuine concurrency rather than in isolation.
        //
        // This is the shape of test that would have caught the late-submit overwrite and the shared
        // undo stack without anyone having to think of them first.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        const CLIPS: usize = 60;
        for n in 0..CLIPS {
            db.insert_segment(&seg(&format!("s{n:03}"), "دەقی سەرەتایی")).unwrap();
        }
        drop(db);

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let people = [("tok-a", "Sara"), ("tok-b", "Hemn"), ("tok-c", "Ali")];
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: people.iter().map(|(t, n)| (t.to_string(), n.to_string())).collect(),
            ..CouchState::default()
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let joins: Vec<_> = (0..people.len())
            .map(|i| {
                spawn_server_loop(i, server.clone(), db_path.clone(), state.clone(), shutdown.clone())
                    .expect("server thread spawns")
            })
            .collect();

        // One client thread per reviewer, all working the queue at the same time.
        let clients: Vec<_> = people
            .iter()
            .map(|(token, _)| {
                let (token, base) = (token.to_string(), format!("http://127.0.0.1:{port}"));
                std::thread::spawn(move || {
                    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20)).build();
                    let (mut accepted, mut throttled) = (0usize, 0usize);
                    for _round in 0..4 {
                        let queue: serde_json::Value =
                            match agent.get(&format!("{base}/api/queue?t={token}")).call() {
                                Ok(r) => r.into_json().unwrap(),
                                Err(_) => continue,
                            };
                        let items = queue["items"].as_array().cloned().unwrap_or_default();
                        if items.is_empty() {
                            break;
                        }
                        for item in items {
                            let body = serde_json::json!({
                                "id": item["id"], "action": "edit", "text": format!("{} ✓", item["text"].as_str().unwrap_or("x")),
                            })
                            .to_string();
                            // Returns (ok, throttled) rather than ureq's Result: clippy flags a
                            // closure whose Err variant is 272 bytes, and the two flags are all this
                            // loop needs from the response.
                            let post = || match agent
                                .post(&format!("{base}/api/decision?t={token}"))
                                .send_string(&body)
                            {
                                Ok(r) => (r.status() == 200, false),
                                Err(ureq::Error::Status(429, _)) => (false, true),
                                Err(_) => (false, false),
                            };
                            let (ok, limited) = post();
                            accepted += usize::from(ok);
                            throttled += usize::from(limited);
                            // The lost-response retry. Must be absorbed, never counted twice.
                            throttled += usize::from(post().1);
                        }
                    }
                    (accepted, throttled)
                })
            })
            .collect();
        let results: Vec<(usize, usize)> = clients.into_iter().map(|c| c.join().expect("client thread")).collect();
        let accepted: usize = results.iter().map(|(a, _)| a).sum();
        let throttled: usize = results.iter().map(|(_, t)| t).sum();

        shutdown.store(true, Ordering::SeqCst);
        server.unblock();
        for join in joins {
            join.join().expect("every server thread joins cleanly under load");
        }

        let db = Database::open(&db_path).unwrap();
        let decided = db.get_segments(Some(true)).unwrap();
        assert!(decided.len() >= CLIPS / 2, "three reviewers should get through most of it, got {}", decided.len());
        // Every 200 the clients OBSERVED corresponds to a real decided clip: no phantom successes.
        // The REVERSE is deliberately not asserted. A first attempt that fails transiently under
        // three-way contention, followed by a retry that lands, leaves a decided clip the client never
        // counted — which is the retry path doing its job, not a lost decision. Requiring equality
        // would make this test fail on the very behaviour it exists to prove.
        assert!(
            accepted <= decided.len(),
            "clients saw {accepted} successes but only {} clips are decided — a success was phantom",
            decided.len()
        );
        // Measured, not assumed: the throttle does NOT fire here, because leasing partitions 60 clips
        // across three reviewers so none of them approaches the 120/min budget. That is a real
        // property worth knowing — normal team review does not brush the rate limit — and the limiter
        // itself is proven by `a_runaway_page_is_throttled_per_reviewer` rather than duplicated here.
        assert_eq!(throttled, 0, "ordinary concurrent reviewing must not be rate limited");

        for s in &decided {
            let who = s.reviewed_by.as_deref().unwrap_or("");
            assert!(
                people.iter().any(|(_, n)| *n == who),
                "clip {} is attributed to '{who}', which is not one of the reviewers",
                s.id
            );
            // THE DOUBLE-DECIDE CHECK. A retry or a late submit that slipped through would show up as
            // a second learning pair on a clip only one human ever corrected.
            let pairs: i64 = db
                .connection()
                .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = ?1", [&s.id], |r| r.get(0))
                .unwrap();
            assert_eq!(
                pairs, 1,
                "clip {} has {pairs} learning pairs — a retry was recorded as a second decision",
                s.id
            );
        }
    }

    #[test]
    fn live_server_gates_every_route_on_the_token_and_keeps_two_reviewers_apart() {
        // REAL end-to-end: boot the server on an OS-assigned port with TWO reviewers and TWO accept
        // threads, and hit it over loopback HTTP. Every HTTP call carries explicit timeouts so a wedged
        // server/socket FAILS the test with the offending call named, instead of hanging the suite.
        //
        // Beyond the token gate this pins the multi-reviewer contract over the wire: the token decides
        // WHICH reviewer you are, the two are handed different clips, and a decision is attributed in the
        // real database file. It also proves multi-threaded shutdown terminates — with several threads
        // parked in the accept loop, the join() deadlock this server hit live would come back as a hung
        // test rather than a silent one.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەقی تاقیکردنەوە")).unwrap();
        db.insert_segment(&seg("s2", "دەقی دووەم")).unwrap();
        drop(db); // the server opens its own connections

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: HashMap::from([
                ("saratoken123".to_string(), "Sara".to_string()),
                ("hemntoken456".to_string(), "Hemn".to_string()),
            ]),
            ..CouchState::default()
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let joins: Vec<_> = (0..2)
            .map(|i| {
                spawn_server_loop(i, server.clone(), db_path.clone(), state.clone(), shutdown.clone())
                    .expect("test server thread spawns")
            })
            .collect();

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let base = format!("http://127.0.0.1:{port}");
        let status_of_url = |url: String| -> u16 {
            agent.get(&url).call().map(|r| r.status()).unwrap_or_else(|e| match e {
                ureq::Error::Status(c, _) => c,
                other => panic!("{url} transport error (timeout/hang?): {other}"),
            })
        };
        // No token -> 401 on every DATA route. The page shell itself is deliberately public since
        // phase 2 (fragment links): it embeds nothing, and gating it would force the token back into
        // a server-visible part of the URL. The dedicated shell/claim test pins that side.
        for path in ["/api/queue", "/api/audio/s1"] {
            assert_eq!(status_of_url(format!("{base}{path}")), 401, "{path} must be token-gated");
        }
        assert_eq!(status_of_url(format!("{base}/")), 200, "the empty shell is public by design");
        // An unrecognised token has no reviewer, so it has no way in either.
        assert_eq!(status_of_url(format!("{base}/api/queue?t=notatoken")), 401, "an unknown token resolves to nobody");

        // With a token: the page serves, and the queue identifies WHICH reviewer it answered.
        assert_eq!(status_of_url(format!("{base}/?t=saratoken123")), 200);
        let sara: serde_json::Value =
            agent.get(&format!("{base}/api/queue?t=saratoken123")).call().unwrap().into_json().unwrap();
        assert_eq!(sara["reviewer"], "Sara", "the token, not the request, decides the identity");
        let sara_ids: Vec<&str> = sara["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(!sara_ids.is_empty());

        let hemn: serde_json::Value =
            agent.get(&format!("{base}/api/queue?t=hemntoken456")).call().unwrap().into_json().unwrap();
        assert_eq!(hemn["reviewer"], "Hemn");
        let hemn_ids: Vec<&str> = hemn["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        for id in &sara_ids {
            assert!(!hemn_ids.contains(id), "clip {id} was served to both reviewers over real HTTP");
        }

        // A phone decision over real HTTP verifies the row in the real db file, under the right name.
        let mine = sara_ids[0];
        let resp = agent
            .post(&format!("{base}/api/decision?t=saratoken123"))
            .send_string(&serde_json::json!({"id": mine, "action": "accept", "text": "دەقی تاقیکردنەوە"}).to_string())
            .unwrap();
        assert_eq!(resp.status(), 200);
        let db = Database::open(tmp.path().join("couch-test.db").to_string_lossy().as_ref()).unwrap();
        let row = db.get_segment_by_id(mine).unwrap().unwrap();
        assert!(row.verified);
        assert_eq!(row.reviewed_by.as_deref(), Some("Sara"), "the decision is attributed in the real database");

        // THE ROUTING TABLE ITSELF. Every other test calls api_renew / api_undo / api_audio directly,
        // so deleting their match arms — or flipping the /api/audio/ guard — changed nothing any test
        // could observe: the handlers stayed correct while becoming unreachable. Mutation testing
        // found exactly that. Each route is asserted to be WIRED, by a status only its own handler
        // can produce.
        let post = |path: &str, body: &str| -> u16 {
            agent.post(&format!("{base}{path}?t=saratoken123")).send_string(body).map(|r| r.status()).unwrap_or_else(
                |e| match e {
                    ureq::Error::Status(c, _) => c,
                    other => panic!("{path} transport error: {other}"),
                },
            )
        };
        // /api/renew reaches api_renew: a bad id is its 400, a missing route would be 404.
        assert_eq!(post("/api/renew", &serde_json::json!({"id": "../etc"}).to_string()), 400, "/api/renew is wired");
        // /api/undo reaches api_undo: Sara has one decision above, so this is its 200.
        assert_eq!(post("/api/undo", ""), 200, "/api/undo is wired");
        // /api/audio/<id> reaches api_audio: the id is unknown to the db, which is its 404 — NOT the
        // router's 404, which a deleted guard would give for a well-formed id it never matched.
        // P3.8: the page response plants an HttpOnly cookie, and the token then works from the
        // COOKIE ALONE — which is what lets the page strip `?t=` out of the visible URL and out of
        // browser history. Proven end to end rather than asserted: no query param, cookie only.
        let page = agent.get(&format!("{base}/?t=saratoken123")).call().unwrap();
        let cookie = page.header("Set-Cookie").unwrap_or_default().to_string();
        assert!(cookie.starts_with("cortex_couch=saratoken123"), "the page must plant the session cookie: {cookie}");
        assert!(cookie.contains("HttpOnly"), "the token must be unreadable from page JavaScript: {cookie}");
        assert!(cookie.contains("SameSite=Strict"), "the cookie must not ride cross-site requests: {cookie}");
        let by_cookie = agent
            .get(&format!("{base}/api/queue"))
            .set("Cookie", "cortex_couch=saratoken123")
            .call()
            .expect("the cookie alone must authenticate");
        assert_eq!(by_cookie.status(), 200, "no ?t= in the URL, and the request still works");
        // A wrong cookie is still nobody.
        let bad = agent.get(&format!("{base}/api/queue")).set("Cookie", "cortex_couch=nope").call();
        assert!(matches!(bad, Err(ureq::Error::Status(401, _))), "an unknown cookie token is unauthorized");

        // A BAD id gives api_audio's own 400 — a deleted guard would give the router's 404 instead,
        // and asserting only "404 for an unknown id" cannot tell those two apart (it did not).
        assert_eq!(
            status_of_url(format!("{base}/api/audio/..%2Fetc?t=saratoken123")),
            400,
            "/api/audio is wired — this 400 comes from its id validation, not from the router"
        );
        // And an unrelated GET must still reach the router's 404, not be swallowed by a widened guard.
        assert_eq!(status_of_url(format!("{base}/nonsense?t=saratoken123")), 404, "unknown paths stay 404");
        // And the body cap rejects an over-sized payload with ITS OWN message, so a truncating `take`
        // (which would leave a short body to fail later as bad json) is distinguishable from a refusal.
        let huge = "x".repeat(MAX_BODY_BYTES + 4096);
        let resp = agent.post(&format!("{base}/api/decision?t=saratoken123")).send_string(&huge);
        match resp {
            Err(ureq::Error::Status(400, r)) => assert!(
                r.into_string().unwrap_or_default().contains("body too large"),
                "an over-cap body must be REFUSED, not silently truncated into a parse error"
            ),
            other => panic!("expected 400 body-too-large, got {other:?}"),
        }

        shutdown.store(true, Ordering::SeqCst);
        server.unblock(); // the fix under test: unblock() (not a bare connect) ends the accept loops
        for join in joins {
            join.join().expect("every server thread joins cleanly after unblock()");
        }
    }
}
