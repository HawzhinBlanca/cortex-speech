//! couch.rs — "Couch Review": a TLS-protected, paired phone review page served by the desktop app.
//!
//! The owner starts it from Settings, opens the shown URL on a phone on the SAME Wi-Fi, and reviews
//! clips (listen → correct → save) from the couch. Decisions land in the same database through the
//! same human-decision transaction the desktop review uses, with phone finalization committed in that
//! transaction, so a phone review cannot leave a half-written row.
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
//! Transport is TLS on every interface. A locally generated certificate is persisted with its private
//! key protected by Windows DPAPI; Settings exposes its SHA-256 fingerprint for first-device pairing.
//! Reviewer links carry a pairing secret in the URL fragment, exchange it for a distinct Secure,
//! HttpOnly session cookie, and never send the pairing secret in a URL or data request.

use crate::db::{Database, ReviewDecisionLimit, SpeechSegment, REVIEW_PILOT_LIMIT_REACHED};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The single global server handle (at most one couch session at a time).
static COUCH: Mutex<Option<CouchHandle>> = Mutex::new(None);
/// Serializes snapshot+write as one ordered persistence operation. Atomic rename prevents torn JSON,
/// but without ordering an older request could snapshot before a revoke, finish after it, and restore
/// the revoked credential on restart.
static SESSION_PERSIST: Mutex<()> = Mutex::new(());
/// Orders per-reviewer revocation against requests that already copied a valid identity out of the
/// session map. Protected requests take a read guard only after their bounded body has been read and
/// keep it through their data access/commit; the server revalidates and takes a second read guard
/// through the synchronous network response. Revoke takes the write guard before deleting tokens.
/// Therefore revoke may wait for an already-authorized request/response to finish, but once it
/// RETURNS no earlier slow request can disclose audio or commit under the revoked identity.
static REVIEW_AUTHORIZATION: RwLock<()> = RwLock::new(());
/// Serializes the pre-receipt pilot check with the final database transaction inside this process.
/// SQLite's IMMEDIATE transaction is the cross-connection authority; this outer lock additionally
/// prevents an over-cap request from minting a playback receipt while another reviewer commits the
/// final slot.
static PILOT_DECISION_COMMIT: Mutex<()> = Mutex::new(());

/// Fixed default port — memorable, and stable across restarts so the phone bookmark keeps working.
const COUCH_PORT: u16 = 8737;
/// Cap on request bodies (a decision payload is < 1 KB; 256 KB leaves room for very long transcripts).
const MAX_BODY_BYTES: usize = 256 * 1024;
/// The credential probe accepts one tiny JSON object and nothing else. Keeping its own bound far
/// below the general decision-body cap makes the release gate cheap even when it is presented with
/// a hostile body, and prevents a read-only health endpoint from becoming an allocation surface.
const MAX_CLAIM_PROBE_BODY_BYTES: usize = 1024;
/// Pairing tokens generated by this server are 64 ASCII bytes. This deliberately wider compatibility
/// ceiling admits previously issued durable links without allowing an attacker to make the lookup
/// retain an arbitrarily large string.
const MAX_CLAIM_PROBE_TOKEN_BYTES: usize = 512;
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
const TLS_IDENTITY_FILE: &str = "couch_tls_identity.json";
const COUCH_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How stale a session's persisted issue time may get before a page load rewrites it. Small next to
/// `COUCH_SESSION_TTL`, so the durable expiry tracks the in-memory one closely, and large enough that
/// a reloading phone does not re-encrypt every session on every request (review 2026-08-20).
const RENEWAL_PERSIST_AFTER: Duration = Duration::from_secs(15 * 60);
const MAX_SESSIONS_PER_REVIEWER: usize = 8;

/// Per-reviewer request budget (P1.6). Every desktop IPC command is throttled; these HTTP routes were
/// the one unthrottled way into the database, so a phone stuck in a reload loop could hammer it
/// unbounded.
///
/// The numbers are 120 requests per SECOND sustained, with a burst of 60. Read the refill in
/// throttle.rs before changing them: `tokens += elapsed.as_secs_f64() * rate`, so `rate` is per second
/// — this comment claimed "120/min" for a long time and was wrong by 60x. What that buys is honest but
/// narrower than the old comment implied: it is far above any human (a reviewer averages a handful of
/// requests per clip) and it does NOT throttle a merely chatty page, but it does bound a runaway loop
/// running at machine speed. Measured: the solo-drain soak below outruns it and starts seeing 429 at
/// clip 73, which is exactly the behaviour a phone stuck reloading would get.
///
/// Keyed by REVIEWER, so one person's misbehaving tab cannot starve anyone else. Applied only AFTER
/// the token resolves: an unauthenticated request is rejected before it touches the database, and the
/// 244-bit token space puts guessing out of reach, so throttling that path would buy nothing.
static COUCH_RATE_LIMITER: crate::throttle::GlobalRateLimiter =
    crate::throttle::GlobalRateLimiter::new_with_burst(120, 60);

/// One undoable phone decision: the pre-decision snapshot to restore, plus the row's
/// post-decision monotonic revision — the staleness fence `api_undo` compares atomically while
/// restoring (text-provenance audit #2/#3: the old fence keyed only on `reviewed_by`, which no
/// non-decision writer touches, so a batch normalize / desktop edit / champion re-draft between
/// decision and undo was silently reverted to the snapshot).
struct UndoEntry {
    /// Identifies THIS in-flight submit. Two requests from the same reviewer can overlap; removing
    /// `stack.pop()` on one failure could otherwise discard the other request's successful undo entry.
    operation_id: String,
    seg_id: String,
    prev: SpeechSegment,
    /// `None` until the decision's final row write lands: an entry with no revision is a decision
    /// whose write never completed, and restoring over an unverifiable state is not a safe inverse.
    decided_revision: Option<i64>,
}

/// Per-session state shared by the accept threads. One mutex: the critical sections are map lookups,
/// held for microseconds, while the slow work (DB writes, WAV encoding) happens outside it.
#[derive(Default)]
struct CouchState {
    /// Pre-decision row snapshots, keyed by reviewer name — so undo is per-person and can never
    /// retract someone else's work.
    undo: HashMap<String, Vec<UndoEntry>>,
    /// Client operation UUIDs currently crossing the normal-decision transaction. This closes the
    /// in-process preflight/INSERT race for two tabs replaying the same outbox item and, critically,
    /// ensures they share one provisional undo inverse rather than racing it away on one failure.
    in_flight_operations: HashSet<String>,
    /// segment id -> (reviewer who holds it, when it was granted). Entries older than [`LEASE_TTL`]
    /// are ignored and pruned, so a lease needs no explicit release on disconnect.
    leases: HashMap<String, (String, Instant)>,
    /// Work ids that `/api/queue` actually returned, keyed to the reviewer who saw them.
    ///
    /// A lease alone is not proof of delivery: `/api/renew` can reclaim an expired lease for a page
    /// that still has the clip open. Keeping the delivery receipt separate means renew can preserve
    /// that reliability property without becoming a way for an authenticated reviewer to claim an
    /// arbitrary known id and then fetch its biometric audio. Unlike hidden-check receipts below,
    /// these are session-local because work leases themselves are session-local.
    served_work: HashSet<(String, String)>,
    /// token -> reviewer name. The token is the credential; the name is what lands in the database.
    ///
    /// Lives HERE, in the state every accept thread shares, rather than in a snapshot handed to each
    /// thread at spawn. With per-thread `Arc<HashMap>` copies, revoking a reviewer would have removed
    /// them from the owner's view while every serving thread kept honouring the token forever — a
    /// revoke that revokes nothing. One shared map means removal takes effect on the next request.
    reviewers: HashMap<String, String>,
    /// Pairing secret -> reviewer. Production links expose only these secrets (in the fragment); a
    /// successful claim mints a different random session token into `reviewers`. Kept separate so a
    /// copied link is not itself the long-lived cookie used on every audio/data request.
    pairing_codes: HashMap<String, String>,
    /// Session token -> issue time. Production sessions are bounded and expire even if a client never
    /// revisits. Legacy unit fixtures omit this metadata and remain valid only inside their test state.
    ///
    /// WALL CLOCK, not `Instant`, since 2026-08-20: an `Instant` is meaningless across a process
    /// restart, and these sessions now SURVIVE one (see `SavedCookieSession`). A backwards clock jump
    /// makes `duration_since` fail, and every such site treats the failure as "not expired" — the
    /// available direction. A clock skew must never log eight reviewers out.
    session_issued: HashMap<String, SystemTime>,
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
    /// Subset of `spot_checks` first served under the bound controlled-pilot policy. It caps hidden
    /// checks at the mathematically required ceil(10/8)=2 per reviewer across reloads/restarts.
    pilot_spot_checks: HashSet<(String, String)>,
    /// Where to re-save the remembered session when the served-set grows: (data_dir, db_path).
    /// None outside a durable session (tests, no data dir) — then nothing is persisted, as before.
    session_store: Option<(PathBuf, String)>,
    /// Deterministic failure injection for the pilot durability regression. Filesystem permission
    /// tricks are not portable on Windows and previously let the supposedly failing save succeed.
    #[cfg(test)]
    fail_session_persist: bool,
    /// Deterministic, token-selective DPAPI failure injection. A per-state seam keeps parallel tests
    /// isolated; production has no corresponding switch and always calls Windows DPAPI directly.
    #[cfg(test)]
    fail_cookie_protection_for: Option<String>,
    /// reviewer -> the segment ids they explicitly SKIPPED: "I cannot judge this one" (R4.4).
    ///
    /// Kept so the queue stops handing a reviewer work they have already declined. Without it the
    /// button is a treadmill: the clip is still pending, so the very next refill serves it straight
    /// back, and the only way past it remains a verdict the reviewer cannot stand behind.
    ///
    /// ponytail: memory only, deliberately. A restart offers the clip once more, which costs one
    /// repeated clip — unlike `spot_checks`, where losing an entry MIS-SCORES a reviewer, and which is
    /// why that one is persisted and this is not. Never pruned either: it is bounded by the pending
    /// backlog, and the count reported to the page is derived from the live pending rows rather than
    /// from this map's size, so a stale entry cannot inflate anything.
    skipped: HashMap<String, HashSet<String>>,
    /// The exact operating policy this session was started under. A file edit never hot-resets a
    /// paid counter: any mismatch pauses requests until the owner explicitly stops and restarts.
    pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
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

    #[cfg(test)]
    fn inject_session_persist_failure(&self) -> bool {
        self.fail_session_persist
    }

    #[cfg(not(test))]
    fn inject_session_persist_failure(&self) -> bool {
        false
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
    /// SHA-256 over the certificate DER, shown on the trusted desktop for first-device verification.
    certificate_fingerprint: String,
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
    /// The same page over Tailscale Funnel — a PUBLIC https://<host>.ts.net address that works on any
    /// network with no Tailscale client on the reviewer's device.
    ///
    /// The other two links die the moment a reviewer leaves the network: the LAN URL needs the same
    /// Wi-Fi, and the tailnet URL needs the tailnet up on their phone — which iOS suspends whenever
    /// the VPN app is backgrounded. Measured 2026-08-19: a reviewer mid-session lost audio and had
    /// five skips silently fail to reach the server, while the server, the files and the Funnel were
    /// all healthy. The only link that survived was the one the app never handed out.
    ///
    /// None unless Tailscale itself reports a Funnel serving THIS port. A guessed hostname would be
    /// a dead link, which is worse than no link — so this is read from `tailscale serve status`,
    /// never constructed. The transport is public; the couch token gate still applies to every route.
    pub funnel_url: Option<String>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CouchStatus {
    pub running: bool,
    /// One entry per named reviewer, each with its own token/URL. Empty when stopped.
    pub reviewers: Vec<CouchReviewer>,
    /// Colon-separated SHA-256 fingerprint of the self-signed TLS certificate.
    pub certificate_fingerprint: Option<String>,
}

impl CouchStatus {
    fn stopped() -> Self {
        CouchStatus { running: false, reviewers: Vec::new(), certificate_fingerprint: None }
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
/// Durable deny marker written by Stop before the server is torn down. If deleting a stale session
/// file fails (permissions/AV/disk fault), startup still refuses to resurrect its credentials.
const SESSION_REVOCATION_FILE: &str = "couch_session.revoked";

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedTlsIdentity {
    certificate_pem: String,
    protected_private_key_pem: String,
    subject_alt_names: Vec<String>,
}

struct TlsIdentity {
    certificate_pem: Vec<u8>,
    private_key_pem: Vec<u8>,
    fingerprint: String,
}

fn tls_subject_alt_names() -> Vec<String> {
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string(), lan_ip()];
    if let Ok(host) = std::env::var("COMPUTERNAME") {
        let host = host.trim();
        if !host.is_empty() {
            names.push(host.to_string());
            names.push(format!("{host}.local"));
        }
    }
    if let Some(ip) = tailscale_ip() {
        names.push(ip);
    }
    names.sort();
    names.dedup();
    names
}

fn certificate_der_from_pem(pem: &str) -> Result<Vec<u8>, String> {
    let encoded: String = pem.lines().filter(|line| !line.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| format!("decode Couch TLS certificate PEM: {e}"))
}

fn certificate_fingerprint(pem: &str) -> Result<String, String> {
    let digest = Sha256::digest(certificate_der_from_pem(pem)?);
    Ok(digest.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(":"))
}

fn generate_tls_identity(subject_alt_names: &[String]) -> Result<TlsIdentity, String> {
    let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(subject_alt_names.to_vec())
        .map_err(|e| format!("generate Couch TLS certificate: {e}"))?;
    let certificate_pem = cert.pem();
    let private_key_pem = signing_key.serialize_pem();
    let fingerprint = certificate_fingerprint(&certificate_pem)?;
    Ok(TlsIdentity {
        certificate_pem: certificate_pem.into_bytes(),
        private_key_pem: private_key_pem.into_bytes(),
        fingerprint,
    })
}

fn save_tls_identity(data_dir: &Path, identity: &TlsIdentity, subject_alt_names: &[String]) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("create Couch TLS identity directory: {e}"))?;
    let private_key = String::from_utf8(identity.private_key_pem.clone())
        .map_err(|_| "generated Couch TLS private key was not UTF-8 PEM".to_string())?;
    let saved = SavedTlsIdentity {
        certificate_pem: String::from_utf8(identity.certificate_pem.clone())
            .map_err(|_| "generated Couch TLS certificate was not UTF-8 PEM".to_string())?,
        protected_private_key_pem: crate::dpapi::protect(&private_key)?,
        subject_alt_names: subject_alt_names.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&saved).map_err(|e| format!("serialize Couch TLS identity: {e}"))?;
    let final_path = data_dir.join(TLS_IDENTITY_FILE);
    let temp_path = data_dir.join(format!("{TLS_IDENTITY_FILE}.tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temp_path, bytes).map_err(|e| format!("write Couch TLS identity staging file: {e}"))?;
    crate::atomic_file::replace_file(&temp_path, &final_path)
        .map_err(|e| format!("replace Couch TLS identity atomically: {e}"))
}

fn load_tls_identity(data_dir: &Path, required_names: &[String]) -> Result<Option<TlsIdentity>, String> {
    let path = data_dir.join(TLS_IDENTITY_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read Couch TLS identity {}: {error}", path.display())),
    };
    let saved: SavedTlsIdentity =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse Couch TLS identity {}: {e}", path.display()))?;
    if required_names.iter().any(|name| !saved.subject_alt_names.contains(name)) {
        return Ok(None);
    }
    let private_key_pem = crate::dpapi::unprotect(&saved.protected_private_key_pem)?;
    let fingerprint = certificate_fingerprint(&saved.certificate_pem)?;
    Ok(Some(TlsIdentity {
        certificate_pem: saved.certificate_pem.into_bytes(),
        private_key_pem: private_key_pem.into_bytes(),
        fingerprint,
    }))
}

fn load_or_create_tls_identity(data_dir: Option<&Path>) -> Result<TlsIdentity, String> {
    let names = tls_subject_alt_names();
    if let Some(dir) = data_dir {
        if let Some(identity) = load_tls_identity(dir, &names)? {
            return Ok(identity);
        }
    }
    let identity = generate_tls_identity(&names)?;
    if let Some(dir) = data_dir {
        save_tls_identity(dir, &identity, &names)?;
    }
    Ok(identity)
}

/// One remembered session: the reviewer names and the tokens their links carry.
#[derive(serde::Serialize, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
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
    #[serde(default)]
    pilot_spot_checks: Vec<(String, String)>,
    /// Live COOKIE sessions, protected at rest exactly like the pairing tokens above.
    ///
    /// Added 2026-08-20 after measuring why six of eight paid reviewers went silent. The pairing
    /// tokens were durable (that is what this whole file is for), but the session credential layered
    /// on top of them lived only in memory — so every app restart made the server forget every cookie
    /// it had issued, while the browser went on holding a perfectly valid one for its full 24 h
    /// `Max-Age`. The app restarted 4–9 times a DAY (48 times in the nine days measured). Each restart
    /// answered every reviewer's next request `401`, and the page turns a settled 401 into the
    /// terminal "link expired" — with no Retry, because an expired link is genuinely unrecoverable.
    ///
    /// It was not expired. The server had amnesia, and the reviewer had no way to know the difference:
    /// the page deliberately strips `#t=` from the address bar after claiming, so whatever they
    /// bookmarked (or installed to their home screen, whose `start_url` is `/`) carries no token to
    /// re-claim with. One restart turned a working shortcut into a dead end, and the only way back was
    /// to find the original chat message from days earlier.
    ///
    /// `serde(default)` so a session file written before this field still loads.
    #[serde(default)]
    sessions: Vec<SavedCookieSession>,
    /// Bound policy survives an app/watchdog restart, so changing a baseline cannot silently reset a
    /// live paid pilot. Older unrestricted sessions deserialize as `None`.
    #[serde(default)]
    pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
}

/// One remembered cookie session. Same protection as a pairing token: it is a credential.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedCookieSession {
    /// DPAPI-protected session token — the cookie value the browser will present.
    token: String,
    reviewer: String,
    /// Seconds since the epoch. Wall clock because an `Instant` cannot cross a restart, which is the
    /// entire point of writing this down. Restoring compares it against `COUCH_SESSION_TTL`, so a
    /// session that expired while the app was closed stays expired instead of being resurrected.
    issued_unix: u64,
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

fn session_revocation_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SESSION_REVOCATION_FILE)
}

/// Establish the durable revocation fact before reporting Stop success or shutting down the live
/// server. A failure leaves the live handle registered, so the owner gets an honest error instead of
/// a false "stopped" state whose old links return on restart.
fn write_session_revocation(data_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| format!("create Couch Review state directory {}: {e}", data_dir.display()))?;
    let path = session_revocation_path(data_dir);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => return Ok(()),
        Ok(_) => return Err(format!("Couch Review revocation marker {} is not a file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("inspect Couch Review revocation marker {}: {e}", path.display())),
    }
    let tmp = data_dir.join(format!("{SESSION_REVOCATION_FILE}.tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, b"revoked\n").and_then(|()| crate::atomic_file::replace_file(&tmp, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("persist Couch Review revocation marker {}: {e}", path.display())
    })
}

fn clear_session_revocation(data_dir: &Path) -> Result<(), String> {
    let path = session_revocation_path(data_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove Couch Review revocation marker {}: {e}", path.display())),
    }
}

/// Returns `Err` when the complete credential generation could not be written. Credential-minting,
/// renewal, initial Start, and hidden-key serving paths must propagate that failure before publishing
/// success; a memory-only green response is precisely the restart-amnesia state this file prevents.
/// `revoke` also surfaces the error because a revoke that lives only in memory is silently undone by
/// the next restart.
/// The map that holds the durable LINK tokens for this state.
///
/// Normally `pairing_codes`. A state where that map is empty but `reviewers` holds tokens is the
/// pre-pairing-code shape, and there the session tokens ARE the links — writing an empty map there
/// would hand `save_session` a `reviewers: {}` whose partial-write refusal passes vacuously (0 == 0),
/// and `load_session` then returns `None` for the whole file: every reviewer link dead at the next
/// restart. One definition, because this used to live only at the spot-check save site while the
/// newer per-claim save had no idea it was needed (review 2026-08-20).
fn durable_pairing_codes(st: &CouchState) -> HashMap<String, String> {
    if st.pairing_codes.is_empty() {
        st.reviewers.clone()
    } else {
        st.pairing_codes.clone()
    }
}

struct SessionSaveSnapshot {
    dir: PathBuf,
    db_path: String,
    reviewers: HashMap<String, String>,
    spot_checks: HashSet<(String, String)>,
    pilot_spot_checks: HashSet<(String, String)>,
    sessions: HashMap<String, (String, SystemTime)>,
    pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
    #[cfg(test)]
    fail_cookie_protection_for: Option<String>,
}

struct SessionSaveInput<'a> {
    data_dir: &'a Path,
    reviewers: &'a HashMap<String, String>,
    db_path: &'a str,
    spot_checks: &'a HashSet<(String, String)>,
    pilot_spot_checks: &'a HashSet<(String, String)>,
    sessions: &'a HashMap<String, (String, SystemTime)>,
    pilot_policy: Option<&'a crate::review_pilot::ReviewPilotPolicy>,
}

fn snapshot_session_save(st: &CouchState) -> Option<SessionSaveSnapshot> {
    st.session_store.clone().map(|(dir, db_path)| SessionSaveSnapshot {
        dir,
        db_path,
        reviewers: durable_pairing_codes(st),
        spot_checks: st.spot_checks.clone(),
        pilot_spot_checks: st.pilot_spot_checks.clone(),
        sessions: snapshot_live_sessions(st),
        pilot_policy: st.pilot_policy.clone(),
        #[cfg(test)]
        fail_cookie_protection_for: st.fail_cookie_protection_for.clone(),
    })
}

fn save_session_snapshot(snapshot: &SessionSaveSnapshot) -> Result<(), String> {
    #[cfg(test)]
    if let Some(failed_token) = snapshot.fail_cookie_protection_for.as_deref() {
        return save_session_snapshot_with_cookie_protector(snapshot, |token| {
            if token == failed_token {
                Err("injected cookie protection failure".to_string())
            } else {
                crate::dpapi::protect(token)
            }
        });
    }
    save_session_snapshot_with_cookie_protector(snapshot, crate::dpapi::protect)
}

fn save_session_snapshot_with_cookie_protector<F>(
    snapshot: &SessionSaveSnapshot,
    protect_cookie: F,
) -> Result<(), String>
where
    F: Fn(&str) -> Result<String, String>,
{
    save_session_with_cookie_protector(
        SessionSaveInput {
            data_dir: &snapshot.dir,
            reviewers: &snapshot.reviewers,
            db_path: &snapshot.db_path,
            spot_checks: &snapshot.spot_checks,
            pilot_spot_checks: &snapshot.pilot_spot_checks,
            sessions: &snapshot.sessions,
            pilot_policy: snapshot.pilot_policy.as_ref(),
        },
        protect_cookie,
    )
}

/// Snapshot the session state and write it OUTSIDE the lock.
///
/// Called wherever a cookie session is minted or renewed. Without this a session created after the
/// last save existed only in memory — which is exactly the amnesia this whole mechanism exists to
/// end, just with a smaller window. DPAPI + file IO must never run inside the request-path mutex, so
/// the snapshot is taken under the lock and the write happens after it is dropped.
///
/// Failure is part of the caller's commit boundary: a claim, renewal, Start, or newly registered
/// hidden-check batch cannot be reported successful until this ordered durable generation succeeds.
fn lock_session_persist() -> std::sync::MutexGuard<'static, ()> {
    SESSION_PERSIST.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn persist_session_state(state: &Mutex<CouchState>) -> Result<(), String> {
    persist_session_state_with_hook(state, || {})
}

fn persist_session_state_with_hook<F: FnOnce()>(state: &Mutex<CouchState>, after_snapshot: F) -> Result<(), String> {
    // Acquire BEFORE snapshotting. Locking only around save_session still lets an old snapshot wait
    // behind a newer revoke and then overwrite it after the revoke completes.
    let _persist = lock_session_persist();
    let snapshot = { snapshot_session_save(&lock_state(state)) };
    after_snapshot();
    if let Some(snapshot) = snapshot {
        if let Err(e) = save_session_snapshot(&snapshot) {
            tracing::warn!(
                "Couch Review session not remembered ({e}) — a restart will ask this device to re-open its link"
            );
            return Err(e);
        }
    }
    Ok(())
}

/// The live cookie sessions, in the shape `save_session` persists.
///
/// One place builds this, so a new save site cannot accidentally write the pairing map into the
/// session field or forget the issue times that make expiry meaningful.
fn snapshot_live_sessions(st: &CouchState) -> HashMap<String, (String, SystemTime)> {
    snapshot_sessions_from_maps(&st.reviewers, &st.session_issued)
}

fn snapshot_sessions_from_maps(
    reviewers: &HashMap<String, String>,
    session_issued: &HashMap<String, SystemTime>,
) -> HashMap<String, (String, SystemTime)> {
    reviewers
        .iter()
        .filter_map(|(token, name)| session_issued.get(token).map(|at| (token.clone(), (name.clone(), *at))))
        .collect()
}

fn save_session(
    data_dir: &Path,
    reviewers: &HashMap<String, String>,
    db_path: &str,
    spot_checks: &HashSet<(String, String)>,
    pilot_spot_checks: &HashSet<(String, String)>,
    sessions: &HashMap<String, (String, SystemTime)>,
    pilot_policy: Option<&crate::review_pilot::ReviewPilotPolicy>,
) -> Result<(), String> {
    save_session_with_cookie_protector(
        SessionSaveInput { data_dir, reviewers, db_path, spot_checks, pilot_spot_checks, sessions, pilot_policy },
        crate::dpapi::protect,
    )
}

fn save_session_with_cookie_protector<F>(input: SessionSaveInput<'_>, protect_cookie: F) -> Result<(), String>
where
    F: Fn(&str) -> Result<String, String>,
{
    let SessionSaveInput { data_dir, reviewers, db_path, spot_checks, pilot_spot_checks, sessions, pilot_policy } =
        input;
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
    let now = SystemTime::now();
    let live_sessions = sessions
        .iter()
        .filter(|(_, (_, issued))| now.duration_since(*issued).unwrap_or_default() < COUCH_SESSION_TTL)
        .map(|(token, (reviewer, issued))| {
            let protected = protect_cookie(token)
                .map_err(|_| "a live cookie session could not be protected at rest".to_string())?;
            let issued_unix = issued
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "a live cookie session has an invalid issue time".to_string())?
                .as_secs();
            Ok(SavedCookieSession { token: protected, reviewer: reviewer.clone(), issued_unix })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let saved = SavedSession {
        reviewers: protected,
        db_path: db_path.to_string(),
        // Plaintext by design: the pair (segment id, reviewer name) is which clips were handed out
        // as checks — worth protecting from casual reading no more than the queue itself, and DPAPI
        // per entry would make every batch serve cost a round of CryptProtectData for no threat.
        spot_checks: spot_checks.iter().cloned().collect(),
        pilot_spot_checks: pilot_spot_checks.iter().cloned().collect(),
        // Expired sessions are intentionally omitted. Every live session is all-or-nothing: silently
        // dropping one after DPAPI or timestamp conversion failure makes memory green while a restart
        // forgets that reviewer's working cookie, so the whole generation must fail instead.
        sessions: live_sessions,
        pilot_policy: pilot_policy.cloned(),
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

fn clear_session(data_dir: &Path) -> Result<(), String> {
    let path = session_path(data_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            tracing::warn!("Couch Review session file not removed ({}): {e}", path.display());
            Err(format!("Couch Review stopped securely, but its stale session file could not be removed: {e}"))
        }
    }
}

/// What `load_session` recovers.
///
/// A NAMED struct, not a tuple: this grew a third member and every destructuring site would have
/// broken silently-or-loudly at once (it has cost this repo two rounds of that already).
#[derive(Default)]
struct RememberedSession {
    /// Pairing token -> reviewer name. The long-lived secret in the link the owner sends.
    pairing: HashMap<String, String>,
    /// Served-but-unanswered spot checks.
    spot_checks: HashSet<(String, String)>,
    /// Pilot checks already served, including answered ones; used to prevent reloads minting extras.
    pilot_spot_checks: HashSet<(String, String)>,
    /// Cookie session token -> (reviewer, issued). Already filtered to sessions still within
    /// `COUCH_SESSION_TTL`: restoring is resuming, never extending.
    sessions: HashMap<String, (String, SystemTime)>,
    /// The policy snapshot written when this durable session was started.
    pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
}

/// Read back a remembered session, or None if there is none / it is unreadable / it belongs to a
/// different library.
fn load_session(data_dir: &Path, db_path: &str) -> Option<RememberedSession> {
    // Stop writes this marker BEFORE teardown. It is authoritative even if the stale credential file
    // could not be deleted; unreadable marker metadata also fails closed.
    match std::fs::metadata(session_revocation_path(data_dir)) {
        Ok(_) => return None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!("Couch Review session not resumed: revocation state could not be read: {e}");
            return None;
        }
    }
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
    // Cookie sessions are restored INDIVIDUALLY and best-effort, the mirror of how they are saved: an
    // unreadable or expired one costs its holder a re-claim, never the whole resume. Expiry is judged
    // against the wall clock now, so time that passed while the app was closed still counts — a
    // restart resumes a session, it never grants it a fresh 24 hours.
    let now = SystemTime::now();
    let sessions: HashMap<String, (String, SystemTime)> = saved
        .sessions
        .into_iter()
        .filter_map(|entry| {
            let token = crate::dpapi::unprotect(&entry.token).ok()?;
            let issued = UNIX_EPOCH.checked_add(Duration::from_secs(entry.issued_unix))?;
            (now.duration_since(issued).unwrap_or_default() < COUCH_SESSION_TTL)
                .then_some((token, (entry.reviewer, issued)))
        })
        .collect();
    (!out.is_empty()).then_some(RememberedSession {
        pairing: out,
        spot_checks: saved.spot_checks.into_iter().collect(),
        pilot_spot_checks: saved.pilot_spot_checks.into_iter().collect(),
        sessions,
        pilot_policy: saved.pilot_policy,
    })
}

/// Read-only restore preflight: a durable Couch generation can retain the controlled-pilot baseline
/// even if its separate policy file is unexpectedly missing. A bare DB restore must not change the
/// review_events history underneath that remembered cap. Invalid/unreadable state is an error so the
/// caller can fail closed instead of guessing that it was unrestricted.
pub(crate) fn durable_controlled_pilot_state(data_dir: &Path) -> Result<bool, String> {
    match std::fs::metadata(session_revocation_path(data_dir)) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Couch session revocation state is unreadable: {error}")),
    }
    let path = session_path(data_dir);
    crate::atomic_file::recover_interrupted_replace(&path)
        .map_err(|error| format!("Couch session recovery failed: {error}"))?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Couch session state is unreadable: {error}")),
    };
    let saved: SavedSession =
        serde_json::from_str(&raw).map_err(|error| format!("Couch session state is invalid: {error}"))?;
    Ok(saved.pilot_policy.is_some() || !saved.pilot_spot_checks.is_empty())
}

/// The port [`start`] and [`resume`] bind: [`COUCH_PORT`], unless `CORTEX_COUCH_PORT` overrides it.
///
/// Same reason `start_on_port` exists, one layer further out. That was added so a Rust test could
/// drive `start()`'s real wiring without binding 8737 while the owner's own Couch Review holds it.
/// An END-TO-END harness has the identical problem and cannot reach `start_on_port` at all: it goes
/// through the `start_couch_review` IPC command, which calls this. Without the override, the only
/// way to test the review path over real HTTP is to shut down the owner's server first — so the
/// project's most reviewer-facing surface stayed the one with no end-to-end coverage.
///
/// Read at start time, not cached: an unparseable or 0 value falls back to the default rather than
/// failing, because an env var must never be able to stop the owner's phone link from coming up.
fn configured_port() -> u16 {
    port_from(std::env::var("CORTEX_COUCH_PORT").ok().as_deref())
}

/// The parsing half of [`configured_port`], split out so the FALLBACK can be tested. Reading the
/// variable is the untestable part (process-global, and mutating the environment mid-test is unsound
/// with other threads running); deciding what a bad value means is the part that matters, because
/// getting it wrong takes the owner's phone link down rather than merely ignoring a typo.
fn port_from(raw: Option<&str>) -> u16 {
    raw.and_then(|v| v.trim().parse::<u16>().ok()).filter(|p| *p != 0).unwrap_or(COUCH_PORT)
}

/// Start the couch server for `reviewers` (idempotent: returns the existing session if already running,
/// WITHOUT re-tokenizing — a running session's links must not be invalidated by a status refresh).
pub fn start(db_path: String, reviewers: Vec<String>, data_dir: Option<PathBuf>) -> Result<CouchStatus, String> {
    start_on_port(db_path, reviewers, configured_port(), data_dir)
}

/// Bring back the session the owner last started, if the app was closed without pressing Stop.
///
/// This is what makes the phone usable on the owner's terms: start Couch Review once, then open the
/// same link from any device whenever, close the app in between, and never come back to the desktop
/// for a fresh URL. Returns None when there is nothing remembered, which is the normal first-run and
/// post-Stop state.
pub fn resume(db_path: String, data_dir: &Path) -> Option<CouchStatus> {
    // Same port `start` would use, or the remembered link would come back on a different one.
    resume_on_port(db_path, data_dir, configured_port())
}

/// `resume` with the port injected, for the same reason `start_on_port` exists: the resurrection a
/// revoke has to survive happens HERE, in the roster rebuilt from the remembered file, so the test
/// must drive this exact path — and it cannot bind 8737 while the owner's own server is using it.
fn resume_on_port(db_path: String, data_dir: &Path, port: u16) -> Option<CouchStatus> {
    let remembered = load_session(data_dir, &db_path)?;
    let mut names: Vec<String> = remembered.pairing.values().cloned().collect();
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
    start_on_port_with_session_lifecycle(
        db_path,
        reviewers,
        port,
        data_dir,
        save_session_snapshot,
        clear_session_revocation,
    )
}

/// Exact lifecycle seam for the initial durable generation. Production always supplies
/// [`save_session_snapshot`]; a focused test supplies one deterministic error without process-global
/// flags or filesystem permission tricks that behave differently on Windows.
fn start_on_port_with_session_lifecycle<F, G>(
    db_path: String,
    reviewers: Vec<String>,
    port: u16,
    data_dir: Option<PathBuf>,
    initial_session_writer: F,
    clear_revocation: G,
) -> Result<CouchStatus, String>
where
    F: FnOnce(&SessionSaveSnapshot) -> Result<(), String>,
    G: FnOnce(&Path) -> Result<(), String>,
{
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
    let configured_pilot = match data_dir.as_deref() {
        Some(dir) => {
            crate::review_pilot::load(dir).map_err(|error| format!("controlled review pilot cannot start: {error}"))?
        }
        None => None,
    };
    if let Some(policy) = configured_pilot.as_ref() {
        if !policy.matches_session(&names) {
            return Err(format!(
                "controlled review pilot requires exactly these two reviewers: {}",
                policy.reviewer_names().join(", ")
            ));
        }
    }

    // One random token PER REVIEWER (2× UUIDv4 = 244 random bits each). Never logged — the token IS
    // the identity, so sharing one would erase the attribution.
    //
    // REUSED from the remembered session when the same person is still on the list, so a link the
    // owner already sent keeps working after the app is closed and reopened. A name that was not in
    // the saved session gets a fresh token, and a name that has been dropped simply stops existing.
    let remembered_session = data_dir.as_deref().and_then(|dir| load_session(dir, &db_path));
    if remembered_session.as_ref().is_some_and(|remembered| remembered.pilot_policy != configured_pilot) {
        return Err(
            "controlled review policy differs from the remembered session; press Stop, verify the policy, then start a fresh session"
                .to_string(),
        );
    }
    let remembered = remembered_session.unwrap_or_default();
    let served_checks = remembered.spot_checks.clone();
    let pilot_served_checks = remembered.pilot_spot_checks.clone();
    let tokens: HashMap<String, String> = names
        .iter()
        .map(|name| {
            let existing = remembered.pairing.iter().find(|(_, n)| n == &name).map(|(t, _)| t.clone());
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
    let preflight_db = Database::open(&db_path).map_err(|e| format!("Couch Review cannot open the library: {e}"))?;
    if let Some(policy) = configured_pilot.as_ref() {
        if policy.after_review_event_id > preflight_db.max_review_event_id().map_err(|error| error.to_string())? {
            return Err("controlled review pilot baseline is ahead of durable review history".to_string());
        }
        let limit = build_pilot_decision_limit(policy)?;
        preflight_db
            .review_decision_progress(&limit)
            .map_err(|error| format!("controlled review pilot history is invalid: {error}"))?;
    }
    drop(preflight_db);

    let tls_identity = load_or_create_tls_identity(data_dir.as_deref())?;
    let server = Arc::new(
        tiny_http::Server::https(
            ("0.0.0.0", port),
            tiny_http::SslConfig::load_from_mem(
                tls_identity.certificate_pem.clone(),
                tls_identity.private_key_pem.clone(),
            ),
        )
        .map_err(|e| format!("Couch TLS server could not bind port {port}: {e}"))?,
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    // Rehydrate the served spot-check set for names still on the roster, so a check outstanding
    // across the restart is recognized and scored when its answer arrives instead of 409ing.
    let spot_checks: HashSet<(String, String)> =
        served_checks.into_iter().filter(|(_, name)| names.contains(name)).collect();
    let pilot_spot_checks: HashSet<(String, String)> =
        pilot_served_checks.into_iter().filter(|(_, name)| names.contains(name)).collect();
    let session_store = data_dir.as_ref().map(|dir| (dir.clone(), db_path.clone()));
    // Cookie sessions come back with the server, for names still on the roster. Without this the
    // browser holds a valid cookie the server has never heard of, answers 401, and the page reports
    // "link expired" for a link that is fine — the amnesia that silenced six of eight reviewers.
    // Filtered by name so a reviewer REMOVED from the roster does not keep a working session.
    let restored: HashMap<String, (String, SystemTime)> =
        remembered.sessions.into_iter().filter(|(_, (name, _))| names.contains(name)).collect();
    let reviewers: HashMap<String, String> =
        restored.iter().map(|(token, (name, _))| (token.clone(), name.clone())).collect();
    let session_issued: HashMap<String, SystemTime> =
        restored.into_iter().map(|(token, (_, issued))| (token, issued)).collect();
    if !reviewers.is_empty() {
        tracing::info!("Couch Review restored {} live session(s) — saved shortcuts keep working", reviewers.len());
    }
    let state = Arc::new(Mutex::new(CouchState {
        pairing_codes: tokens,
        spot_checks,
        pilot_spot_checks,
        session_store,
        reviewers,
        session_issued,
        pilot_policy: configured_pilot,
        ..CouchState::default()
    }));
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
                let _ = server.unblock();
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
    let handle = CouchHandle {
        shutdown,
        server,
        port: bound,
        state,
        joins,
        data_dir: data_dir.clone(),
        certificate_fingerprint: tls_identity.fingerprint,
    };
    // Remember it only once the server is actually serving. Saving earlier would leave a session file
    // pointing at a start that failed, and the next launch would resume links to nothing.
    if let Some(dir) = data_dir.as_deref() {
        let _persist = lock_session_persist();
        let snapshot = {
            let state = lock_state(&handle.state);
            snapshot_session_save(&state)
        };
        let Some(snapshot) = snapshot else {
            shutdown_unpublished_handle(handle);
            return Err("Couch Review could not durably start: session storage is unavailable".to_string());
        };
        // This write is part of Start's commit, not advisory. Publishing links that exist only in
        // memory makes a successful-looking start lose every fresh cookie/link generation at the next
        // watchdog restart. Tear the unpublished server down before returning the error.
        if let Err(error) = initial_session_writer(&snapshot) {
            shutdown_unpublished_handle(handle);
            return Err(format!("Couch Review could not durably start: {error}"));
        }
        // A prior Stop marker is cleared ONLY after the fresh token map is durable; a failed write
        // returns above with the marker untouched, so stale pre-Stop credentials cannot revive.
        if let Err(error) = clear_revocation(dir) {
            shutdown_unpublished_handle(handle);
            return Err(format!(
                "Couch Review could not durably start: revocation marker could not be cleared ({error})"
            ));
        }
    }
    let status = status_of(&handle);
    *guard = Some(handle);
    tracing::info!(
        "Couch Review started with TLS on port {bound} for {} reviewer(s) (paired sessions; stop from Settings)",
        names.len()
    );
    Ok(status)
}

/// Dispose a server that bound successfully but never committed its durable starting state. It must
/// never enter `COUCH`, and the listener must be released synchronously so the next honest Start does
/// not inherit an orphaned socket.
fn shutdown_unpublished_handle(mut handle: CouchHandle) {
    handle.shutdown.store(true, Ordering::SeqCst);
    let _ = handle.server.unblock();
    for join in handle.joins.drain(..) {
        let _ = join.join();
    }
    let port = handle.port;
    drop(handle);
    release_listener(port);
}

/// Stop the couch server and drop every session token (revoking all reviewer links at once).
pub fn stop() -> Result<CouchStatus, String> {
    stop_with_data_dir(None)
}

/// Stop with an AppState-provided data directory. The fallback matters when the server is already
/// stopped: pressing Stop again must retry cleanup of a session file whose first deletion failed.
pub fn stop_with_data_dir(fallback_data_dir: Option<&Path>) -> Result<CouchStatus, String> {
    let mut guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    let durable_dir =
        guard.as_ref().and_then(|handle| handle.data_dir.clone()).or_else(|| fallback_data_dir.map(Path::to_path_buf));

    // Persist the deny fact BEFORE taking the only live handle. If this fails, leave the server
    // registered and running and return an error; we must never claim revocation that a restart can
    // silently undo.
    if let Some(dir) = durable_dir.as_deref() {
        write_session_revocation(dir)?;
    }

    let mut cleanup_error = None;
    if let Some(mut h) = guard.take() {
        h.shutdown.store(true, Ordering::SeqCst);
        // unblock() makes the accept loops stop yielding requests so the threads exit. (A bare TCP
        // connect does NOT wake tiny_http — it yields only complete HTTP requests — so without this,
        // join() deadlocked; caught by the live end-to-end test.) The loops additionally poll with
        // `recv_timeout`, so a thread unblock() fails to reach still exits within ACCEPT_POLL rather
        // than wedging stop() forever — the same deadlock class, now impossible for any thread count.
        let _ = h.server.unblock();
        for join in h.joins.drain(..) {
            let _ = join.join();
        }
        let port = h.port;
        // STOP IS THE REVOKE. Closing the app remembers the session so a link keeps working; pressing
        // Stop is the owner deciding it should not. Forgetting this line would make "stopping revokes
        // every link" a lie the moment the app reopened.
        if let Some(dir) = durable_dir.as_deref() {
            if let Err(e) = clear_session(dir) {
                // The revocation marker remains, so stale credentials cannot resume. Surface the
                // cleanup failure and let a second Stop retry it instead of swallowing it.
                cleanup_error = Some(e);
            }
        }
        // Drop the Server BEFORE waking it. Dropping is what sets tiny_http's internal close flag, so
        // the order decides whether the wake-up connection means "exit" or "serve me".
        drop(h);
        release_listener(port);
        tracing::info!("Couch Review stopped");
    } else if let Some(dir) = durable_dir.as_deref() {
        // Retry path after a previous Stop securely shut down but could not remove the stale file.
        if let Err(e) = clear_session(dir) {
            cleanup_error = Some(e);
        }
    }
    if let Some(error) = cleanup_error {
        Err(error)
    } else {
        Ok(CouchStatus::stopped())
    }
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
    // A request may have resolved its cookie before the owner pressed Revoke, then waited while its
    // body arrived. Serialize the actual revoke against the request's final live-session check and
    // whole protected operation; deleting the map entry alone cannot invalidate an already-copied
    // reviewer String.
    let _authorization = REVIEW_AUTHORIZATION.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Ordered before the state lock, matching persist_session_state. Every older snapshot must finish
    // before this mutation, and every later save must snapshot the already-revoked maps.
    let _persist = lock_session_persist();
    let mut st = lock_state(state);
    let roster = if st.pairing_codes.is_empty() { &st.reviewers } else { &st.pairing_codes };
    if !roster.values().any(|reviewer| reviewer.eq_ignore_ascii_case(name)) {
        return Err(format!("No reviewer named '{name}' in this session"));
    }
    let distinct_reviewers: HashSet<String> = roster.values().map(|reviewer| reviewer.to_lowercase()).collect();
    if distinct_reviewers.len() <= 1 {
        return Err("That is the only reviewer — stop Couch Review instead".to_string());
    }
    st.pairing_codes.retain(|_, reviewer| !reviewer.eq_ignore_ascii_case(name));
    st.reviewers.retain(|_, reviewer| !reviewer.eq_ignore_ascii_case(name));
    let live_sessions: HashSet<String> = st.reviewers.keys().cloned().collect();
    st.session_issued.retain(|token, _| live_sessions.contains(token));
    // Their held clips go back to the pool at once; leaving them leased would strand real work behind
    // someone who can no longer reach the server.
    st.leases.retain(|_, (who, _)| !who.eq_ignore_ascii_case(name));
    // Delivery receipts are authorization state, not review history. If the same display name is
    // later paired again, it must receive a fresh queue; retaining these would let the new session
    // renew an old id and reconstruct the revoked reviewer's object grant.
    st.served_work.retain(|(_, who)| !who.eq_ignore_ascii_case(name));

    // PERSIST, or say so. Dropping the token from the in-memory map denies the lost phone right now,
    // but `couch_session.json` is what `resume()` rebuilds the roster from — and `start_on_port`
    // deliberately RE-ISSUES the remembered token for a remembered name, so a memory-only revoke is
    // undone by the next launch, handing the revoked phone back its original working link. The
    // watchdog relaunches this app unattended every 5 minutes, so that restart is not hypothetical.
    //
    // Snapshot under the lock and write outside it, like the batch-serve path: save_session does DPAPI
    // plus file IO, which has no business inside the request-path mutex.
    let persist = st.session_store.clone().map(|(dir, db_path)| {
        (
            dir,
            db_path,
            st.pairing_codes.clone(),
            st.spot_checks.clone(),
            st.pilot_spot_checks.clone(),
            snapshot_live_sessions(&st),
            st.pilot_policy.clone(),
        )
    });
    drop(st);
    if let Some((dir, db_path, reviewers, checks, pilot_checks, sessions, pilot_policy)) = persist {
        // Access is already denied in memory at this point, so this is not a rollback — it is the
        // difference between "revoked" and "revoked until the next restart", and the owner revoking a
        // LOST PHONE has to be told which one they got.
        save_session(
            &dir,
            &reviewers,
            &db_path,
            &checks,
            &pilot_checks,
            &sessions,
            pilot_policy.as_ref(),
        )
        .map_err(|e| {
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
    let funnel = funnel_host(h.port);
    let state = lock_state(&h.state);
    let link_credentials = if state.pairing_codes.is_empty() { &state.reviewers } else { &state.pairing_codes };
    let mut reviewers: Vec<CouchReviewer> = link_credentials
        .iter()
        // `#t=`, not `?t=`: a fragment never leaves the browser, so a link pasted into a chat app
        // hands its preview bot the empty shell rather than a live credential. The page claims the
        // fragment via POST /api/claim into the HttpOnly cookie on first open.
        .map(|(token, name)| CouchReviewer {
            name: name.clone(),
            url: format!("https://{lan}:{}/#t={token}", h.port),
            tailscale_url: tailscale.as_ref().map(|ip| format!("https://{ip}:{}/#t={token}", h.port)),
            funnel_url: funnel.as_ref().map(|host| format!("https://{host}/#t={token}")),
        })
        .collect();
    // HashMap iteration order is randomized per process, so sort for a stable Settings list — otherwise
    // the URLs visibly reshuffle on every status poll and the owner cannot tell whose link is whose.
    reviewers.sort_by(|a, b| a.name.cmp(&b.name));
    CouchStatus { running: true, reviewers, certificate_fingerprint: Some(h.certificate_fingerprint.clone()) }
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

/// The public Funnel hostname serving THIS port, if Tailscale reports one.
///
/// Asks `tailscale serve status --json` rather than assembling a hostname, because a Funnel that is
/// configured-but-off, or pointed at a different port, would produce a URL that resolves and then
/// refuses — a dead link handed to a reviewer as if it were their way in. Three things must all hold:
/// a Web handler on this host, at path `/`, proxying to this exact port, AND `AllowFunnel` true for
/// it. Anything else returns None and the owner simply sees no Funnel row.
fn funnel_host(port: u16) -> Option<String> {
    // CACHED for the process lifetime, and the subprocess waits at most 3 seconds — because this is
    // called from status_of(), which runs UNDER the COUCH mutex. Unbounded, a wedged tailscale.exe
    // would freeze every status call and is_running() itself, which is the restore fence: the whole
    // app's DB-restore safety gated on an external binary answering. Timing out returns None (no
    // Funnel row this session — the LAN and tailnet links still show), and the leaked waiter thread
    // is one thread, once, only on a machine whose tailscale is already broken.
    static FUNNEL: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FUNNEL
        .get_or_init(|| {
            let (send, recv) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let candidates = ["tailscale", r"C:\Program Files\Tailscale\tailscale.exe"];
                let output = candidates.iter().find_map(|exe| {
                    std::process::Command::new(exe)
                        .args(["serve", "status", "--json"])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                });
                let _ = send.send(output);
            });
            let output = recv.recv_timeout(std::time::Duration::from_secs(3)).ok().flatten()?;
            funnel_host_from_status(&serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()?, port)
        })
        .clone()
}

/// The decision half of [`funnel_host`], split out so it can be tested without a tailnet.
fn funnel_host_from_status(parsed: &serde_json::Value, port: u16) -> Option<String> {
    let web = parsed.get("Web")?.as_object()?;
    let suffix = format!("://127.0.0.1:{port}");
    for (host_port, entry) in web {
        let proxies_here = entry
            .get("Handlers")
            .and_then(|h| h.get("/"))
            .and_then(|h| h.get("Proxy"))
            .and_then(|p| p.as_str())
            .is_some_and(|proxy| proxy.ends_with(&suffix));
        let funnel_on =
            parsed.get("AllowFunnel").and_then(|f| f.get(host_port)).and_then(|v| v.as_bool()).unwrap_or(false);
        if proxies_here && funnel_on {
            // "host:443" -> "host". Any other port would need to stay in the URL, and a Funnel only
            // ever serves 443/8443/10000, so a non-443 entry keeps its port rather than being guessed.
            return Some(match host_port.strip_suffix(":443") {
                Some(host) => host.to_string(),
                None => host_port.clone(),
            });
        }
    }
    None
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
                    let _ = server.unblock();
                    return;
                }
            };
            while !shutdown.load(Ordering::SeqCst) {
                // recv_timeout, not the blocking iterator: it lets EVERY thread observe `shutdown`
                // within ACCEPT_POLL even if unblock() only wakes one of them, so stop()/join() is
                // bounded no matter how many reviewers are running.
                let mut request = match server.recv_timeout(ACCEPT_POLL) {
                    Ok(Ok(Some(request))) => request,
                    // None = timed out (poll again) or unblocked (the shutdown check catches it).
                    Ok(Ok(None)) => continue,
                    Ok(Err(e)) => {
                        tracing::warn!("Couch Review request parse error: {e}");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("Couch Review accept error: {e}");
                        continue;
                    }
                };
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // Keep the response-delivery authorization separate from operation authorization.
                // `handle_request` reads bounded POST bodies before its commit guard so a slow upload
                // cannot hold Revoke hostage. Its reply is fully materialized in memory, but bytes are
                // not disclosed until `request.respond` below. Revalidate the cookie and hold the
                // read side through that synchronous send; otherwise Revoke could return in the gap
                // and an old handler could transmit already-materialized biometric audio afterward.
                let response_path = request.url().split('?').next().unwrap_or("").to_string();
                let delivery_requires_auth = response_path.starts_with("/api/")
                    && response_path != "/api/claim"
                    && response_path != "/api/claim/probe";
                let delivery_token = delivery_requires_auth.then(|| token_from_request(&request)).flatten();
                let (mut response, mut set_cookie) = handle_request(&mut request, &db, &state);
                let delivery_authorization =
                    delivery_token.as_deref().and_then(|token| live_session_guard(token, None, &state));
                if delivery_requires_auth && delivery_authorization.is_none() {
                    response = err_reply(401, "unauthorized");
                    set_cookie = None;
                }
                let _ = respond_with_cookie(request, response, set_cookie);
                drop(delivery_authorization);
            }
        })
        .map_err(|e| format!("Couch Review server thread failed to spawn: {e}"))
}

/// (status, content_type, body, extra response headers)
///
/// The fourth slot exists for `/api/audio` (phase R1). Every reply used to ship with nothing but a
/// Content-Type, which made caching and range requests impossible to express: a reviewer re-opening a
/// clip they had already played re-downloaded all 300-500 KB of it over cellular, and a browser asking
/// for a byte range got an unconditional full body back. Every other route passes `vec![]`.
type Reply = (u16, &'static str, Vec<u8>, Vec<(&'static str, String)>);

/// A reply that also plants the session cookie. Used only for the page itself: the token arrives in
/// the URL exactly once, and from then on the cookie carries it.
///
/// `HttpOnly` is the point — the SERVER sets it, so page JavaScript can never read the token back
/// out. `Secure` is mandatory because Couch Review now serves TLS on every interface;
/// `SameSite=Strict` keeps it off cross-site requests and a 24-hour sliding lifetime narrows exposure
/// from a lost phone without interrupting an active reviewer.
fn page_reply_with_cookie(token: &str, body: Vec<u8>) -> (Reply, Option<String>) {
    (
        (200, "text/html; charset=utf-8", body, vec![]),
        Some(format!("{COUCH_COOKIE}={token}; Path=/; Max-Age=86400; SameSite=Strict; Secure; HttpOnly")),
    )
}

/// The fragment bootstrap (docs/REMOTE_PUBLIC_LINKS_PLAN.md phase 2): the page reads `#t=` out of
/// `location.hash` — which the browser never transmits — and presents it here ONCE, in a POST body.
/// The reply moves it into the HttpOnly cookie and the page strips the fragment. From that moment the
/// token never appears in any URL, log line, or preview-bot fetch.
///
/// The pairing secret and session credential are deliberately distinct. Re-opening a pairing link may
/// mint a fresh session (preview bots never see fragments), but the secret itself never rides on audio
/// or decision requests and can be revoked independently of every already-issued cookie.
fn api_claim(body: &[u8], state: &Mutex<CouchState>) -> (Reply, Option<String>) {
    #[derive(serde::Deserialize)]
    struct ClaimBody {
        token: String,
    }
    let Ok(parsed) = serde_json::from_slice::<ClaimBody>(body) else {
        return (err_reply(400, "bad json"), None);
    };
    // Claim is a tiny two-phase transaction over memory and the durable session file. Taking the
    // persistence-order lock BEFORE the state snapshot keeps claims/revokes/saves in one generation:
    // nobody can persist a map between our provisional insertion and its commit/rollback.
    let _persist = lock_session_persist();
    let (reviewer, session_token, proposed_reviewers, proposed_session_issued, snapshot, inject_persist_failure) = {
        let guard = lock_state(state);
        let reviewer = guard.pairing_codes.get(&parsed.token).or_else(|| guard.reviewers.get(&parsed.token)).cloned();
        let Some(reviewer) = reviewer else {
            // Same answer as every other bad credential: no hint whether the token was close.
            return (err_reply(401, "unauthorized"), None);
        };
        // Stage the whole cookie-map generation off to the side. Until the encrypted file succeeds,
        // no request can observe an eviction or a cookie that would disappear on restart.
        let mut proposed_reviewers = guard.reviewers.clone();
        let mut proposed_session_issued = guard.session_issued.clone();
        let now = SystemTime::now();
        let expired: Vec<String> = proposed_session_issued
            .iter()
            // `unwrap_or_default()` = a clock that moved backwards reads as "no time passed", i.e. NOT
            // expired. Availability is the safe direction here: a skew must never sign eight reviewers out.
            .filter(|(_, issued)| now.duration_since(**issued).unwrap_or_default() >= COUCH_SESSION_TTL)
            .map(|(token, _)| token.clone())
            .collect();
        for token in expired {
            proposed_session_issued.remove(&token);
            proposed_reviewers.remove(&token);
        }
        while proposed_reviewers.values().filter(|name| *name == &reviewer).count() >= MAX_SESSIONS_PER_REVIEWER {
            let oldest = proposed_session_issued
                .iter()
                .filter(|(token, _)| proposed_reviewers.get(*token) == Some(&reviewer))
                .min_by_key(|(_, issued)| **issued)
                .map(|(token, _)| token.clone());
            let Some(oldest) = oldest else {
                break;
            };
            proposed_session_issued.remove(&oldest);
            proposed_reviewers.remove(&oldest);
        }
        let session_token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        proposed_reviewers.insert(session_token.clone(), reviewer.clone());
        proposed_session_issued.insert(session_token.clone(), now);
        let mut snapshot = snapshot_session_save(&guard);
        if let Some(snapshot) = snapshot.as_mut() {
            snapshot.sessions = snapshot_sessions_from_maps(&proposed_reviewers, &proposed_session_issued);
        }
        let inject_persist_failure = guard.inject_session_persist_failure();
        (reviewer, session_token, proposed_reviewers, proposed_session_issued, snapshot, inject_persist_failure)
    };
    // DPAPI and filesystem I/O are intentionally outside CouchState. SESSION_PERSIST remains held,
    // so another claim/revoke/save still cannot overtake this provisional generation.
    let saved = if inject_persist_failure {
        Err("injected session persistence failure".to_string())
    } else {
        snapshot.as_ref().map(save_session_snapshot).unwrap_or(Ok(()))
    };
    if let Err(error) = saved {
        tracing::error!("Couch Review refused to mint a cookie because its durable session save failed: {error}");
        return (err_reply(503, "pairing is temporarily unavailable"), None);
    }
    {
        let mut guard = lock_state(state);
        guard.reviewers = proposed_reviewers;
        guard.session_issued = proposed_session_issued;
    }
    (
        (
            200,
            "application/json",
            serde_json::json!({ "ok": true, "reviewer": reviewer }).to_string().into_bytes(),
            vec![],
        ),
        Some(format!("{COUCH_COOKIE}={session_token}; Path=/; Max-Age=86400; SameSite=Strict; Secure; HttpOnly")),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CookieRenewal {
    Current,
    Renewed,
    Missing,
}

/// Advance a cookie's server-side/durable checkpoint only when the last checkpoint is old enough.
/// Frequent page loads do NOT move the in-memory time: otherwise every sub-15-minute visit resets the
/// clock before it becomes eligible to persist, and an active reviewer eventually dies at restart.
fn renew_cookie_checkpoint(token: &str, state: &Mutex<CouchState>, now: SystemTime) -> Result<CookieRenewal, String> {
    let _persist = lock_session_persist();
    let (proposed_session_issued, snapshot, inject_persist_failure) = {
        let guard = lock_state(state);
        if !guard.reviewers.contains_key(token) {
            return Ok(CookieRenewal::Missing);
        }
        let Some(issued) = guard.session_issued.get(token).copied() else {
            return Ok(CookieRenewal::Missing);
        };
        if now.duration_since(issued).unwrap_or_default() < RENEWAL_PERSIST_AFTER {
            return Ok(CookieRenewal::Current);
        }
        let mut proposed_session_issued = guard.session_issued.clone();
        proposed_session_issued.insert(token.to_string(), now);
        let mut snapshot = snapshot_session_save(&guard);
        if let Some(snapshot) = snapshot.as_mut() {
            snapshot.sessions = snapshot_sessions_from_maps(&guard.reviewers, &proposed_session_issued);
        }
        (proposed_session_issued, snapshot, guard.inject_session_persist_failure())
    };
    let saved = if inject_persist_failure {
        Err("injected session persistence failure".to_string())
    } else {
        snapshot.as_ref().map(save_session_snapshot).unwrap_or(Ok(()))
    };
    saved?;
    lock_state(state).session_issued = proposed_session_issued;
    Ok(CookieRenewal::Renewed)
}

/// Immutable snapshot used by the link-health probe. It deliberately contains only the durable
/// serving contract: link credentials, hidden-check registration, live cookie sessions, the bound
/// pilot, and the database binding. Queue leases, undo state, skips, and in-flight decisions are not
/// durable and therefore do not participate in the comparison (the tests still pin that probing
/// cannot change any of them).
struct ClaimProbeSnapshot {
    reviewer: String,
    dir: PathBuf,
    db_path: String,
    pairing_codes: HashMap<String, String>,
    spot_checks: HashSet<(String, String)>,
    pilot_spot_checks: HashSet<(String, String)>,
    live_sessions: Option<HashMap<String, (String, u64)>>,
    pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
}

/// Canonicalize the live cookie map into the second precision written by [`save_session`]. `None`
/// means the two runtime maps disagree (a usable cookie with no issue time, or orphan issue
/// metadata), which is precisely the cookie-amnesia state this probe exists to make red.
fn unexpired_live_session_seconds(state: &CouchState, now: SystemTime) -> Option<HashMap<String, (String, u64)>> {
    if state.session_issued.keys().any(|token| !state.reviewers.contains_key(token)) {
        return None;
    }
    let mut sessions = HashMap::new();
    for (token, reviewer) in &state.reviewers {
        let issued = *state.session_issued.get(token)?;
        if now.duration_since(issued).unwrap_or_default() >= COUCH_SESSION_TTL {
            continue;
        }
        let issued_unix = issued.duration_since(UNIX_EPOCH).ok()?.as_secs();
        sessions.insert(token.clone(), (reviewer.clone(), issued_unix));
    }
    Some(sessions)
}

fn remembered_session_seconds(
    sessions: HashMap<String, (String, SystemTime)>,
) -> Option<HashMap<String, (String, u64)>> {
    sessions
        .into_iter()
        .map(|(token, (reviewer, issued))| {
            issued.duration_since(UNIX_EPOCH).ok().map(|duration| (token, (reviewer, duration.as_secs())))
        })
        .collect()
}

/// Compare the state that is authenticating requests right now with the encrypted file a restart
/// would restore. The caller holds [`SESSION_PERSIST`] across both the memory snapshot and this read,
/// so a concurrent claim/revoke/save cannot make the comparison observe two different generations.
fn durable_claim_probe_state_matches(snapshot: &ClaimProbeSnapshot) -> bool {
    let Some(live_sessions) = snapshot.live_sessions.as_ref() else {
        return false;
    };
    let Some(remembered) = load_session(&snapshot.dir, &snapshot.db_path) else {
        return false;
    };
    let Some(remembered_sessions) = remembered_session_seconds(remembered.sessions) else {
        return false;
    };
    snapshot.pairing_codes == remembered.pairing
        && snapshot.spot_checks == remembered.spot_checks
        && snapshot.pilot_spot_checks == remembered.pilot_spot_checks
        && *live_sessions == remembered_sessions
        && snapshot.pilot_policy == remembered.pilot_policy
}

fn sha256_lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn claim_probe_denied() -> Reply {
    // One generic response for malformed, unknown, cookie-only, and oversized credentials: the
    // endpoint never explains which part of a secret was close and never echoes untrusted input.
    err_reply(401, "unauthorized")
}

/// Read-only counterpart to [`api_claim`], used only by the production link gate. It authenticates
/// the durable pairing secret but never mints/prunes a cookie, touches a lease, consumes a hidden
/// key, writes the database, or persists state. The database path itself is intentionally absent;
/// callers compare a SHA-256 binding instead of receiving a private machine path.
fn api_claim_probe(body: &[u8], state: &Mutex<CouchState>) -> Reply {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ClaimProbeBody {
        token: String,
    }

    if body.len() > MAX_CLAIM_PROBE_BODY_BYTES {
        return claim_probe_denied();
    }
    let Ok(parsed) = serde_json::from_slice::<ClaimProbeBody>(body) else {
        return claim_probe_denied();
    };
    if parsed.token.is_empty() || parsed.token.len() > MAX_CLAIM_PROBE_TOKEN_BYTES {
        return claim_probe_denied();
    }

    // Same lock order as every durable save: persistence first, then CouchState. Reversing it would
    // deadlock a probe against a concurrent claim or revoke.
    let _persist = lock_session_persist();
    let snapshot = {
        let guard = lock_state(state);
        // Pairing credentials only. A cookie copied out of `reviewers` is not a durable reviewer link
        // and must never make this gate green.
        let Some(reviewer) = guard.pairing_codes.get(&parsed.token).cloned() else {
            return claim_probe_denied();
        };
        let Some((dir, db_path)) = guard.session_store.clone() else {
            return err_reply(503, "probe unavailable");
        };
        ClaimProbeSnapshot {
            reviewer,
            dir,
            db_path,
            pairing_codes: guard.pairing_codes.clone(),
            spot_checks: guard.spot_checks.clone(),
            pilot_spot_checks: guard.pilot_spot_checks.clone(),
            live_sessions: unexpired_live_session_seconds(&guard, SystemTime::now()),
            pilot_policy: guard.pilot_policy.clone(),
        }
    };
    let durable_state_matches = durable_claim_probe_state_matches(&snapshot);
    let db_binding_sha256 = sha256_lower_hex(snapshot.db_path.as_bytes());
    json_reply(
        200,
        serde_json::json!({
            "reviewer": snapshot.reviewer,
            "pilotPolicy": snapshot.pilot_policy,
            "dbBindingSha256": db_binding_sha256,
            "durableStateMatches": durable_state_matches,
        }),
    )
}

fn json_reply(status: u16, value: serde_json::Value) -> Reply {
    (status, "application/json", value.to_string().into_bytes(), vec![])
}

/// Build the one reviewer-accounting payload used by every queue/decision/undo response.
///
/// `reviewedMs` is full judged-audio progress. `earnedMicroIqd` is the distinct, action-weighted
/// ledger total and is encoded as a decimal STRING so JavaScript cannot round an i64 amount. Keeping
/// this in one helper prevents the retry, spot-check and Undo paths from drifting onto different pay
/// semantics. Callers that have already committed irreplaceable work use
/// [`json_reply_with_accounting`], which reports accounting as unavailable instead of inventing zero.
fn reviewer_accounting(db: &Database, reviewer: &str) -> Result<serde_json::Value, String> {
    let reviewed_ms = db.reviewed_audio_ms(reviewer).map_err(|error| error.to_string())?;
    let compensation = db.review_compensation_summary(reviewer).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "accountingAvailable": true,
        "reviewedMs": reviewed_ms,
        "correctedMs": compensation.corrected_audio_ms,
        "earnedMicroIqd": compensation.earned_micro_iqd.to_string(),
        "settledMicroIqd": compensation.settled_micro_iqd.to_string(),
        "outstandingMicroIqd": compensation.outstanding_micro_iqd.to_string(),
        "payPolicyVersion": compensation.policy_version,
        "legacyEventsPendingReconciliation": compensation.legacy_events_pending_reconciliation,
        "fallbackIdentityEntries": compensation.fallback_identity_entries,
    }))
}

fn merge_json_object(target: &mut serde_json::Value, source: serde_json::Value) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    if let Some(source) = source.as_object() {
        target.extend(source.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
}

/// A verdict may already be durably committed when the read-only summary query fails. Returning 500
/// would leave the phone retrying completed work forever; returning a made-up zero would be worse.
/// Acknowledge the saved verdict and mark every accounting value explicitly unavailable instead.
fn json_reply_with_accounting(status: u16, mut value: serde_json::Value, db: &Database, reviewer: &str) -> Reply {
    match reviewer_accounting(db, reviewer) {
        Ok(accounting) => merge_json_object(&mut value, accounting),
        Err(error) => {
            tracing::error!("Couch Review accounting unavailable for {reviewer}: {error}");
            merge_json_object(
                &mut value,
                serde_json::json!({
                    "accountingAvailable": false,
                    "reviewedMs": null,
                    "correctedMs": null,
                    "earnedMicroIqd": null,
                    "settledMicroIqd": null,
                    "outstandingMicroIqd": null,
                    "payPolicyVersion": null,
                    "legacyEventsPendingReconciliation": null,
                    "fallbackIdentityEntries": null,
                }),
            );
        }
    }
    json_reply(status, value)
}

fn err_reply(status: u16, msg: &str) -> Reply {
    (status, "text/plain; charset=utf-8", msg.as_bytes().to_vec(), vec![])
}

fn respond_with_cookie(
    request: tiny_http::Request,
    (status, ctype, body, extra): Reply,
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
    // Caching / range headers from the route (R1). Same tolerance as Content-Type: a header that will
    // not parse is dropped rather than allowed to kill the serving thread. Content-Length is set by
    // tiny_http from the body itself and must never be overridden here — on a 206 the body IS the
    // slice, so its length is already the right answer.
    for (name, value) in &extra {
        if let Ok(header) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response = response.with_header(header);
        }
    }
    request.respond(response)
}

/// The cookie the server sets so the token stops travelling in the URL (P3.8).
const COUCH_COOKIE: &str = "cortex_couch";

/// Resolve the session token from a request: the `Cookie` header, and NOTHING ELSE.
///
/// The `?t=` query form used to be accepted here as well, because that is how a shared link arrived
/// before the fragment bootstrap existed. Removing it is the last step of
/// docs/REMOTE_PUBLIC_LINKS_PLAN.md phase 2, and it is a PREREQUISITE for exposing this server to the
/// public internet rather than a tidy-up.
///
/// The threat is not hypothetical and not a tail risk. A link pasted into WhatsApp or Telegram IS
/// fetched by that platform's preview bot within seconds. While `?t=` authenticated, that fetch
/// succeeded and handed the platform a durable credential to special-category biometric audio —
/// recorded in the plan as "the certain token leak". A fragment is never transmitted by any browser,
/// so the bot sees the bare shell; but that protection was only ever as strong as the server's
/// refusal to accept the query form, and until now the server did accept it.
///
/// Nothing legitimate is lost. A `#t=` link claims its token ONCE via POST /api/claim and rides the
/// HttpOnly cookie from then on — including for `<audio src>`, which cannot set a header but does
/// send cookies. Only a pre-fragment legacy link stops working, which is exactly the link that must
/// stop working. Reissuing a session regenerates every token anyway.
fn token_from_request(request: &tiny_http::Request) -> Option<String> {
    let cookies = request.headers().get("Cookie")?;
    cookies.as_str().split(';').find_map(|kv| {
        let (name, value) = kv.split_once('=')?;
        (name.trim() == COUCH_COOKIE).then(|| value.trim().to_string())
    })
}

/// One request header by name, case-insensitively (tiny_http's `equiv` does the casing, and requires
/// a `'static` name — every caller passes a literal).
fn request_header(request: &tiny_http::Request, name: &'static str) -> Option<String> {
    request.headers().get(name).map(|value| value.as_str().to_string())
}

fn handle_request(
    request: &mut tiny_http::Request,
    db: &Database,
    state: &Mutex<CouchState>,
) -> (Reply, Option<String>) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = request.method().clone();

    // This RELEASE-HEALTH route must run before cookie resolution. Cookie resolution intentionally
    // prunes expired sessions; doing that first made a supposedly read-only probe alter live state
    // merely because the monitoring client happened to carry an old cookie. Only the exact POST URL
    // exists: a GET or query-bearing lookalike is denied here as well, before it can touch auth state.
    // Its own smaller reader also gives malformed, unknown, and oversized probes one generic answer.
    if path == "/api/claim/probe" {
        if method != tiny_http::Method::Post || url != path {
            return (err_reply(404, "not found"), None);
        }
        if request.body_length().is_some_and(|length| length > MAX_CLAIM_PROBE_BODY_BYTES) {
            return (claim_probe_denied(), None);
        }
        return match read_claim_probe_body(request) {
            Ok(body) => (api_claim_probe(&body, state), None),
            Err(()) => (claim_probe_denied(), None),
        };
    }

    // Reject a declared oversized body before authentication or body parsing. The patched parser
    // drains an abandoned connection through a fixed buffer, so even usize::MAX cannot allocate from
    // this untrusted value; this explicit 413 also avoids waiting for bytes that will never be sent.
    if request.body_length().is_some_and(|length| length > MAX_BODY_BYTES) {
        return (err_reply(413, "body too large"), None);
    }

    // EVERY DATA route is token-gated. Three routes are deliberately reachable without a cookie,
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
    //   POST /api/claim/probe — the release gate validates a durable pairing link without minting a
    //                      cookie or touching serving state. It is handled above, before cookie
    //                      lookup, to make that non-mutation guarantee literal even for stale cookies.
    //
    // The token still RESOLVES the reviewer everywhere else: an unknown token has no identity, so
    // there is no path on which a decision can be written without a name attached. Resolved from the
    // SHARED map on every request, so a revoked token stops working immediately.
    let authenticated =
        token_from_request(request).and_then(|token| {
            let mut guard = lock_state(state);
            if guard.session_issued.get(&token).is_some_and(|issued| {
                SystemTime::now().duration_since(*issued).unwrap_or_default() >= COUCH_SESSION_TTL
            }) {
                guard.session_issued.remove(&token);
                guard.reviewers.remove(&token);
                return None;
            }
            guard.reviewers.get(&token).cloned().map(|name| (token, name))
        });

    if let (tiny_http::Method::Get, "/") = (&method, path.as_str()) {
        let shell = include_str!("../assets/couch.html").as_bytes().to_vec();
        return match authenticated {
            // Authenticated page load: RE-plant the cookie. This is the sliding expiry — an active
            // reviewer's cookie renews on every visit, so a bookmark used weekly never dies while
            // the session lives. (A fixed Max-Age from first claim would expire mid-engagement.)
            Some((token, _)) => {
                match renew_cookie_checkpoint(&token, state, SystemTime::now()) {
                    Ok(CookieRenewal::Current | CookieRenewal::Renewed) => page_reply_with_cookie(&token, shell),
                    // A concurrent durable revoke wins. The shell is public, but re-planting a token
                    // the current state no longer owns would make the browser look authenticated.
                    Ok(CookieRenewal::Missing) => ((200, "text/html; charset=utf-8", shell, vec![]), None),
                    Err(error) => {
                        tracing::error!(
                            "Couch Review refused to renew a cookie because its durable save failed: {error}"
                        );
                        (err_reply(503, "session renewal is temporarily unavailable"), None)
                    }
                }
            }
            // Unauthenticated: the bare shell, NO cookie. This is what a preview bot receives.
            None => ((200, "text/html; charset=utf-8", shell, vec![]), None),
        };
    }
    // Static assets for install-to-home-screen (phase 6), public for the same reason the shell is:
    // an icon and a manifest carry nothing. The manifest's start_url is "/" and NEVER a token — an
    // installed app must ride the cookie, or a revoked token would be frozen into someone's phone.
    if let (tiny_http::Method::Get, "/icon.png") = (&method, path.as_str()) {
        return ((200, "image/png", include_bytes!("../assets/couch-icon.png").to_vec(), vec![]), None);
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
        return ((200, "application/manifest+json", manifest.to_string().into_bytes(), vec![]), None);
    }
    if let (tiny_http::Method::Post, "/api/claim") = (&method, path.as_str()) {
        return match read_body(request) {
            Ok(body) => {
                // Pairing is an authorization grant and must be ordered with per-reviewer revoke too:
                // a claim already parsed before Revoke either finishes wholly before it (and is then
                // removed), or observes the removed pairing code after it.
                let _authorization = REVIEW_AUTHORIZATION.read().unwrap_or_else(|poisoned| poisoned.into_inner());
                api_claim(&body, state)
            }
            Err(e) => (err_reply(400, &e), None),
        };
    }

    let Some((token, reviewer)) = authenticated else {
        // LOUD, because a refused reviewer is otherwise invisible. Six of eight paid reviewers went
        // silent over nine days and the log held not one line about it: nothing recorded a refusal,
        // so "their link is dead" and "they stopped working" looked identical from here. The path is
        // safe to print; the credential never is, and is not printed.
        tracing::warn!(
            "Couch Review refused an unauthenticated request for {path} — a reviewer whose session the server no longer knows sees \"link expired\" and cannot re-enter without their original link"
        );
        return (err_reply(401, "unauthorized"), None);
    };
    let reviewer = reviewer.as_str();
    if let Err(e) = COUCH_RATE_LIMITER.check(reviewer) {
        return ((429, "text/plain; charset=utf-8", e.into_bytes(), vec![]), None);
    }

    // Copied out BEFORE the match, because the POST arms below take `request` mutably and a borrow
    // held across them would not compile.
    let range = request_header(request, "Range");
    let if_none_match = request_header(request, "If-None-Match");

    let reply = match (method, path.as_str()) {
        (tiny_http::Method::Get, "/api/queue") => {
            with_live_reviewer(&token, reviewer, state, || api_queue(db, reviewer, state))
        }
        // HEAD alongside GET: some mobile media stacks probe with it before opening a stream, and every
        // non-GET/POST method used to fall through to 404 — a probe answered "no such thing" while the
        // GET beside it worked. tiny_http suppresses the body for a HEAD itself and keeps the
        // Content-Length, so the same reply is the correct answer to both.
        (tiny_http::Method::Get | tiny_http::Method::Head, p) if p.starts_with("/api/audio/") => {
            with_live_reviewer(&token, reviewer, state, || {
                api_audio(
                    db,
                    p.trim_start_matches("/api/audio/"),
                    reviewer,
                    state,
                    range.as_deref(),
                    if_none_match.as_deref(),
                )
            })
        }
        (tiny_http::Method::Post, "/api/decision") => match read_body(request) {
            Ok(body) => with_live_reviewer(&token, reviewer, state, || api_decision(db, &body, reviewer, state)),
            Err(e) => err_reply(400, &e),
        },
        (tiny_http::Method::Post, "/api/renew") => match read_body(request) {
            Ok(body) => with_live_reviewer(&token, reviewer, state, || api_renew(&body, reviewer, state)),
            Err(e) => err_reply(400, &e),
        },
        (tiny_http::Method::Post, "/api/undo") => {
            with_live_reviewer(&token, reviewer, state, || api_undo(db, reviewer, state))
        }
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

/// Execute one protected route only while the cookie still resolves to the same reviewer. The caller
/// reads the bounded POST body first, so a slow client cannot hold revocation hostage merely by never
/// finishing its upload. The read guard stays through the whole closure; `revoke_in` takes the write
/// side, which closes the copied-identity TOCTOU without serializing independent reviewer requests.
fn with_live_reviewer<F>(token: &str, reviewer: &str, state: &Mutex<CouchState>, operation: F) -> Reply
where
    F: FnOnce() -> Reply,
{
    let Some(_authorization) = live_session_guard(token, Some(reviewer), state) else {
        return err_reply(401, "unauthorized");
    };
    operation()
}

/// Acquire the revocation barrier and prove the cookie is still live. `expected_reviewer` binds the
/// operation-time identity; response delivery only needs the same bearer to remain live because the
/// reply was already produced under that exact binding. Returning the guard makes its lifetime
/// explicit: callers decide whether it covers the operation or the synchronous socket send.
fn live_session_guard(
    token: &str,
    expected_reviewer: Option<&str>,
    state: &Mutex<CouchState>,
) -> Option<std::sync::RwLockReadGuard<'static, ()>> {
    let authorization = REVIEW_AUTHORIZATION.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    let live = {
        let mut guard = lock_state(state);
        let expired = guard
            .session_issued
            .get(token)
            .is_some_and(|issued| SystemTime::now().duration_since(*issued).unwrap_or_default() >= COUCH_SESSION_TTL);
        if expired {
            guard.session_issued.remove(token);
            guard.reviewers.remove(token);
        }
        !expired
            && guard
                .reviewers
                .get(token)
                .is_some_and(|name| expected_reviewer.map_or(true, |expected| name == expected))
    };
    live.then_some(authorization)
}

fn read_claim_probe_body(request: &mut tiny_http::Request) -> Result<Vec<u8>, ()> {
    let mut body = Vec::new();
    request.as_reader().take(MAX_CLAIM_PROBE_BODY_BYTES as u64 + 1).read_to_end(&mut body).map_err(|_| ())?;
    (body.len() <= MAX_CLAIM_PROBE_BODY_BYTES).then_some(body).ok_or(())
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
///
/// DELIBERATELY NOT `quality::effective_transcript`, even though that is the canonical VERBATIM LAW
/// accessor and this looks like a duplicate of it. `effective_transcript` puts `verdict_transcript`
/// first for a human-decided clip — and on a SPOT-CHECK clip that field holds the ANSWER KEY. The
/// listening QC works precisely by serving a already-answered clip with its RAW, known-wrong draft;
/// showing the stored answer instead would hand every reviewer the solution and auto-pass every
/// check, silently. Tried 2026-08-15; caught by
/// `every_decision_lands_in_the_append_only_audit_trail`, which saw the spot check reclassify from
/// "edit" to "accept" because the submitted text then matched what was served.
///
/// Phone finalization now shares the human-decision transaction, so a correction cannot exist in
/// `verdict_transcript` while the row remains pending. This precedence remains separate solely to
/// protect blinded spot checks from seeing their answer key.
fn review_text(seg: &SpeechSegment) -> String {
    seg.annotated_transcript.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| seg.raw_transcript.clone())
}

/// Was this clip MEASURED to span a turn between two people? (Migration v47.)
///
/// The reviewer needs this before they decide, not after. Chunks are cut on silence and labelled
/// afterwards, so a clip holding two speakers still shows exactly one `SPEAKER_xx` — the phone would
/// otherwise present a two-speaker clip as ordinary work, and "Looks good" would walk it into a
/// single-speaker corpus with an authoritative-looking wrong label attached.
///
/// `None` (not measured) is FALSE here, and the page shows nothing rather than "one speaker". Absence
/// of a measurement is not evidence of a single speaker, and the badge never claims it is.
fn holds_a_speaker_change(seg: &SpeechSegment) -> bool {
    seg.speaker_change_score.is_some_and(|s| (s as f32) < crate::diarization::SPEAKER_CHANGE_THRESHOLD)
}

fn build_pilot_decision_limit(policy: &crate::review_pilot::ReviewPilotPolicy) -> Result<ReviewDecisionLimit, String> {
    ReviewDecisionLimit::new(
        policy.after_review_event_id,
        policy.max_total_corpus_actions,
        policy.reviewers.iter().map(|reviewer| (reviewer.name.clone(), reviewer.max_corpus_actions)).collect(),
    )
    .map_err(|error| error.to_string())
}

/// Re-read and compare the operating file with the snapshot bound to this session. A live edit is a
/// pause, not a hot reset: Stop + Start is the explicit boundary that can authorize a new baseline.
fn active_pilot_policy(
    reviewer: &str,
    state: &Mutex<CouchState>,
) -> Result<Option<crate::review_pilot::ReviewPilotPolicy>, String> {
    let (data_dir, bound, live_names) = {
        let st = lock_state(state);
        let names: Vec<String> = if st.pairing_codes.is_empty() {
            st.reviewers.values().cloned().collect()
        } else {
            st.pairing_codes.values().cloned().collect()
        };
        (st.session_store.as_ref().map(|(dir, _)| dir.clone()), st.pilot_policy.clone(), names)
    };
    let current = match data_dir.as_deref() {
        Some(dir) => crate::review_pilot::load(dir)?,
        None => None,
    };
    if current != bound {
        return Err(
            "controlled review policy changed during this session; review is paused until the owner stops and restarts it"
                .to_string(),
        );
    }
    if let Some(policy) = current.as_ref() {
        if !policy.matches_session(&live_names) {
            return Err(
                "controlled review pilot roster changed; review is paused until exactly the two authorized reviewers are restored"
                    .to_string(),
            );
        }
        if policy.cap_for(reviewer).is_none() {
            return Err("this reviewer is not authorized for the controlled review pilot".to_string());
        }
    }
    Ok(current)
}

/// Remaining review-action slots for this reviewer, validated against the global count as well.
/// Returns an error for unauthorized/over-cap history rather than hiding it behind zero.
fn pilot_remaining_slots(
    db: &Database,
    reviewer: &str,
    policy: &crate::review_pilot::ReviewPilotPolicy,
) -> Result<usize, String> {
    let limit = build_pilot_decision_limit(policy)?;
    let progress = db.review_decision_progress(&limit).map_err(|error| error.to_string())?;
    let reviewer_cap =
        policy.cap_for(reviewer).ok_or_else(|| "reviewer is outside the controlled review pilot".to_string())?;
    let reviewer_count = progress
        .by_reviewer
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(reviewer.trim()))
        .map(|(_, count)| *count)
        .unwrap_or(0);
    let reviewer_remaining = reviewer_cap.saturating_sub(reviewer_count);
    let total_remaining = policy.max_total_corpus_actions.saturating_sub(progress.total_review_actions);
    usize::try_from(reviewer_remaining.min(total_remaining))
        .map_err(|_| "controlled review pilot remaining count is invalid".to_string())
}

/// Number of distinct hidden keys this bounded pilot may ever mint for one reviewer.
///
/// This is derived from the same action cap and cadence that bound the queue.  Unlike the ordinary
/// long-running campaign, the pilot does not need the independent three-key launch floor: ten
/// actions at one check per eight require exactly two keys.  The served count is persisted so a page
/// reload or process restart cannot reset the budget and quietly consume a third key.
fn pilot_spot_check_quota(policy: &crate::review_pilot::ReviewPilotPolicy, reviewer: &str) -> Result<usize, String> {
    let cap = policy.cap_for(reviewer).ok_or_else(|| "reviewer is outside the controlled review pilot".to_string())?;
    let cap = usize::try_from(cap).map_err(|_| "controlled review pilot action cap is invalid".to_string())?;
    Ok(cap.div_ceil(SPOT_CHECK_EVERY))
}

/// Plan one pilot refill without ever minting more distinct keys than `quota`.
///
/// Outstanding keys are re-served first.  They do not consume a second quota unit; this is what
/// keeps a reload from either evading the hidden checks or burning a fresh key.  Completed/declined
/// checks stay in `distinct_served`, so later refills do not over-test the bounded ten-action sample.
fn pilot_spot_check_plan(
    work_items: usize,
    quota: usize,
    distinct_served: usize,
    outstanding: usize,
    force_complete: bool,
) -> (usize, usize) {
    let desired = if force_complete { quota } else { work_items.div_ceil(SPOT_CHECK_EVERY).min(quota) };
    let resend = desired.min(outstanding);
    let fresh = desired.saturating_sub(resend).min(quota.saturating_sub(distinct_served));
    (resend, fresh)
}

/// Rehydrate pilot checks that were already handed to this reviewer but have not been answered.
/// Every returned row is revalidated against the current key, focus, dialect and audio state.  A
/// previously minted key that silently ceased to be valid is a hard error: replacing it would exceed
/// the proven two-key capacity, while pretending it remains a check would corrupt reviewer scoring.
fn pilot_outstanding_spot_checks(
    db: &Database,
    reviewer: &str,
    ids: &[String],
    skipped: &HashSet<String>,
    allowed_dialects: Option<&[String]>,
    focus: Option<&HashSet<String>>,
) -> Result<Vec<SpeechSegment>, String> {
    let rows = db.get_segments_by_ids_with_revisions(ids).map_err(|error| error.to_string())?;
    let mut by_id: HashMap<&str, &SpeechSegment> =
        rows.iter().map(|(segment, _)| (segment.id.as_str(), segment)).collect();
    let mut out = Vec::new();
    for id in ids {
        if db.has_spot_check_result(id, reviewer).map_err(|error| error.to_string())? || skipped.contains(id) {
            continue;
        }
        let Some(segment) = by_id.remove(id.as_str()) else {
            return Err(format!("previously served hidden-check key {id} no longer exists"));
        };
        let expected = crate::quality::human_verified_text(segment)
            .ok_or_else(|| format!("previously served hidden-check key {id} no longer has a human answer"))?;
        if !segment.verified
            || !Path::new(&segment.audio_path).is_file()
            || !crate::dialect::reviewer_may_judge(allowed_dialects, &segment.audio_path)
            || focus.is_some_and(|allowed| !allowed.contains(id))
            || crate::normalizer::learning_text_key(expected)
                == crate::normalizer::learning_text_key(&segment.raw_transcript)
        {
            return Err(format!("previously served hidden-check key {id} is no longer eligible"));
        }
        out.push(segment.clone());
    }
    Ok(out)
}

type ReviewerPolicy = (Option<Vec<String>>, Option<Arc<HashSet<String>>>);
type SpotCheckSelection = Result<Option<(Vec<SpeechSegment>, Option<usize>)>, String>;

fn lock_pilot_decision_commit() -> std::sync::MutexGuard<'static, ()> {
    PILOT_DECISION_COMMIT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Resolve the policy that authorizes one reviewer's queue and decisions.
///
/// This is deliberately shared by BOTH boundaries. Filtering only `/api/queue` is not authorization:
/// a phone can retain an id in its outbox, and anyone holding a valid reviewer credential can POST a
/// known id without fetching a queue first. Re-reading here preserves the policy files' hot-reload
/// contract and makes a change effective on the next decision as well as the next queue fetch.
fn reviewer_policy(reviewer: &str, state: &Mutex<CouchState>) -> Result<ReviewerPolicy, String> {
    let dir = { lock_state(state).session_store.as_ref().map(|(data_dir, _db_path)| data_dir.clone()) };
    let allowed_dialects = match dir.as_ref().map(|d| crate::dialect::load_roster(d)) {
        Some(Err(e)) => return Err(format!("policy file broken, no clips served: {e}")),
        // Matched the way the session layer matches names (trim + ASCII case), never an exact
        // HashMap::get — a roster key that binds nobody is the wrong-dialect incident returning.
        Some(Ok(roster)) => crate::dialect::allowed_for(&roster, reviewer).cloned(),
        None => None,
    };
    let focus = crate::voice_focus::resolve(dir.as_deref())?;
    Ok((allowed_dialects, focus))
}

fn reviewer_policy_allows(
    allowed_dialects: Option<&[String]>,
    focus: Option<&HashSet<String>>,
    segment: &SpeechSegment,
) -> bool {
    crate::dialect::reviewer_may_judge(allowed_dialects, &segment.audio_path)
        && focus.map_or(true, |ids| ids.contains(&segment.id))
}

/// Pending (unverified) clips, oldest first — the same "work that needs doing" the desktop queue leads
/// with — MINUS anything another reviewer currently holds, and leased to this reviewer on the way out.
///
/// The lease is what makes two phones safe: without it both are handed the same head-of-queue clips and
/// race to decide them (duplicated effort at best, one reviewer's verdict silently overwriting the
/// other's at worst). Batches are small so the first reviewer to load cannot lease the entire backlog,
/// and leases expire so a closed browser tab never strands work.
fn api_queue(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    // P1.3: IDs, not whole rows. This walked EVERY pending segment's full record — transcript,
    // alignment JSON, evidence JSON, the lot — to hand out at most QUEUE_BATCH of them. The counts
    // below genuinely need every pending row (they depend on in-memory LEASE state, which no SQL
    // aggregate can see), but a row that is only being COUNTED does not need anything except its id.
    // The <= 25 clips actually served are hydrated after the lock is released.
    // Dialect roster, re-read per fetch so the owner can change who reviews what without restarting
    // the app. A reviewer with no entry is unrestricted, exactly as before this existed.
    // Voice focus (owner instruction 2026-08-19): when `<data_dir>/voice_focus.json` names a set of
    // clips, every reviewer's queue narrows to it — so paid hours build the one speaker's set being
    // collected now instead of spreading across 34 h of mixed audio. Same hot-reload-per-fetch.
    //
    // A policy file that EXISTS but cannot be honoured is a 503, not a shrug (owner instruction
    // 2026-08-20 — present-but-broken fails CLOSED). Failing open here silently pointed every paid
    // reviewer at work the file was written to keep them off; the page treats >=500 as retryable and
    // shows the status, so the line stops loudly and the very next fetch after the owner fixes the
    // file works. A MISSING file is still "no restriction", exactly as before either file existed.
    let (allowed_dialects, focus) = match reviewer_policy(reviewer, state) {
        Err(e) => return err_reply(503, &e),
        Ok(policy) => policy,
    };
    let pilot_policy = match active_pilot_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    let pilot_slots = match pilot_policy.as_ref() {
        Some(policy) => match pilot_remaining_slots(db, reviewer, policy) {
            Ok(remaining) => Some(remaining),
            Err(error) => return err_reply(503, &format!("controlled review pilot is unavailable: {error}")),
        },
        None => None,
    };
    // Verify the full activity + weighted-pay view BEFORE leasing work or marking a hidden check as
    // served. If accounting is unhealthy, this request must leave no queue/session side effects.
    let accounting = match reviewer_accounting(db, reviewer) {
        Ok(accounting) => accounting,
        Err(error) => {
            tracing::error!("Couch Review queue paused because accounting is unavailable for {reviewer}: {error}");
            return err_reply(503, "Review is temporarily paused: reviewer accounting is unavailable");
        }
    };
    let pending_ids = match db.pending_segment_ids_focused(allowed_dialects.as_deref(), focus.as_deref()) {
        Ok(ids) => ids,
        Err(e) => return err_reply(500, &e.to_string()),
    };
    let pending_total = pilot_slots.map_or(pending_ids.len(), |remaining| pending_ids.len().min(remaining));
    // An empty queue means two very different things, and the page must not say "all clips reviewed"
    // for the second: everything really is done, OR this reviewer is restricted to a dialect that has
    // no work right now (today: everything playable is Hawleri, so a Sorani-only reviewer has none).
    // A VOICE FOCUS counts as a restriction here too (2026-08-20 hunt): with the queue narrowed to
    // one speaker and that set drained, an unrestricted reviewer used to be shown "all clips
    // reviewed 🎉" while thousands of pending clips sat outside the focus — a lie about the
    // library, told at the exact moment the owner would want to widen the focus.
    let restricted_and_empty =
        (allowed_dialects.is_some() || focus.is_some() || pilot_slots.is_some()) && pending_total == 0;
    let now = Instant::now();
    let mut guard = lock_state(state);
    let mut serving: Vec<String> = Vec::new();
    let mut held_by_others = 0usize;
    let mut skipped_by_you = 0usize;
    let serving_limit = pilot_slots.map_or(QUEUE_BATCH, |remaining| QUEUE_BATCH.min(remaining));
    for id in pending_ids {
        // Someone else's live lease: skip it, but COUNT it. Our own is renewed below (a reviewer
        // reloading the page must get their own in-progress work back, not a fresh batch that
        // abandons it).
        if guard.holder(&id, now).is_some_and(|who| who != reviewer) {
            held_by_others += 1;
            continue;
        }
        // This reviewer already said they cannot judge this one (R4.4). Not theirs to be handed
        // again — but COUNTED, because it is still pending work somebody owes a verdict, and an
        // empty batch full of these must not draw "🎉 all clips reviewed".
        if guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(&id)) {
            skipped_by_you += 1;
            continue;
        }
        if serving.len() >= serving_limit {
            continue; // keep counting what is left, but hand out no more this round
        }
        guard.leases.insert(id.clone(), (reviewer.to_string(), now));
        serving.push(id);
    }
    // Drop leases that expired while their holder was away, so the map cannot grow without bound across
    // a long session. Cheap: the pending queue is the only thing that can be leased.
    guard.leases.retain(|_, (_, granted)| now.duration_since(*granted) < LEASE_TTL);
    // Snapshotted under the lock and handed to the spot-check selection below, which runs OUTSIDE it.
    // Spot checks are inserted after the loop above, so the skip filter in that loop never sees them —
    // without passing this along, a check somebody skipped was re-inserted into every batch forever.
    let skipped_by_me = guard.skipped.get(reviewer).cloned().unwrap_or_default();
    // A pilot key budget is DISTINCT-KEY based, not request based. Snapshot it under the same lock
    // that protects insertion so two concurrent reloads cannot both observe room for the final key.
    let mut pilot_check_ids: Vec<String> = guard
        .pilot_spot_checks
        .iter()
        .filter(|(_, name)| name.eq_ignore_ascii_case(reviewer))
        .map(|(id, _)| id.clone())
        .collect();
    pilot_check_ids.sort();
    pilot_check_ids.dedup();
    // A persisted session is the production paid-review path. Ephemeral test/dev callers preserve
    // their historical best-effort behaviour, while a real durable reviewer link must never receive
    // unmeasured work after its hidden-key pool runs dry.
    let require_complete_spot_checks =
        guard.session_store.is_some() && (!guard.pairing_codes.is_empty() || !guard.reviewers.is_empty());
    drop(guard);

    // Hydrate ONLY what is being served — at most QUEUE_BATCH rows — and do it OUTSIDE the state lock,
    // which the whole-library read never was. `get_segments_by_ids` re-imposes its own global ordering,
    // so the rows are indexed by id and re-emitted in `serving` order; handing a reviewer the same
    // clips in a different order than they were leased would be a silent behaviour change.
    let rows = match db.get_segments_by_ids_with_revisions(&serving) {
        Ok(rows) => rows,
        Err(e) => return err_reply(500, &e.to_string()),
    };
    let by_id: std::collections::HashMap<&str, (&SpeechSegment, i64)> =
        rows.iter().map(|(segment, revision)| (segment.id.as_str(), (segment, *revision))).collect();
    // filter_map, not unwrap: a clip can be deleted between the id query and this fetch. Serving one
    // fewer clip is correct; panicking on a race in the reviewer's request path is not. Its lease simply
    // expires.
    let mut queue: Vec<serde_json::Value> = serving
        .iter()
        .filter_map(|id| by_id.get(id.as_str()))
        .map(|(s, revision)| {
            serde_json::json!({
                "id": s.id,
                "text": review_text(s),
                "durationMs": s.duration_ms,
                "speakerId": s.speaker_id,
                "speakerChange": holds_a_speaker_change(s),
                // The row's change-fingerprint at serve time; the page echoes it on the decision so
                // a draft replaced by a background writer in between is refused, never recorded.
                "rowVersion": revision.to_string(),
                // A pre-pilot durable outbox has no baseline and is refused before any write.  The
                // page echoes this field exactly; it is authorization context, not a client claim.
                "pilotAfterReviewEventId": pilot_policy.as_ref().map(|policy| policy.after_review_event_id),
            })
        })
        .collect();

    // Salt the batch with spot checks (P2.1). They are NOT leased: two reviewers meeting the same
    // known-answer clip is the point — independent measurement — not a collision. They also carry no
    // marker of any kind in the payload, because a reviewer who can spot the test is not being tested.
    let pilot_catchup = pilot_policy.is_some() && pilot_slots == Some(0);
    if !queue.is_empty() || pilot_catchup {
        // CEILING, not floor. `len / EVERY` silently gives ZERO checks to any batch under EVERY
        // items — so a reviewer arriving late to a nearly-drained queue, or sharing a small backlog,
        // would never be measured at all. Rounding up guarantees at least one check in every non-empty
        // batch while holding the ~1-in-8 ratio wherever the batch is large enough for it to mean
        // something. A reviewer must not be able to coast just because their batches came out short.
        let work_len = queue.len();
        let cadence_wanted = work_len.div_ceil(SPOT_CHECK_EVERY);
        let selected: SpotCheckSelection = if let Some(policy) = pilot_policy.as_ref() {
            (|| {
                let quota = pilot_spot_check_quota(policy, reviewer)?;
                let outstanding = pilot_outstanding_spot_checks(
                    db,
                    reviewer,
                    &pilot_check_ids,
                    &skipped_by_me,
                    allowed_dialects.as_deref(),
                    focus.as_deref(),
                )?;
                let (resend, fresh_wanted) =
                    pilot_spot_check_plan(work_len, quota, pilot_check_ids.len(), outstanding.len(), pilot_catchup);
                let mut candidates: Vec<SpeechSegment> = outstanding.into_iter().take(resend).collect();
                if fresh_wanted > 0 {
                    // Already-minted pilot ids are either re-served above or permanently consumed by
                    // an answer/decline.  Excluding them here makes `fresh_wanted` mean fresh exactly.
                    let mut exclude = skipped_by_me.clone();
                    exclude.extend(pilot_check_ids.iter().cloned());
                    let fresh = db
                        .list_spot_check_candidates(
                            fresh_wanted,
                            reviewer,
                            &exclude,
                            allowed_dialects.as_deref(),
                            focus.as_deref(),
                        )
                        .map_err(|error| error.to_string())?;
                    if fresh.len() < fresh_wanted {
                        Err("genuine hidden-check capacity is below the controlled-pilot requirement".to_string())
                    } else {
                        candidates.extend(fresh.into_iter().map(|(segment, _)| segment));
                        Ok(Some((candidates, Some(quota))))
                    }
                } else {
                    Ok(Some((candidates, Some(quota))))
                }
            })()
        } else {
            match db.list_spot_check_candidates(
                cadence_wanted,
                reviewer,
                &skipped_by_me,
                allowed_dialects.as_deref(),
                focus.as_deref(),
            ) {
                Ok(candidates) if require_complete_spot_checks && candidates.len() < cadence_wanted => {
                    Err("genuine hidden-check capacity is below the required campaign coverage".to_string())
                }
                Ok(candidates) => Ok(Some((candidates.into_iter().map(|(segment, _)| segment).collect(), None))),
                Err(error) if require_complete_spot_checks => Err(format!("hidden-check selection failed: {error}")),
                Err(error) => {
                    tracing::warn!("Couch Review spot-check selection failed: {error}");
                    Ok(None)
                }
            }
        };
        match selected {
            Err(error) => {
                // These work ids were leased above but no batch is being served. Release only this
                // reviewer's batch so another request is not told the work is held while the paid
                // line is correctly paused on the same quality-capacity failure.
                let mut guard = lock_state(state);
                for id in &serving {
                    if guard.leases.get(id).is_some_and(|(who, _)| who == reviewer) {
                        guard.leases.remove(id);
                    }
                }
                return err_reply(503, &format!("Review is temporarily paused: {error}"));
            }
            Ok(Some((candidates, pilot_quota))) => {
                let wanted = candidates.len();
                // Pilot key registration and its session-file save are one ordered operation. Taking
                // the same cross-request persistence lock BEFORE mutating memory prevents a concurrent
                // save/revoke from snapshotting a half-registered key budget.
                let _pilot_persist = pilot_quota.map(|_| lock_session_persist());
                let mut guard = lock_state(state);
                if let Some(quota) = pilot_quota {
                    // A second reload may have selected concurrently from the same snapshot. Recheck
                    // the distinct-key total while holding the insertion lock; never trust the earlier
                    // planning read as the authority for the two-key ceiling.
                    let current: HashSet<String> = guard
                        .pilot_spot_checks
                        .iter()
                        .filter(|(_, name)| name.eq_ignore_ascii_case(reviewer))
                        .map(|(id, _)| id.clone())
                        .collect();
                    let additional = candidates.iter().filter(|segment| !current.contains(&segment.id)).count();
                    if current.len().saturating_add(additional) > quota {
                        for id in &serving {
                            if guard.leases.get(id).is_some_and(|(who, _)| who == reviewer) {
                                guard.leases.remove(id);
                            }
                        }
                        return err_reply(503, "Review is temporarily paused: controlled hidden-check quota changed");
                    }
                }
                let mut grew = false;
                let mut inserted_spot_checks = Vec::new();
                let mut inserted_pilot_checks = Vec::new();
                for (idx, seg) in candidates.into_iter().enumerate() {
                    let key = (seg.id.clone(), reviewer.to_string());
                    if guard.spot_checks.insert(key.clone()) {
                        grew = true;
                        inserted_spot_checks.push(key.clone());
                    }
                    if pilot_quota.is_some() && guard.pilot_spot_checks.insert(key.clone()) {
                        grew = true;
                        inserted_pilot_checks.push(key);
                    }
                    // SPREAD EVENLY, and this is a fix, not a preference. The position used to be
                    // `((idx + 1) * SPOT_CHECK_EVERY).min(queue.len())`, and the comment above it
                    // claimed to interleave "rather than append in a run at the tail". Measured across a
                    // five-batch session: a spot check landed LAST in 5 of 5 batches. `wanted` is
                    // `div_ceil`, so a 25-clip batch asks for 4 checks while only three multiples of 8
                    // fall inside it (8, 16, 24) — the fourth computed 32, clamped to the end, and was
                    // appended every single time. A reviewer who noticed that the last clip of every
                    // batch is a trap could pass every test in a session by listening to one clip.
                    //
                    // Dividing the batch into `wanted + 1` gaps puts every check strictly inside it:
                    // 25 work clips and 4 checks give 5, 11, 17, 23 (the `+ idx` accounts for the
                    // earlier insertions having shifted everything after them). The `.min` is kept only
                    // so the index can never exceed the length — with this formula it does not bind, and
                    // Vec::insert panicking is not an acceptable way to find that out.
                    let at = ((idx + 1) * (work_len + 1) / (wanted + 1) + idx).min(queue.len());
                    queue.insert(
                        at,
                        serde_json::json!({
                            "id": seg.id,
                            // The RAW draft — the known-wrong one. Serving the corrected text would
                            // make the check unpassable-by-failing: there would be nothing to catch.
                            "text": seg.raw_transcript,
                            "durationMs": seg.duration_ms,
                            "speakerId": seg.speaker_id,
                            // Computed exactly as for work clips. A spot check that carried this
                            // field differently would be spottable, and a reviewer who can spot the
                            // test is not being tested.
                            "speakerChange": holds_a_speaker_change(&seg),
                            // Same field as work clips for the same indistinguishability reason.
                            "rowVersion": db.segment_row_stamp(&seg.id).ok().flatten(),
                            "pilotAfterReviewEventId": pilot_policy.as_ref().map(|policy| policy.after_review_event_id),
                        }),
                    );
                }
                let pilot_snapshot = (pilot_quota.is_some() && grew).then(|| snapshot_session_save(&guard)).flatten();
                #[cfg(test)]
                let inject_pilot_save_failure = pilot_quota.is_some() && grew && guard.fail_session_persist;
                // Persist the grown served-set into the remembered session (phase 4): a check served
                // now may be answered AFTER an app restart, and it must still be scored then rather
                // than 409ing the reviewer. The persistence helper snapshots after taking the global
                // ordering lock, so an older queue save cannot overwrite a later credential revoke.
                drop(guard);
                if grew {
                    let saved = if pilot_quota.is_some() {
                        #[cfg(test)]
                        if inject_pilot_save_failure {
                            Err("injected session persistence failure".to_string())
                        } else {
                            pilot_snapshot
                                .as_ref()
                                .ok_or_else(|| "controlled pilot has no durable session store".to_string())
                                .and_then(save_session_snapshot)
                        }
                        #[cfg(not(test))]
                        {
                            pilot_snapshot
                                .as_ref()
                                .ok_or_else(|| "controlled pilot has no durable session store".to_string())
                                .and_then(save_session_snapshot)
                        }
                    } else {
                        persist_session_state(state)
                    };
                    if let Err(error) = saved {
                        if pilot_quota.is_some() {
                            let mut guard = lock_state(state);
                            // The response was never authorized, so restore the last durable served
                            // set. This also ensures a same-process retry cannot see `grew == false`
                            // and return keys that only ever existed in memory.
                            for key in inserted_spot_checks {
                                guard.spot_checks.remove(&key);
                            }
                            for key in inserted_pilot_checks {
                                guard.pilot_spot_checks.remove(&key);
                            }
                            for id in &serving {
                                if guard.leases.get(id).is_some_and(|(who, _)| who == reviewer) {
                                    guard.leases.remove(id);
                                }
                            }
                            return err_reply(
                                503,
                                &format!(
                                    "Review is temporarily paused: controlled hidden-check state was not saved ({error})"
                                ),
                            );
                        }
                    }
                }
            }
            Ok(None) => {}
        }
    }
    // Record delivery only after every fallible hidden-check/durability gate above has passed. A
    // provisional lease is deliberately NOT enough: failed queue construction returns no ids to the
    // reviewer and therefore must not mint an audio authorization that `/api/renew` could resurrect.
    // `by_id` excludes rows deleted between the pending-id scan and hydration, so receipts are issued
    // only for ordinary work that is actually present in the successful response.
    {
        let mut guard = lock_state(state);
        for id in serving.iter().filter(|id| by_id.contains_key(id.as_str())) {
            guard.served_work.insert((id.clone(), reviewer.to_string()));
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
    //   `skippedByYou` — how many pending clips THIS reviewer declined to judge. Same class of lie as
    //   `heldByOthers`, one step further along: a reviewer who skips their way through a small backlog
    //   would otherwise be congratulated with "🎉 all clips reviewed" over work nobody has judged.
    //
    //   `pendingTotal` — how much pending work EXISTS, leased or not. The page could previously only
    //   count the batch in its hands, so "clip 7 of 25" was the whole truth it had: a reviewer working
    //   a 400-clip backlog saw a bar that filled up and reset, over and over, with no way to tell a
    //   nearly-finished corpus from a barely-started one. Honest overall progress needs the total, and
    //   the total is free here — the query already walked every pending row.
    let mut payload = serde_json::json!({
        "reviewer": reviewer,
        "items": queue,
        "heldByOthers": held_by_others,
        "skippedByYou": skipped_by_you,
        "pendingTotal": pending_total,
        "pilotRemainingReviewActions": pilot_slots,
        // "there is no work in YOUR dialect", not "everything is reviewed".
        "noWorkInYourDialect": restricted_and_empty,
    });
    merge_json_object(&mut payload, accounting);
    json_reply(200, payload)
}

/// Everything that determines a clip's WAV bytes, hashed into one value used as BOTH the cache key and
/// the ETag. Deriving them from the same fingerprint is what makes a 304 free: the server can answer
/// "unchanged" without decoding anything.
///
/// ponytail: covers the segment's identity and its alignment — a re-alignment moves the boundaries and
/// therefore the bytes, and must invalidate both. It does NOT hash the source file's contents, so
/// overwriting a source WAV in place while keeping its path would serve stale bytes for up to the
/// cache's lifetime. That is out of reach of the app itself (imports are write-once under a
/// content-addressed name), and hashing hundreds of MB per request to close it would cost far more
/// than it buys. Add a mtime/len check here if sources ever become mutable.
fn audio_fingerprint(seg: &crate::db::SpeechSegment) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seg.id.hash(&mut h);
    seg.audio_path.hash(&mut h);
    seg.alignment_json.hash(&mut h);
    seg.duration_ms.hash(&mut h);
    h.finish()
}

/// Materialised clip bytes, newest last. Bounded by total SIZE, not entry count — clips vary.
///
/// What a repeat `/api/audio` hit actually costs without this, read out of audio.rs rather than
/// assumed: `decode_to_pcm` consults an LRU of decoded source PCM, but its key is
/// `pcm_cache_key` — which OPENS THE SOURCE FILE AND BLAKE3-HASHES ALL OF IT before the cache can be
/// consulted at all. So every request re-reads the whole source (the owner's is 172 MB), then clones
/// the entire decoded PCM out of the LRU (`cached.clone()`, another ~172 MB memcpy), then re-slices the
/// clip and re-encodes the WAV sample by sample. The decode itself is cached; nothing else on that path
/// is. Caching the finished bytes here skips all of it.
///
/// It matters because the page asks for the same clip up to three times — the `<audio>` element, the
/// waveform's `decodeAudioData`, and the prefetch — and range support adds a fourth (Safari opens media
/// with a 2-byte probe). Immutable caching now lets the browser elide most of those, but a reviewer
/// replaying a clip, or a second reviewer meeting the same spot check, still arrives here.
static AUDIO_CACHE: Mutex<Vec<(u64, Arc<Vec<u8>>)>> = Mutex::new(Vec::new());

/// ~32 MB, i.e. roughly 80 clips at the measured 300-500 KB each: a whole batch plus its spot checks.
const AUDIO_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// Cached clip bytes, materialising on a miss. The decode happens OUTSIDE the cache lock: it takes
/// seconds on a long source, and holding the lock through it would serialise every reviewer's audio
/// behind one clip. Two racing misses for the same clip therefore both decode and the second simply
/// replaces the first — wasteful once, never wrong.
fn cached_audio(fp: u64, seg: &crate::db::SpeechSegment) -> Result<Arc<Vec<u8>>, String> {
    {
        let mut cache = lock_audio_cache();
        if let Some(pos) = cache.iter().position(|(k, _)| *k == fp) {
            let hit = cache.remove(pos);
            let bytes = Arc::clone(&hit.1);
            cache.push(hit); // newest last
            return Ok(bytes);
        }
    }
    let bytes = Arc::new(crate::agentic::segment_audio_as_wav_bytes(seg).map_err(|e| e.to_string())?);
    let mut cache = lock_audio_cache();
    cache.retain(|(k, _)| *k != fp);
    cache.push((fp, Arc::clone(&bytes)));
    let mut total: usize = cache.iter().map(|(_, b)| b.len()).sum();
    while total > AUDIO_CACHE_BYTES && cache.len() > 1 {
        total -= cache.remove(0).1.len(); // evict oldest
    }
    Ok(bytes)
}

/// Same poisoning stance as `lock_state`: a panic elsewhere must not take the audio route down.
fn lock_audio_cache() -> std::sync::MutexGuard<'static, Vec<(u64, Arc<Vec<u8>>)>> {
    AUDIO_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A single `bytes=` range against a known body length, per RFC 9110.
///
/// Deliberately narrow: ONE range only, and a multi-range request falls back to the full body (a legal
/// answer — 206 multipart is optional, and no media element asks for it). Returns None for anything
/// unsatisfiable so the caller can answer 416 rather than guess.
fn parse_range(spec: &str, len: usize) -> Option<(usize, usize)> {
    let spec = spec.trim().strip_prefix("bytes=")?;
    if len == 0 || spec.contains(',') {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (start, end) = match (first.trim(), last.trim()) {
        // "bytes=-N": the LAST n bytes. N >= len means the whole body, not an error. N == 0 falls out
        // as unsatisfiable on its own (start == len fails the guard below), which is what RFC 9110
        // requires — an earlier `.max(1)` here quietly turned it into a request for the final byte.
        ("", n) => (len.saturating_sub(n.parse::<usize>().ok()?), len - 1),
        // "bytes=N-": from N to the end.
        (n, "") => (n.parse::<usize>().ok()?, len - 1),
        (a, b) => (a.parse::<usize>().ok()?, b.parse::<usize>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

#[derive(Clone, Copy)]
enum AudioAssignment {
    Work,
    HiddenCheck { served_in_pilot: bool, distinct_pilot_keys: usize },
}

fn forget_work_audio_assignment(state: &Mutex<CouchState>, id: &str, reviewer: &str) {
    let mut guard = lock_state(state);
    if guard.leases.get(id).is_some_and(|(who, _)| who == reviewer) {
        guard.leases.remove(id);
    }
    guard.served_work.remove(&(id.to_string(), reviewer.to_string()));
}

/// Resolve the in-memory proof that this exact reviewer was handed this exact object. A live lease
/// by itself is intentionally insufficient: renew is allowed to reclaim an expired assignment, so
/// treating any lease as delivery would turn that reliability endpoint into an object-id oracle.
fn audio_assignment(id: &str, reviewer: &str, state: &Mutex<CouchState>) -> Result<AudioAssignment, Reply> {
    let key = (id.to_string(), reviewer.to_string());
    let mut guard = lock_state(state);
    let holder = guard.holder(id, Instant::now()).map(str::to_string);
    let skipped = guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(id));
    if skipped {
        if holder.as_deref() == Some(reviewer) {
            guard.leases.remove(id);
        }
        guard.served_work.remove(&key);
        return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
    }
    if holder.as_deref() == Some(reviewer) && guard.served_work.contains(&key) {
        return Ok(AudioAssignment::Work);
    }
    if guard.spot_checks.contains(&key) {
        let served_in_pilot = guard.pilot_spot_checks.contains(&key);
        let distinct_pilot_keys = guard
            .pilot_spot_checks
            .iter()
            .filter(|(_, who)| who.eq_ignore_ascii_case(reviewer))
            .map(|(segment_id, _)| segment_id)
            .collect::<HashSet<_>>()
            .len();
        return Ok(AudioAssignment::HiddenCheck { served_in_pilot, distinct_pilot_keys });
    }
    Err(err_reply(403, "audio is not assigned to this reviewer — reload your queue"))
}

/// Revalidate every policy that made a queue item servable before returning its audio. Authentication
/// answers who the caller is; this is the separate object-level authorization boundary that answers
/// whether that reviewer may fetch this segment NOW.
fn authorize_audio(db: &Database, id: &str, reviewer: &str, state: &Mutex<CouchState>) -> Result<SpeechSegment, Reply> {
    let assignment = audio_assignment(id, reviewer, state)?;
    let (allowed_dialects, focus) = reviewer_policy(reviewer, state).map_err(|error| err_reply(503, &error))?;
    let pilot_policy = active_pilot_policy(reviewer, state).map_err(|error| err_reply(503, &error))?;

    match (assignment, pilot_policy.as_ref()) {
        (AudioAssignment::Work, Some(policy)) => {
            // Serialize the live cap read with in-process pilot decisions. The database remains the
            // cross-connection authority; this closes the local last-slot/read race without holding
            // the guard while audio is decoded.
            let _pilot_guard = lock_pilot_decision_commit();
            match pilot_remaining_slots(db, reviewer, policy) {
                Ok(0) => {
                    forget_work_audio_assignment(state, id, reviewer);
                    return Err(err_reply(
                        403,
                        "controlled review pilot is complete — no more work audio is authorized",
                    ));
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(err_reply(503, &format!("controlled review pilot is unavailable: {error}")));
                }
            }
        }
        (AudioAssignment::HiddenCheck { served_in_pilot, distinct_pilot_keys }, Some(policy)) => {
            let quota = pilot_spot_check_quota(policy, reviewer)
                .map_err(|error| err_reply(503, &format!("controlled review pilot is unavailable: {error}")))?;
            if !served_in_pilot {
                return Err(err_reply(403, "this hidden-check audio is outside the active pilot"));
            }
            if distinct_pilot_keys > quota {
                return Err(err_reply(503, "controlled hidden-check authorization is inconsistent"));
            }
            // Hidden-only catch-up is deliberately still playable after the corpus-action cap. Those
            // checks are the evidence required to finish the bounded pilot; they consume no corpus slot.
        }
        _ => {}
    }

    let seg = match db.get_segment_by_id(id) {
        Ok(Some(seg)) => seg,
        Ok(None) => {
            if matches!(assignment, AudioAssignment::Work) {
                forget_work_audio_assignment(state, id, reviewer);
            }
            return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
        }
        Err(error) => {
            tracing::error!("Couch Review audio authorization lookup failed: {error}");
            return Err(err_reply(503, "audio authorization is temporarily unavailable"));
        }
    };

    let eligible = match assignment {
        AudioAssignment::Work => {
            let raw = seg.raw_transcript.trim();
            !seg.verified
                && !raw.is_empty()
                && !raw.starts_with('[')
                && Path::new(&seg.audio_path).is_file()
                && reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &seg)
        }
        AudioAssignment::HiddenCheck { .. } => {
            let completed = db.has_spot_check_result(id, reviewer).map_err(|error| {
                tracing::error!("Couch Review hidden-check authorization lookup failed: {error}");
                err_reply(503, "audio authorization is temporarily unavailable")
            })?;
            let expected = (!completed).then(|| crate::quality::human_verified_text(&seg)).flatten();
            !completed
                && seg.verified
                && !seg.raw_transcript.is_empty()
                && (seg.is_gold || seg.reviewed_by.is_none())
                && Path::new(&seg.audio_path).is_file()
                && reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &seg)
                && expected.is_some_and(|answer| {
                    crate::normalizer::learning_text_key(answer)
                        != crate::normalizer::learning_text_key(&seg.raw_transcript)
                })
        }
    };
    if !eligible {
        if matches!(assignment, AudioAssignment::Work) {
            forget_work_audio_assignment(state, id, reviewer);
        }
        return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
    }

    // Close state races between the first receipt lookup and the database/policy reads above. A
    // completed decision removes the lease; a skip records its exclusion. Neither may be followed by
    // a conditional 304 that silently re-authorizes cached biometric bytes.
    let still_assigned = {
        let key = (id.to_string(), reviewer.to_string());
        let mut guard = lock_state(state);
        let skipped = guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(id));
        match assignment {
            AudioAssignment::Work => {
                !skipped
                    && guard.holder(id, Instant::now()).is_some_and(|who| who == reviewer)
                    && guard.served_work.contains(&key)
            }
            AudioAssignment::HiddenCheck { .. } => {
                !skipped
                    && guard.spot_checks.contains(&key)
                    && (pilot_policy.is_none() || guard.pilot_spot_checks.contains(&key))
            }
        }
    };
    if !still_assigned {
        return Err(err_reply(403, "audio is no longer assigned to this reviewer — reload your queue"));
    }
    Ok(seg)
}

/// Authorization failures must never become shared or reusable cache entries either.
fn private_audio_failure(mut reply: Reply) -> Reply {
    reply.3.push(("Cache-Control", "private, no-store".to_string()));
    reply.3.push(("Vary", "Cookie".to_string()));
    reply
}

/// Clip audio, authorization-revalidated, cache-efficient and range-capable.
///
/// `private, no-cache` permits the browser to retain the biometric bytes but requires every reuse to
/// cross this live authorization boundary. A still-authorized replay remains cheap through the ETag
/// 304; a completed/revoked/out-of-policy assignment is refused before the validator is considered.
fn api_audio(
    db: &Database,
    raw_id: &str,
    reviewer: &str,
    state: &Mutex<CouchState>,
    range: Option<&str>,
    if_none_match: Option<&str>,
) -> Reply {
    let id = raw_id.split('?').next().unwrap_or(raw_id);
    if crate::validation::input::validate_identifier(id).is_err() {
        return private_audio_failure(err_reply(400, "bad id"));
    }
    let seg = match authorize_audio(db, id, reviewer, state) {
        Ok(seg) => seg,
        Err(reply) => return private_audio_failure(reply),
    };
    let etag = format!("\"{:016x}\"", audio_fingerprint(&seg));
    // Storage is private and every reuse revalidates the live cookie + assignment. `Vary: Cookie`
    // prevents a cache entry authorized for one reviewer session being selected for another; ETag
    // keeps an authorized replay at zero audio bytes on the wire.
    let base = || {
        vec![
            ("Cache-Control", "private, no-cache".to_string()),
            ("Vary", "Cookie".to_string()),
            ("ETag", etag.clone()),
            ("Accept-Ranges", "bytes".to_string()),
        ]
    };
    // A conditional hit is answered without touching the decoder — the whole point of deriving the
    // ETag from the fingerprint rather than from the bytes. Weak-comparison prefix and `*` both count,
    // and a multi-value If-None-Match is split rather than compared whole.
    if if_none_match.is_some_and(|inm| {
        inm.split(',').any(|c| {
            let c = c.trim();
            c == "*" || c == etag || c.strip_prefix("W/").is_some_and(|c| c == etag)
        })
    }) {
        return (304, "audio/wav", Vec::new(), base());
    }
    let bytes = match cached_audio(audio_fingerprint(&seg), &seg) {
        Ok(bytes) => bytes,
        Err(e) => return private_audio_failure(err_reply(500, &e)),
    };
    let len = bytes.len();
    match range {
        None => (200, "audio/wav", bytes.as_ref().clone(), base()),
        Some(spec) => match parse_range(spec, len) {
            Some((start, end)) => {
                let mut headers = base();
                headers.push(("Content-Range", format!("bytes {start}-{end}/{len}")));
                (206, "audio/wav", bytes[start..=end].to_vec(), headers)
            }
            // Unsatisfiable, and saying so is required: a client that gets a 200 full body here
            // believes its range was honoured and reads the wrong offsets.
            None => {
                let mut headers = base();
                headers.push(("Content-Range", format!("bytes */{len}")));
                (416, "text/plain; charset=utf-8", b"range not satisfiable".to_vec(), headers)
            }
        },
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
/// Reclaiming an unheld clip is allowed only when `/api/queue` actually delivered it to this
/// reviewer: if the lease lapsed but nobody else took it, the page that still has it open should keep
/// it. A bearer presenting an arbitrary known id must not be able to mint that delivery proof.
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
    let key = (parsed.id.clone(), reviewer.to_string());
    if guard.skipped.get(reviewer).is_some_and(|ids| ids.contains(&parsed.id)) {
        return err_reply(409, "this clip is no longer assigned — reload your queue");
    }
    if guard.holder(&parsed.id, now).is_some_and(|who| who != reviewer) {
        // Someone else took it while this reviewer was away. Telling them NOW — rather than at save —
        // is the whole point: they can still copy their correction before it is refused.
        return err_reply(409, "another reviewer is working on this clip");
    }
    if !guard.served_work.contains(&key) {
        // Hidden checks are deliberately not leased. Their exact served receipt is enough for the
        // page heartbeat to succeed, while audio and decision paths independently revalidate that
        // the key is still outstanding and policy-eligible.
        if guard.spot_checks.contains(&key) {
            return json_reply(200, serde_json::json!({ "ok": true, "ttlSeconds": LEASE_TTL.as_secs() }));
        }
        return err_reply(409, "this clip was not served to this reviewer — reload your queue");
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
    let served_ids: HashSet<String> =
        guard.served_work.iter().filter(|(_, who)| who == reviewer).map(|(id, _)| id.clone()).collect();
    for (id, (who, granted)) in guard.leases.iter_mut() {
        if who == reviewer && served_ids.contains(id) {
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
/// This deliberately does NOT look at `verified`. Current phone decisions finalize atomically, but a
/// row created by an older release can still contain a committed decision without phone finalization.
/// Recognizing that legacy half-row lets replay finish only annotation/verification without minting a
/// duplicate learning pair. Callers pair this with `prev.verified` to distinguish complete repeats.
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
    /// Client-authored identity for this one human operation. The phone persists it before its first
    /// POST and reuses it byte-for-byte on every retry, including after an app/server restart.
    #[serde(default, rename = "operationId")]
    operation_id: Option<String>,
    id: String,
    /// "accept" | "edit" | "bad" | "skip" (the last writes nothing — see `api_decision`)
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
    /// The row's monotonic database revision as it was when the clip was SERVED. The
    /// queue payload carries it and the page echoes it back, so a decision made against a draft
    /// that a background writer replaced in between is refused rather than recorded — the accept/
    /// edit classification and the DPO pair are only meaningful against the text the reviewer saw.
    /// Required for every verdict. `skip` is the sole exemption because it writes no judgement.
    #[serde(default, rename = "rowVersion")]
    row_version: Option<String>,
    /// Immutable controlled-pilot baseline carried by the queue item.  Old localStorage outboxes
    /// cannot acquire it retroactively, so a pre-activation operation must reload before it can write.
    #[serde(default, rename = "pilotAfterReviewEventId")]
    pilot_after_review_event_id: Option<i64>,
    /// Cumulative MEDIA time the page actually advanced through this clip, in ms.
    ///
    /// Not wall-clock, not a `play()` call, not a download — a clip can arrive, decode and sit at
    /// 0:00 while nobody hears a word. The page only reports how much it played; the SERVER resolves
    /// the segment's revision and audio fingerprint itself, so a client cannot mint evidence naming a
    /// clip or revision it never loaded.
    #[serde(default, rename = "heardMs")]
    heard_ms: Option<i64>,
    #[serde(default, rename = "clipDurationMs")]
    clip_duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewOperationState {
    New,
    ExactReplay,
    Reused,
}

/// Hash the exact financial operation contract, not the server's later provenance classification.
/// In particular, a submitted `edit` that is materially unchanged may be paid as an `accept`, but
/// changing `edit` to `accept` while reusing the UUID is still a different client operation and must
/// be rejected. Length framing makes the tuple unambiguous for arbitrary Unicode transcript text.
fn decision_operation_payload_hash(segment_id: &str, action: &str, text: &str, reviewer: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cortex-review-operation-v1");
    for value in [segment_id, action, text, reviewer] {
        let bytes = value.as_bytes();
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

fn review_operation_state(
    db: &Database,
    operation_id: &str,
    operation_payload_hash: &str,
    segment_id: &str,
    reviewer: &str,
) -> crate::error::AppResult<ReviewOperationState> {
    let Some(receipt) = db.review_operation(operation_id)? else {
        return Ok(ReviewOperationState::New);
    };
    if receipt.operation_payload_hash == operation_payload_hash
        && receipt.segment_id == segment_id
        && receipt.reviewer == reviewer
    {
        Ok(ReviewOperationState::ExactReplay)
    } else {
        Ok(ReviewOperationState::Reused)
    }
}

/// A UNIQUE conflict can mean another identical retry committed while this request was in flight.
/// Re-read the immutable receipt before deciding whether a write error is success, conflict, or a
/// genuine retryable failure. This is the post-write half of idempotency; the preflight alone has a
/// check-then-insert race.
fn operation_result_after_write_failure(
    db: &Database,
    reviewer: &str,
    segment_id: &str,
    operation_id: &str,
    operation_payload_hash: &str,
    fallback_status: u16,
    fallback_message: &str,
) -> Reply {
    match review_operation_state(db, operation_id, operation_payload_hash, segment_id, reviewer) {
        Ok(ReviewOperationState::ExactReplay) => {
            json_reply_with_accounting(200, serde_json::json!({ "ok": true, "duplicate": true }), db, reviewer)
        }
        Ok(ReviewOperationState::Reused) => err_reply(409, "operation UUID is already bound to another decision"),
        Ok(ReviewOperationState::New) => err_reply(fallback_status, fallback_message),
        Err(error) => err_reply(500, &format!("{fallback_message}; operation receipt lookup failed: {error}")),
    }
}

/// Record one phone decision through the shared human-decision path. Verdict, provenance/learning
/// effects, annotation, and verification commit atomically; an accepted response can never leave a
/// decided-but-pending row. The decision is attributed to `reviewer`, and refused outright on a clip
/// another reviewer currently holds.
///
/// `action: "skip"` is the one path that writes NOTHING to the corpus — see the block that handles it.
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

    // Validate and look up a supplied operation identity before consulting mutable corpus state. An
    // exact durable receipt is sufficient proof that this request already committed, even if the app
    // restarted, the row revision advanced, the lease expired, or the current focus later changed.
    // Conversely, a bound UUID with ANY different contract is a hard conflict, never a fresh write.
    let operation_payload_hash = if let Some(operation_id) = parsed.operation_id.as_deref() {
        let Ok(uuid) = uuid::Uuid::parse_str(operation_id) else {
            return err_reply(400, "operationId must be a canonical UUID");
        };
        if uuid.hyphenated().to_string() != operation_id {
            return err_reply(400, "operationId must be a lowercase hyphenated UUID");
        }
        let payload_hash = decision_operation_payload_hash(&parsed.id, &parsed.action, &parsed.text, reviewer);
        match review_operation_state(db, operation_id, &payload_hash, &parsed.id, reviewer) {
            Ok(ReviewOperationState::ExactReplay) => {
                return json_reply_with_accounting(
                    200,
                    serde_json::json!({ "ok": true, "duplicate": true }),
                    db,
                    reviewer,
                );
            }
            Ok(ReviewOperationState::Reused) => {
                return err_reply(409, "operation UUID is already bound to another decision");
            }
            Ok(ReviewOperationState::New) => {}
            Err(error) => return err_reply(500, &format!("operation receipt lookup failed: {error}")),
        }
        Some(payload_hash)
    } else {
        None
    };
    let (prev, request_revision) = match db.get_segment_by_id_with_revision(&parsed.id) {
        Ok(Some(row)) => row,
        Ok(None) => return err_reply(404, "no such segment"),
        Err(e) => return err_reply(500, &e.to_string()),
    };
    // Hidden checks are deliberately served with their RAW, known-wrong draft even when the row also
    // carries its human answer in annotated_transcript. Pay/action classification must describe what
    // the reviewer did to the text they actually saw; QC correctness is scored separately against the
    // answer key below. Keeping those two baselines distinct prevents an attentive correction from
    // being paid as a 10% accept and a blind raw accept from being paid as a 100% edit.
    let was_served_as_spot_check = {
        let guard = lock_state(state);
        guard.spot_checks.contains(&(parsed.id.clone(), reviewer.to_string()))
    };

    let (decision, text): (&str, Option<String>) = if parsed.action == "skip" {
        ("skip", None)
    } else {
        match parsed.action.as_str() {
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
                // Pay/corpus semantics follow a MATERIAL transcript change, not byte trivia. NFC
                // composition and collapsed whitespace are the same words under the project's own
                // no-op learning key; treating either as an edit would let a formatting-only change
                // earn the 100% correction rate instead of the authorized 10% accept rate.
                let submitted_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(&text));
                let served_text =
                    if was_served_as_spot_check { prev.raw_transcript.clone() } else { review_text(&prev) };
                let served_key = crate::normalizer::learning_text_key(&crate::db::to_nfc(served_text.trim()));
                let is_edit = submitted_key != served_key;
                (if is_edit { "edit" } else { "accept" }, Some(text))
            }
            "bad" => ("reject", None),
            other => return err_reply(400, &format!("unknown action '{other}'")),
        }
    };

    // A dropped RESPONSE (not a dropped request) means the write landed and the page never heard so.
    // Answer the retry with the success it already earned, before taking a lease, minting a second
    // receipt, or pushing an undo entry for a decision that is already recorded.
    //
    // `already_recorded` without `verified` is the HALF-WRITTEN state: write one committed and the row
    // upsert did not. That is not a duplicate to be waved through — the clip is still unverified, so it
    // would be served as pending work forever — and it is not new work either, because replaying write
    // one would double the learning pair. It is an interrupted write to be FINISHED, below.
    let already_recorded =
        parsed.action != "skip" && is_repeat_of_stored_decision(&prev, reviewer, decision, text.as_deref());
    if already_recorded && prev.verified {
        // BOUNDED ROLLING-COMPATIBILITY EXCEPTION. The immediately previous page can have committed a
        // verdict before operation UUIDs existed, lost the response, then acquire a UUID only while
        // its legacy localStorage array is upgraded. There is no truthful event to which that later
        // UUID can be attached without mutating immutable financial history, so this pure ACK does
        // not reserve it. The page deletes that one-use UUID immediately; this branch writes no row,
        // event, ledger entry, receipt, or undo state and therefore cannot duplicate compensation.
        // Carries the total as well: this is the RETRY path after a dropped response, which is
        // exactly when a reviewer is least sure their work registered.
        forget_work_audio_assignment(state, &parsed.id, reviewer);
        return json_reply_with_accounting(200, serde_json::json!({ "ok": true, "duplicate": true }), db, reviewer);
    }

    // A completed hidden check never changes the corpus row, so the corpus duplicate predicate above
    // cannot recognize its retry. The served-check set persists across restart and the DB result is
    // first-answer immutable; once both say this reviewer already answered, every replay is a pure ACK.
    // This must be before the new-write rowVersion guard so an outbox created by the previous page build
    // can drain, and before `audit` so retries cannot append another pay-shaped generic review event.
    let still_has_answer_key = prev.verified
        && !crate::quality::is_human_rejected(&prev)
        && crate::quality::human_verified_text(&prev).is_some();
    if was_served_as_spot_check && still_has_answer_key {
        match db.has_spot_check_result(&parsed.id, reviewer) {
            Ok(true) => {
                return json_reply_with_accounting(200, serde_json::json!({ "ok": true }), db, reviewer);
            }
            Ok(false) => {}
            Err(error) => return err_reply(500, &format!("spot-check retry lookup failed: {error}")),
        }
    }

    // A NEW (or half-written) verdict without the serve-time revision has no proof of which draft the
    // reviewer judged. Reject it before minting playback evidence, claiming a lease, scoring a hidden
    // check, writing an audit event, or touching the corpus. The completed identical retry above is
    // deliberately earlier: acknowledging bytes that are already durably stored writes nothing and
    // lets an outbox created by the immediately previous phone build drain safely after deployment.
    // Spot checks still require a parseable stamp, though their stamp is not compared for freshness
    // because grading never mutates the answer-key row. A skip remains the only mutating-flow
    // exemption because it writes no verdict.
    let served_revision = if parsed.action == "skip" {
        None
    } else {
        let Some(stamp) = parsed.row_version.as_deref() else {
            return err_reply(400, "rowVersion is required — reload this clip before deciding");
        };
        let Ok(revision) = stamp.parse::<i64>() else {
            return err_reply(400, "rowVersion is invalid — reload this clip before deciding");
        };
        Some(revision)
    };

    // WRITE-TIME AUTHORIZATION. Queue filtering alone is not a security boundary: an outbox can
    // retain an old id after the owner changes reviewer_dialects.json / voice_focus.json, and a valid
    // bearer can POST any known id without fetching `/api/queue`. Re-read both hot policies for every
    // non-duplicate submit and refuse before ANY state or database write. An identical lost-response
    // retry above is only an acknowledgement of a write that already committed, so it needs no new
    // authorization and remains idempotent.
    let (allowed_dialects, focus) = match reviewer_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(e) => return err_reply(503, &e),
    };
    let pilot_policy = match active_pilot_policy(reviewer, state) {
        Ok(policy) => policy,
        Err(error) => return err_reply(503, &error),
    };
    match pilot_policy.as_ref() {
        Some(policy) if parsed.pilot_after_review_event_id != Some(policy.after_review_event_id) => {
            return err_reply(409, "controlled review pilot changed — reload the queue before deciding")
        }
        None if parsed.pilot_after_review_event_id.is_some() => {
            return err_reply(409, "controlled review pilot is no longer active — reload the queue before deciding")
        }
        _ => {}
    }
    if !reviewer_policy_allows(allowed_dialects.as_deref(), focus.as_deref(), &prev) {
        // If policy changed after this reviewer was served the clip, release only THEIR lease so it
        // can immediately return to an eligible reviewer. Never disturb somebody else's live work.
        let mut guard = lock_state(state);
        if guard.holder(&parsed.id, Instant::now()).is_some_and(|who| who == reviewer) {
            guard.leases.remove(&parsed.id);
        }
        guard.served_work.remove(&(parsed.id.clone(), reviewer.to_string()));
        return err_reply(403, "this clip is outside your current review assignment — reload your queue");
    }

    // SPOT-CHECK DETECTION, hoisted above the version fence (2026-08-20 hunt). Grading a check
    // writes NOTHING to the corpus row, so a revision bump between serve and submit cannot
    // invalidate it — and bulk metadata runs (a rights stamp, a re-annotation) bump every GOLD
    // row's revision at once, which would otherwise 409 every check in flight and cost the honesty
    // measurement its scores. The full staleness rationale for the key itself is on the grading
    // block below.
    let (expected_key, invalid_pilot_key, served_in_this_pilot): (Option<String>, bool, bool) = {
        let mut guard = lock_state(state);
        let key = (parsed.id.clone(), reviewer.to_string());
        if !guard.spot_checks.contains(&key) {
            (None, false, false)
        } else {
            let served_in_this_pilot = guard.pilot_spot_checks.contains(&key);
            let answer = if crate::quality::is_human_rejected(&prev) {
                None
            } else {
                crate::quality::human_verified_text(&prev)
            };
            if let Some(answer) = answer {
                (Some(answer.to_string()), false, served_in_this_pilot)
            } else if served_in_this_pilot {
                // Replacing this bounded pilot key would consume a third distinct key; treating the
                // submit as ordinary work would silently erase the measurement. Pause instead.
                (None, true, true)
            } else {
                // The answer key is gone — the thing that made this a check. Drop the stale pair so it
                // cannot swallow this clip again, and treat the submit as what it is: real work.
                guard.spot_checks.remove(&key);
                tracing::info!(
                    "Couch Review: stale spot check for {} dropped — the human answer key is gone",
                    parsed.id
                );
                (None, false, false)
            }
        }
    };
    if invalid_pilot_key {
        return err_reply(503, "Review is temporarily paused: a controlled hidden-check key is no longer valid");
    }
    if pilot_policy.is_some() && expected_key.is_some() && !served_in_this_pilot {
        return err_reply(409, "controlled review pilot requires a fresh queue before this check can be submitted");
    }
    if pilot_policy.is_some() && expected_key.is_none() {
        let mut guard = lock_state(state);
        if !guard.holder(&parsed.id, Instant::now()).is_some_and(|who| who == reviewer) {
            return err_reply(409, "controlled review pilot requires this work to be served first — reload the queue");
        }
    }

    // SERVE/DECIDE VERSION FENCE (text-provenance audit #4). The queue stamped this clip's
    // monotonic revision into the payload; if the row changed between serve and submit (batch
    // re-transcribe, refine loop, desktop edit — all of which target exactly the unverified rows
    // the couch serves), the reviewer judged text that no longer exists: recording it would
    // misclassify accept-vs-edit against the NEW row and mint a DPO pair anti-training the fresher
    // draft. Refuse; the page reloads and serves the current text.
    //
    // Placed BEFORE the receipt mint (audit fix 2026-08-20). The receipt's revision, fingerprint
    // and duration are all resolved from the row, so minting one for a submit whose serve predates
    // a re-chunk would bind this reviewer's real listening to audio they never heard — evidence
    // manufactured by ordering. A stale serve must be refused while it is still only a claim.
    //
    // Three exemptions, each because the fence would refuse an act that cannot conflict:
    //   * a replay finishing an already-recorded decision — its write one landed, so the stamp has
    //     legitimately moved;
    //   * a SKIP — it writes no verdict, and fencing it boomeranged the clip back to the same
    //     reviewer forever (the lease is kept on a 409) while the offline replay of the skip was
    //     dropped into the "work lost" banner, all for an act with nothing to be stale about;
    //   * a SPOT CHECK — see above.
    // Neither exemption can manufacture evidence: the mint below re-verifies the revision
    // atomically and simply declines the receipt when the row has moved.
    if !already_recorded
        && parsed.action != "skip"
        && expected_key.is_none()
        && served_revision != Some(request_revision)
    {
        return err_reply(409, "this clip changed since it was served — reload for the fresh draft");
    }

    // Rolling-deploy exception: completed legacy verdict/check retries above are pure ACKs and may
    // omit this new field. Every operation that can still write anything must have its durable client
    // UUID before the first side effect (including a playback receipt and the zero-credit skip event).
    let Some(operation_id) = parsed.operation_id.as_deref() else {
        return err_reply(400, "operationId is required — reload this page before deciding");
    };
    let Some(operation_payload_hash) = operation_payload_hash.as_deref() else {
        return err_reply(400, "operationId payload could not be validated — reload this page before deciding");
    };

    // Serialize the last pre-receipt check with the database commit. The database repeats the cap
    // check under BEGIN IMMEDIATE (the cross-connection authority); this outer lock ensures a losing
    // in-process request leaves no playback-receipt side effect before that transaction refuses it.
    // A skip is zero-pay and not a canary decision, but it DOES consume a pilot safety slot: otherwise
    // repeated skips could refill queues and burn hidden keys beyond the ten-action capacity proof.
    // Ordinary spot-check answers do not consume a slot because they are measurement, not new work.
    let mut pilot_decision_limit: Option<ReviewDecisionLimit> = None;
    let _pilot_commit_guard = if parsed.action == "skip" || expected_key.is_none() {
        let guard = lock_pilot_decision_commit();
        let current = match active_pilot_policy(reviewer, state) {
            Ok(policy) => policy,
            Err(error) => return err_reply(503, &error),
        };
        if current != pilot_policy {
            return err_reply(503, "controlled review policy changed while this decision was being checked");
        }
        if let Some(policy) = current.as_ref() {
            match pilot_remaining_slots(db, reviewer, policy) {
                Ok(0) => {
                    return err_reply(409, "controlled review pilot complete — no more review actions are authorized")
                }
                Ok(_) => {}
                Err(error) => return err_reply(503, &format!("controlled review pilot is unavailable: {error}")),
            }
            pilot_decision_limit = match build_pilot_decision_limit(policy) {
                Ok(limit) => Some(limit),
                Err(error) => return err_reply(503, &error),
            };
        }
        Some(guard)
    } else {
        None
    };

    // LISTENING EVIDENCE — minted, then ENFORCED, before anything is written.
    //
    // Order matters and is not obvious: the page reports its heard-time WITH the decision, so the
    // receipt for this very submit has to be recorded before the guard can possibly pass. Checking
    // first would refuse every decision forever. The revision and the fingerprint are still resolved
    // server-side, and the coverage denominator is the server's own clip length, so neither half of
    // the ratio is the client's to assert.
    //
    // A `skip` is exempt and always was: it writes no verdict, no attribution and no `verified`, so
    // there is no claim to stand behind. It is the honest answer for a clip the reviewer cannot
    // judge, and refusing it would leave someone who could not hear a clip with no legal move at all.
    // A receipt is still minted for it — a clip that was heard and then skipped is real evidence.
    //
    // A REJECT is NOT exempt (owner decision 2026-08-19, after the pilot measured four rejects in two
    // seconds with zero playback). An accept or an edit injects text; a reject permanently removes a
    // clip from a corpus whose size is the binding constraint. Both are verdicts, and a verdict on a
    // clip nobody heard is a guess either way.
    let now_ms =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let fingerprint =
        db.segment_audio_fingerprint(&parsed.id).ok().flatten().unwrap_or_else(|| format!("id:{}", parsed.id));
    // The revision this receipt binds to. For a decision it is the one the fence just verified
    // against the serve. For a SPOT CHECK it is the row as it stands NOW: bulk stamps bump gold
    // rows' revisions without touching the audio, the fence exempts checks for exactly that
    // reason, and the receipt + evidence check must agree on one value or an honest listener
    // would be refused.
    let revision = if expected_key.is_some() {
        db.segment_review_revision(&parsed.id).ok().flatten().unwrap_or(request_revision)
    } else {
        request_revision
    };
    // A skip whose SERVE is already stale mints nothing at all: the skip itself still lands (it
    // writes no verdict), but the mint below resolves identity from the CURRENT row, and listening
    // that happened on a serve the fence can no longer verify must not become evidence bound to a
    // row state the reviewer never saw.
    let stale_skip = parsed.action == "skip"
        && parsed
            .row_version
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|served| served != request_revision);
    if stale_skip {
        tracing::info!("skip on {} kept, receipt declined: the serve predates the current row", parsed.id);
    } else if let Some(heard_ms) = parsed.heard_ms {
        let receipt = crate::db::PlaybackReceipt {
            segment_id: parsed.id.clone(),
            segment_revision: revision,
            audio_fingerprint: fingerprint.clone(),
            reviewer: Some(reviewer.to_string()),
            session_id: None,
            started_at_ms: now_ms,
            played_ms: heard_ms.max(0),
            clip_duration_ms: parsed.clip_duration_ms.unwrap_or(0).max(0),
        };
        // Atomic check-and-mint (2026-08-20 hunt): the front door's own resolution re-queried the
        // row AFTER the fence, so a write landing in between rebound the receipt to a revision the
        // reviewer never heard. The mint now verifies the revision in the same statement that
        // inserts — fingerprint and duration are resolved from exactly the verified row.
        match db.record_playback_receipt_if_at_revision(&receipt, revision) {
            Ok(true) => {}
            Ok(false) if parsed.action == "skip" => {
                // The row moved between fetch and mint. The skip still proceeds — it writes no
                // verdict — but WITHOUT a receipt: evidence nobody can bind to the served audio
                // is worth less than no evidence.
                tracing::info!("skip on {} kept, receipt declined: the row moved since the serve", parsed.id);
            }
            Ok(false) => {
                // The fence firing late: same refusal, same page recovery (reload, fresh serve).
                return err_reply(409, "this clip changed since it was served — reload for the fresh draft");
            }
            Err(e) => {
                // NOT a warning to swallow any more. Under enforcement the receipt IS the reviewer's
                // proof, so losing it silently means refusing them for the server's failure: they replay
                // the clip, the write fails again, and nothing they can do will land. A busy database is
                // a 500 -- which the page already holds and retries -- not a verdict about listening.
                tracing::warn!("playback receipt not recorded for {}: {e}", parsed.id);
                // Hold the clip for THIS reviewer before giving up on it. A 428 is a rejected request and
                // deliberately leaves no lease, but a 500 is the server's own failure: the reviewer still
                // has the clip open with their correction typed and now queued, and a freed clip goes to
                // the next reviewer's batch within seconds — their replay then loses a race that never
                // needed to happen. Same rule the decision write below follows for the same reason.
                {
                    let now = Instant::now();
                    let mut guard = lock_state(state);
                    if !guard.holder(&parsed.id, now).is_some_and(|who| who != reviewer) {
                        guard.leases.insert(parsed.id.clone(), (reviewer.to_string(), now));
                    }
                }
                return err_reply(500, &format!("playback receipt not recorded: {e}"));
            }
        }
    }
    if parsed.action != "skip" {
        match db.has_sufficient_playback_evidence(&parsed.id, revision, &fingerprint, Some(reviewer)) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("PLAYBACK_EVIDENCE_REFUSED: {} by {reviewer} at revision {revision}", parsed.id);
                // 428 Precondition Required: the reviewer must do something first, and it is neither a
                // conflict (409) nor a malformed request (400). The page holds the clip and says so; the
                // outbox treats it as a settled answer rather than retrying what can never land.
                return err_reply(
                    428,
                    &db.require_playback_evidence(&parsed.id, revision, &fingerprint, Some(reviewer))
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "E_NO_PLAYBACK_EVIDENCE".to_string()),
                );
            }
            // NOT a verdict about the reviewer. A locked or unwell database cannot answer the
            // question, and telling someone who listened properly that they did not is both false and
            // unactionable — they would replay the clip and be refused again. 500 says "the server is
            // unwell", which the page already holds and retries.
            Err(e) => return err_reply(500, &format!("playback evidence check failed: {e}")),
        }
    }

    // SKIP — the explicit NO-VERDICT (R4.4), handled before any of the write machinery because it
    // shares none of it.
    //
    // A reviewer who genuinely cannot call a clip — two people talking over each other, an accent they
    // do not have, audio that will not play — previously had exactly two ways forward, and both write a
    // judgement they cannot stand behind: "Looks good" promotes an unheard draft to gold, "Reject"
    // permanently excludes a clip that may be perfectly fine. A guess is worse for the corpus than an
    // honest "I don't know", because nothing downstream can tell the two apart. So this writes NOTHING
    // to the row — no decision, no verdict, no attribution, no `verified` — and does only two things:
    //
    //   * records the act in the audit trail, so the owner can see which clips defeated a human (a clip
    //     several reviewers skip is telling you something about the clip), and
    //   * takes it out of THIS reviewer's queue and releases their lease, so it goes to somebody who
    //     can judge it instead of being handed straight back on the next refill.
    //
    // Nothing to undo, because nothing was written; the clip is simply still pending for everyone else.
    if parsed.action == "skip" {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        if let Err(error) = db.record_review_event_with_operation_limit(
            &parsed.id,
            reviewer,
            "skip",
            "couch",
            now_ms,
            operation_id,
            operation_payload_hash,
            pilot_decision_limit.as_ref(),
        ) {
            tracing::warn!("Couch Review skip event not recorded for {}: {error}", parsed.id);
            if error.to_string().contains(REVIEW_PILOT_LIMIT_REACHED) {
                let mut guard = lock_state(state);
                if guard.leases.get(&parsed.id).is_some_and(|(who, _)| who == reviewer) {
                    guard.leases.remove(&parsed.id);
                }
                return err_reply(409, "controlled review pilot complete — no more review actions are authorized");
            }
            return operation_result_after_write_failure(
                db,
                reviewer,
                &parsed.id,
                operation_id,
                operation_payload_hash,
                500,
                "skip not recorded — retrying is safe",
            );
        }
        {
            let mut guard = lock_state(state);
            // Only OUR OWN lease. A clip another reviewer currently holds is not ours to hand back —
            // that would free their in-progress work out from under them.
            if guard.holder(&parsed.id, Instant::now()).is_some_and(|who| who == reviewer) {
                guard.leases.remove(&parsed.id);
            }
            guard.served_work.remove(&(parsed.id.clone(), reviewer.to_string()));
            guard.skipped.entry(reviewer.to_string()).or_default().insert(parsed.id.clone());
        }
        return json_reply_with_accounting(200, serde_json::json!({ "ok": true, "skipped": true }), db, reviewer);
    }

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
    // Carries the ANSWER KEY, not a bool: a check with no key cannot be graded, and returning the key
    // itself makes that structural instead of a comment (external review 2026-08-06).
    //
    // The staleness test used to be `!prev.verified`, which is strictly weaker than the predicate that
    // MINTED the key — `list_spot_check_candidates` requires `human_verified_text(&seg).is_some()`.
    // Anything that strips the human answer while leaving `verified = 1` left the pair live, and the
    // grader then did `.unwrap_or_default()`, scoring the reviewer against "". That is reachable
    // through the ordinary desktop "mark bad": it writes human_decision='reject' / verdict='human_reject'
    // while KEEPING verified, and for a key whose answer lived in verdict_transcript
    // `human_verified_text` then returns None. compute_cer("", submitted) is 1.0 for any non-empty
    // answer, so a reviewer who transcribed the clip CORRECTLY was recorded at a fabricated 1.00 CER
    // (or 0.00 for a reject) and averaged into spot_check_report with no filter — while the HTTP reply
    // was byte-identical to a real success. Test the key, not its artefact.
    if let Some(expected) = expected_key {
        let submitted = text.as_deref().unwrap_or_default();
        if let Err(e) = db.record_spot_check_with_operation(
            &parsed.id,
            reviewer,
            decision,
            submitted,
            &expected,
            operation_id,
            operation_payload_hash,
        ) {
            tracing::warn!("Couch Review spot-check not recorded for {}: {e}", parsed.id);
            // A 200 removes this decision from the phone's durable outbox forever. If the score did
            // not commit, success would silently discard the first answer and let a later attempt
            // replace the measurement. Return the same retryable 5xx class used by ordinary decision
            // write failures; keep the client-facing text generic so the hidden check stays hidden.
            // The retry is safe: record_spot_check is first-answer immutable, and the early completed-
            // check ACK above absorbs every replay once the insert has committed.
            return operation_result_after_write_failure(
                db,
                reviewer,
                &parsed.id,
                operation_id,
                operation_payload_hash,
                500,
                "decision not recorded — retrying is safe",
            );
        }
        // `record_spot_check` committed the immutable first score, audit event, and pay-ledger delta
        // together. A second best-effort audit here would re-open the kill window and double-count a
        // dropped-response retry.
        return json_reply_with_accounting(200, serde_json::json!({ "ok": true }), db, reviewer);
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
    let undo_operation_id = (!already_recorded).then(|| operation_id.to_string());
    {
        let now = Instant::now();
        let mut guard = lock_state(state);
        if guard.holder(&parsed.id, now).is_some_and(|who| who != reviewer) {
            return err_reply(409, "another reviewer is working on this clip");
        }
        if !already_recorded && !guard.in_flight_operations.insert(operation_id.to_string()) {
            return err_reply(503, "this operation is still being saved — retrying is safe");
        }
        guard.leases.insert(parsed.id.clone(), (reviewer.to_string(), now));
        // No undo entry when finishing a half-written decision: `prev` is the HALF-DECIDED row, not the
        // pre-decision one, so storing it would make the undo button restore a corrupt snapshot. The
        // entry pushed by the original attempt is the true one and is deliberately left in place.
        if let Some(undo_operation_id) = undo_operation_id.as_ref() {
            let stack = guard.undo.entry(reviewer.to_string()).or_default();
            // Two tabs may replay the same durable operation concurrently. They share one inverse;
            // never create two undo entries for the one human act.
            if !stack.iter().any(|entry| entry.operation_id == *undo_operation_id) {
                stack.push(UndoEntry {
                    operation_id: undo_operation_id.clone(),
                    seg_id: prev.id.clone(),
                    prev: prev.clone(),
                    decided_revision: None,
                });
            }
            // Bounded HERE, at the only site that grows it: the pushes in api_undo are restorations of
            // an entry just popped, so they cannot exceed what the stack already held.
            let overflow = stack.len().saturating_sub(UNDO_DEPTH);
            if overflow > 0 {
                stack.drain(0..overflow);
            }
        }
    }
    // New decisions finalize in the SAME transaction that mints the DPO pair/correction memories.
    // A legacy half-written row is finalized without replaying those side effects.
    let write_result = if already_recorded {
        db.finalize_phone_human_decision_at_revision(&parsed.id, text.as_deref(), request_revision)
    } else {
        db.record_phone_human_decision_by_at_revision_with_operation_limit(
            &parsed.id,
            decision,
            text.as_deref(),
            reviewer,
            request_revision,
            operation_id,
            operation_payload_hash,
            pilot_decision_limit.as_ref(),
        )
    };
    let decided_revision = match write_result {
        Ok(Some(revision)) => revision,
        Ok(None) => {
            if let Some(undo_operation_id) = undo_operation_id.as_deref() {
                let mut guard = lock_state(state);
                if let Some(stack) = guard.undo.get_mut(reviewer) {
                    stack.retain(|entry| entry.operation_id != undo_operation_id);
                }
            }
            let reply = operation_result_after_write_failure(
                db,
                reviewer,
                &parsed.id,
                operation_id,
                operation_payload_hash,
                409,
                "this clip changed while the decision was being saved — reload for the fresh draft",
            );
            lock_state(state).in_flight_operations.remove(operation_id);
            return reply;
        }
        Err(e) => {
            // A concurrent identical request may have won the UNIQUE race. Classify that durable
            // receipt before deleting the shared undo inverse or returning a retryable failure.
            if matches!(
                review_operation_state(db, operation_id, operation_payload_hash, &parsed.id, reviewer),
                Ok(ReviewOperationState::ExactReplay)
            ) {
                let current_revision =
                    db.get_segment_by_id_with_revision(&parsed.id).ok().flatten().map(|(_, revision)| revision);
                if let Some(revision) = current_revision {
                    if let Some(stack) = lock_state(state).undo.get_mut(reviewer) {
                        if let Some(entry) = stack.iter_mut().find(|entry| entry.operation_id == operation_id) {
                            entry.decided_revision = Some(revision);
                        }
                    }
                }
                forget_work_audio_assignment(state, &parsed.id, reviewer);
                lock_state(state).in_flight_operations.remove(operation_id);
                return json_reply_with_accounting(
                    200,
                    serde_json::json!({ "ok": true, "duplicate": true }),
                    db,
                    reviewer,
                );
            }
            if e.to_string().contains(REVIEW_PILOT_LIMIT_REACHED) {
                let mut guard = lock_state(state);
                if let Some(undo_operation_id) = undo_operation_id.as_deref() {
                    if let Some(stack) = guard.undo.get_mut(reviewer) {
                        stack.retain(|entry| entry.operation_id != undo_operation_id);
                    }
                }
                if guard.leases.get(&parsed.id).is_some_and(|(who, _)| who == reviewer) {
                    guard.leases.remove(&parsed.id);
                }
                guard.in_flight_operations.remove(operation_id);
                return err_reply(409, "controlled review pilot complete — no more review actions are authorized");
            }
            if let Some(undo_operation_id) = undo_operation_id.as_deref() {
                let mut guard = lock_state(state);
                if let Some(stack) = guard.undo.get_mut(reviewer) {
                    stack.retain(|entry| {
                        entry.operation_id != undo_operation_id
                            || entry.seg_id != parsed.id
                            || entry.decided_revision.is_some()
                    });
                }
            }
            // The lease is deliberately KEPT. The reviewer still has the clip open and their outbox can
            // retry; freeing it immediately would hand the same work to another reviewer.
            let reply = operation_result_after_write_failure(
                db,
                reviewer,
                &parsed.id,
                operation_id,
                operation_payload_hash,
                500,
                &e.to_string(),
            );
            lock_state(state).in_flight_operations.remove(operation_id);
            return reply;
        }
    };
    lock_state(state).in_flight_operations.remove(operation_id);
    // Stamp the undo entry with the row's POST-decision revision returned by the SAME transaction —
    // api_undo refuses to restore unless the row still carries exactly this value, which
    // is what makes undo safe against every intervening writer, not only re-reviews. On a resumed
    // half-written decision the original attempt's entry (revision still None) is the one completed.
    {
        let mut guard = lock_state(state);
        if let Some(stack) = guard.undo.get_mut(reviewer) {
            let entry = if let Some(undo_operation_id) = undo_operation_id.as_deref() {
                stack.iter_mut().find(|entry| entry.operation_id == undo_operation_id)
            } else {
                stack.iter_mut().rev().find(|entry| entry.seg_id == parsed.id && entry.decided_revision.is_none())
            };
            if let Some(entry) = entry {
                entry.decided_revision = Some(decided_revision);
            }
        }
    }
    // Decided, so the lease has served its purpose — release it rather than waiting out the TTL. (The
    // clip also leaves the pending queue, so this is housekeeping, not correctness.)
    forget_work_audio_assignment(state, &parsed.id, reviewer);
    // No audit() here: the review event and compensation delta committed WITH the decision, inside
    // its transaction. Refresh both the full activity total and the distinct weighted-pay total.
    json_reply_with_accounting(200, serde_json::json!({ "ok": true }), db, reviewer)
}

/// Undo the LAST phone decision: retract the learning pair the decision produced, then restore the
/// pre-decision row LOSSLESSLY. Returns the undone segment id.
///
/// `clear_human_decision` is kept ONLY for its side effect of deleting the DPO/few-shot agent_examples
/// row the edit produced (a retracted edit left in the trainable pool permanently teaches the model a
/// fix the human took back). The row-restore itself MUST go through `insert_segment_full`, not
/// `insert_segment`: `prev` is the exact pre-decision snapshot, and a clip served to the couch queue can
/// carry jury state (a jury-ESCALATED clip is unverified, so it is in `get_segments(false)`) — verdict,
/// verdict_transcript, rationale, evidence_json, agreement_score, escalated, is_gold. `insert_segment`
/// writes a 17-column subset that OMITS every one of those, so it would leave them at whatever
/// `clear_human_decision` set (jury verdict/evidence NULLed, is_gold from the undone accept still set) —
/// silently dropping the pre-decision jury verdict on undo. `insert_segment_full` rewrites the whole row
/// (including created_at) back to `prev`, so undo is a true inverse.
/// Undo pops from THIS reviewer's stack only. A single shared stack meant one reviewer's ↩ retracted
/// whichever decision happened to be last — usually someone else's, with no indication to either of them.
fn api_undo(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Reply {
    let popped = lock_state(state).undo.get_mut(reviewer).and_then(|stack| stack.pop());
    let Some(entry) = popped else {
        return err_reply(409, "nothing to undo");
    };
    let id = entry.seg_id.clone();
    // STALENESS + ATOMICITY FENCE. The full-row restore and learning-example retraction happen in ONE
    // database transaction whose UPDATE includes both reviewer and monotonic revision predicates.
    // There is no check-then-write window, and a failed restore cannot leave a half-undone row.
    let Some(expected_revision) = entry.decided_revision else {
        lock_state(state).undo.entry(reviewer.to_string()).or_default().push(entry);
        return err_reply(409, "the prior decision did not finish cleanly — retry the decision before undoing it");
    };
    let restored_revision =
        match db.undo_phone_human_decision(&entry.prev, reviewer, expected_revision, &entry.operation_id) {
            Ok(Some(revision)) => revision,
            Ok(None) => {
                let owner = db.get_segment_by_id(&id).ok().flatten().and_then(|row| row.reviewed_by);
                lock_state(state).undo.entry(reviewer.to_string()).or_default().push(entry);
                return match owner {
                    Some(other) if other != reviewer => {
                        err_reply(409, &format!("{other} has reviewed this clip since — undo would erase their work"))
                    }
                    None => {
                        err_reply(409, "this clip has been changed at the desktop since — undo would erase that change")
                    }
                    _ => err_reply(409, "this clip has been changed since the decision — undo would erase that change"),
                };
            }
            Err(e) => {
                lock_state(state).undo.entry(reviewer.to_string()).or_default().push(entry);
                return err_reply(500, &e.to_string());
            }
        };
    // The clip is pending again and this reviewer is about to see it — hold it for them so a second
    // reviewer's queue cannot grab the clip out from under the person mid-correction.
    //
    // But only if it is actually free. This used to `insert` unconditionally, unlike api_renew and
    // api_decision's collision guard, which both refuse when `holder() != reviewer` (external review
    // 2026-08-06 #3). The theft is reachable from the write-two failure state that
    // `an_interrupted_decision_is_finished_on_replay_and_never_recorded_twice` already pins: Sara's
    // submit 500s leaving the row unverified with reviewed_by='Sara' and an undo entry; her lease
    // lapses; Hemn's queue legitimately leases the clip and he starts typing; Sara taps undo. The old
    // line handed her Hemn's lease, and his save was then refused 409 — "another reviewer is working
    // on this clip" — over a conflict the undo itself had just manufactured.
    //
    // `holder` is the right predicate and already expires stale entries, so this grants exactly when
    // the lease is absent, expired, or already hers, and never takes one that is someone else's.
    {
        let mut guard = lock_state(state);
        let now = Instant::now();
        let current = guard.holder(&id, now).map(str::to_string);
        match current {
            Some(other) if other != reviewer => {
                // The undo itself stands — it is her own past decision being reversed. Only the lease
                // claim is refused, so if she does try to save she gets an HONEST 409 against a lease
                // Hemn really holds, rather than him getting a fabricated one.
                tracing::info!(
                    "Couch Review: undo by {reviewer} left {id} leased to {other} — not stealing an active lease"
                );
            }
            _ => {
                guard.leases.insert(id.clone(), (reviewer.to_string(), now));
            }
        }
    }
    // The restore rewrote the row, so the page's served rowVersion is stale by construction. Hand
    // back the fresh fingerprint so the immediate re-decision (the whole point of undo) is not
    // refused by the serve/decide fence it would otherwise trip.
    let row_version = restored_revision.to_string();
    json_reply_with_accounting(200, serde_json::json!({ "id": id, "rowVersion": row_version }), db, reviewer)
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
        // A REAL file on disk, one per clip. `pending_segment_ids` refuses to serve a clip whose audio
        // has gone missing (2026-08-15), so the old shared "/audio/a.wav" placeholder would drop every
        // fixture out of the queue and leave the lease/batch tests below asserting against an empty
        // list — passing while proving nothing. One process-wide temp dir, cleaned up on exit.
        static AUDIO: std::sync::LazyLock<tempfile::TempDir> =
            std::sync::LazyLock::new(|| tempfile::tempdir().expect("fixture audio dir"));
        let path = AUDIO.path().join(format!("{id}.wav"));
        std::fs::write(&path, b"RIFF").expect("fixture audio file");
        SpeechSegment {
            id: id.into(),
            audio_path: path.to_string_lossy().into_owned(),
            raw_transcript: raw.into(),
            duration_ms: 1500,
            ..SpeechSegment::default()
        }
    }

    /// Existing endpoint tests model the current phone, which always persists an operation UUID and
    /// echoes the queue's rowVersion. Add those protocol fields when a test is about some OTHER
    /// property; tests of either mandatory-field fence call `super::api_decision` directly. Explicit
    /// values (especially stale revisions and reused operation IDs) are preserved.
    fn api_decision(db: &Database, body: &[u8], reviewer: &str, state: &Mutex<CouchState>) -> Reply {
        let Ok(mut payload) = serde_json::from_slice::<serde_json::Value>(body) else {
            return super::api_decision(db, body, reviewer, state);
        };
        let action = payload.get("action").and_then(serde_json::Value::as_str).unwrap_or_default();
        let verdict = matches!(action, "accept" | "edit" | "bad");
        if payload.get("operationId").is_none() {
            if let Some(object) = payload.as_object_mut() {
                object.insert("operationId".to_string(), serde_json::Value::String(uuid::Uuid::new_v4().to_string()));
            }
        }
        if verdict && payload.get("rowVersion").is_none() {
            let stamp = payload
                .get("id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| db.segment_row_stamp(id).ok().flatten());
            if let (Some(stamp), Some(object)) = (stamp, payload.as_object_mut()) {
                object.insert("rowVersion".to_string(), serde_json::Value::String(stamp));
            }
        }
        if payload.get("pilotAfterReviewEventId").is_none() {
            let baseline = lock_state(state).pilot_policy.as_ref().map(|policy| policy.after_review_event_id);
            if let (Some(baseline), Some(object)) = (baseline, payload.as_object_mut()) {
                object.insert("pilotAfterReviewEventId".to_string(), serde_json::Value::Number(baseline.into()));
            }
        }
        let encoded = payload.to_string();
        super::api_decision(db, encoded.as_bytes(), reviewer, state)
    }

    #[test]
    fn the_phone_never_serves_a_spot_check_its_own_answer_key() {
        // Pins the reason review_text must NOT delegate to quality::effective_transcript. On a gold
        // answer-key clip, verdict_transcript holds the ANSWER and raw_transcript holds the
        // known-wrong draft the trap depends on. Serving the answer would auto-pass every listening
        // check while looking perfectly healthy — the QC would measure nothing and say nothing.
        let mut answer_key = seg("g1", "دەقی هەڵە");
        answer_key.is_gold = true;
        answer_key.verified = true;
        answer_key.human_decision = Some("edit".into());
        answer_key.verdict = Some("human_edit".into());
        answer_key.verdict_transcript = Some("دەقی ڕاست".into());
        assert_eq!(
            review_text(&answer_key),
            "دەقی هەڵە",
            "a spot check must be served its WRONG draft; serving the stored answer defeats the QC"
        );
    }

    #[test]
    fn review_text_prefers_a_human_annotation_over_the_raw_draft() {
        let mut annotated = seg("s2", "raw draft");
        annotated.annotated_transcript = Some("human typed".into());
        assert_eq!(review_text(&annotated), "human typed");

        // ...and a blank annotation never beats a good draft (the blank-overwrite class).
        let mut blank = seg("s3", "raw draft");
        blank.annotated_transcript = Some("   ".into());
        assert_eq!(review_text(&blank), "raw draft");
    }

    /// A Funnel URL is only worth handing to a reviewer if Tailscale is actually serving it.
    ///
    /// All three conditions matter independently: a handler on this port, at `/`, and AllowFunnel
    /// true. Drop any one and the app publishes a hostname that resolves and then refuses — which is
    /// worse than showing no Funnel row, because the reviewer cannot tell a dead link from a network
    /// problem on their end. The happy-path fixture is the REAL output of `tailscale serve status
    /// --json` on 2026-08-19, with the hostname redacted to a placeholder — the SHAPE is what the
    /// parser reads, and a public repo has no business carrying a real device name.
    #[test]
    fn a_funnel_url_is_only_offered_when_tailscale_really_serves_this_port() {
        let live = serde_json::json!({
            "TCP": { "443": { "HTTPS": true } },
            "Web": { "this-pc.example-tailnet.ts.net:443": {
                "Handlers": { "/": { "Proxy": "https+insecure://127.0.0.1:8737" } } } },
            "AllowFunnel": { "this-pc.example-tailnet.ts.net:443": true }
        });
        assert_eq!(
            super::funnel_host_from_status(&live, 8737).as_deref(),
            Some("this-pc.example-tailnet.ts.net"),
            "the live config serves 8737 with Funnel on — the reviewer should get this link"
        );

        // Funnel configured but NOT public: reachable only inside the tailnet, which is what
        // tailscale_url already covers. Offering it as the any-network link would be a lie.
        let mut private = live.clone();
        private["AllowFunnel"]["this-pc.example-tailnet.ts.net:443"] = serde_json::json!(false);
        assert_eq!(super::funnel_host_from_status(&private, 8737), None);

        // Serving a DIFFERENT port — someone else's service, not Couch Review.
        assert_eq!(super::funnel_host_from_status(&live, 9999), None);

        // A Funnel with no root handler cannot serve the review page.
        let mut subpath = live.clone();
        subpath["Web"]["this-pc.example-tailnet.ts.net:443"]["Handlers"] =
            serde_json::json!({ "/other": { "Proxy": "https+insecure://127.0.0.1:8737" } });
        assert_eq!(super::funnel_host_from_status(&subpath, 8737), None);

        // Nothing served at all.
        assert_eq!(super::funnel_host_from_status(&serde_json::json!({}), 8737), None);
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

    #[cfg(windows)]
    #[test]
    fn tls_identity_is_persistent_and_the_private_key_is_dpapi_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let first = load_or_create_tls_identity(Some(tmp.path())).expect("generate TLS identity");
        let second = load_or_create_tls_identity(Some(tmp.path())).expect("reload TLS identity");
        assert_eq!(first.fingerprint, second.fingerprint, "phones must see a stable certificate across restarts");
        let stored = std::fs::read_to_string(tmp.path().join(TLS_IDENTITY_FILE)).unwrap();
        assert!(!stored.contains("PRIVATE KEY"), "the TLS private key must never be stored as plaintext PEM");
        let parsed: SavedTlsIdentity = serde_json::from_str(&stored).unwrap();
        assert!(crate::dpapi::is_protected(&parsed.protected_private_key_pem));
        assert_eq!(certificate_fingerprint(&parsed.certificate_pem).unwrap(), first.fingerprint);
    }

    #[test]
    fn pairing_mints_distinct_secure_bounded_sessions() {
        let state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-secret".to_string(), "Sara".to_string())]),
            ..CouchState::default()
        });
        for _ in 0..(MAX_SESSIONS_PER_REVIEWER + 3) {
            let (reply, cookie) = api_claim(br#"{"token":"pair-secret"}"#, &state);
            assert_eq!(reply.0, 200);
            let cookie = cookie.expect("claim sets cookie");
            assert!(cookie.contains("Secure") && cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
            assert!(!cookie.contains("pair-secret"), "pairing secret and data-session credential must be distinct");
        }
        let guard = lock_state(&state);
        assert_eq!(guard.reviewers.len(), MAX_SESSIONS_PER_REVIEWER);
        assert_eq!(guard.session_issued.len(), MAX_SESSIONS_PER_REVIEWER);
        assert!(
            !guard.reviewers.contains_key("pair-secret"),
            "pairing code must not authenticate data routes directly"
        );
    }

    #[test]
    fn failed_claim_persistence_returns_no_cookie_and_preserves_every_evicted_session() {
        let tmp = tempfile::tempdir().unwrap();
        let (_db, db_path) = test_db(tmp.path());
        let pairing = HashMap::from([("pair-secret".to_string(), "Sara".to_string())]);
        let base = SystemTime::now() - Duration::from_secs(600);
        let mut sessions: HashMap<String, (String, SystemTime)> = (0..MAX_SESSIONS_PER_REVIEWER)
            .map(|index| {
                (format!("existing-cookie-{index}"), ("Sara".to_string(), base + Duration::from_secs(index as u64)))
            })
            .collect();
        sessions.insert(
            "expired-cookie".to_string(),
            ("Sara".to_string(), SystemTime::now() - COUCH_SESSION_TTL - Duration::from_secs(60)),
        );
        save_session(tmp.path(), &pairing, &db_path, &HashSet::new(), &HashSet::new(), &sessions, None).unwrap();
        let state = Mutex::new(CouchState {
            reviewers: sessions.iter().map(|(token, (name, _))| (token.clone(), name.clone())).collect(),
            pairing_codes: pairing,
            session_issued: sessions.into_iter().map(|(token, (_, issued))| (token, issued)).collect(),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            fail_session_persist: true,
            ..CouchState::default()
        });
        let session_file = session_path(tmp.path());
        let before_state = snapshot_couch_state(&state);
        let before_file = std::fs::read(&session_file).unwrap();

        let (reply, cookie) = api_claim(br#"{"token":"pair-secret"}"#, &state);
        assert_eq!(reply.0, 503, "a memory-only cookie must never be reported as claimed");
        assert!(cookie.is_none(), "persistence failure must return no Set-Cookie value");
        assert!(!String::from_utf8_lossy(&reply.2).contains("pair-secret"), "the failure must not echo the link");
        assert_eq!(
            snapshot_couch_state(&state),
            before_state,
            "the expired cookie, oldest eviction candidate, and every other session map entry must be exact"
        );
        assert_eq!(std::fs::read(&session_file).unwrap(), before_file, "the last durable generation stays intact");
        let guard = lock_state(&state);
        assert!(guard.reviewers.contains_key("existing-cookie-0"), "the would-be eviction must be restored");
        assert!(guard.reviewers.contains_key("expired-cookie"), "provisional expiry pruning must be restored too");
    }

    #[test]
    fn selective_cookie_protection_failure_fails_claim_without_mutating_memory_or_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let (_db, db_path) = test_db(tmp.path());
        let pairing = HashMap::from([("pair-secret".to_string(), "Sara".to_string())]);
        let issued = SystemTime::now() - Duration::from_secs(60);
        let sessions = HashMap::from([
            ("cookie-safe".to_string(), ("Sara".to_string(), issued)),
            ("cookie-protect-fails".to_string(), ("Sara".to_string(), issued)),
        ]);
        save_session(tmp.path(), &pairing, &db_path, &HashSet::new(), &HashSet::new(), &sessions, None).unwrap();
        let state = Mutex::new(CouchState {
            reviewers: sessions.iter().map(|(token, (name, _))| (token.clone(), name.clone())).collect(),
            pairing_codes: pairing,
            session_issued: sessions.into_iter().map(|(token, (_, at))| (token, at)).collect(),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            fail_cookie_protection_for: Some("cookie-protect-fails".to_string()),
            ..CouchState::default()
        });
        let session_file = session_path(tmp.path());
        let before_state = snapshot_couch_state(&state);
        let before_file = std::fs::read(&session_file).unwrap();

        let (reply, cookie) = api_claim(br#"{"token":"pair-secret"}"#, &state);

        assert_eq!(reply.0, 503, "one unprotectable live cookie must fail the whole claimed generation");
        assert!(cookie.is_none(), "a claim that cannot persist every live cookie must not emit Set-Cookie");
        assert!(!String::from_utf8_lossy(&reply.2).contains("cookie-protect-fails"));
        assert_eq!(snapshot_couch_state(&state), before_state, "the proposed cookie and all prior sessions roll back");
        assert_eq!(std::fs::read(&session_file).unwrap(), before_file, "the last complete durable generation remains");
    }

    #[test]
    fn frequent_page_loads_preserve_one_checkpoint_until_it_is_durably_renewed() {
        let tmp = tempfile::tempdir().unwrap();
        let (_db, db_path) = test_db(tmp.path());
        let pairing = HashMap::from([("pair-secret".to_string(), "Sara".to_string())]);
        let checkpoint = UNIX_EPOCH
            + Duration::from_secs(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs().saturating_sub(60));
        let sessions = HashMap::from([("cookie-live".to_string(), ("Sara".to_string(), checkpoint))]);
        save_session(tmp.path(), &pairing, &db_path, &HashSet::new(), &HashSet::new(), &sessions, None).unwrap();
        let state = Mutex::new(CouchState {
            reviewers: HashMap::from([("cookie-live".to_string(), "Sara".to_string())]),
            pairing_codes: pairing,
            session_issued: HashMap::from([("cookie-live".to_string(), checkpoint)]),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            ..CouchState::default()
        });
        let session_file = session_path(tmp.path());
        let original_file = std::fs::read(&session_file).unwrap();

        for elapsed in
            [Duration::from_secs(5 * 60), Duration::from_secs(10 * 60), RENEWAL_PERSIST_AFTER - Duration::from_secs(1)]
        {
            assert_eq!(
                renew_cookie_checkpoint("cookie-live", &state, checkpoint + elapsed).unwrap(),
                CookieRenewal::Current
            );
            assert_eq!(lock_state(&state).session_issued["cookie-live"], checkpoint);
            assert_eq!(std::fs::read(&session_file).unwrap(), original_file, "sub-threshold loads must not rewrite");
            let probe = api_claim_probe(br#"{"token":"pair-secret"}"#, &state);
            let payload: serde_json::Value = serde_json::from_slice(&probe.2).unwrap();
            assert_eq!(payload["durableStateMatches"], true, "a harmless page load must not make health red");
        }

        let renewal = checkpoint + RENEWAL_PERSIST_AFTER;
        lock_state(&state).fail_session_persist = true;
        let before_failed_renewal = snapshot_couch_state(&state);
        assert!(
            renew_cookie_checkpoint("cookie-live", &state, renewal).is_err(),
            "a due checkpoint that cannot reach disk fails closed"
        );
        assert_eq!(snapshot_couch_state(&state), before_failed_renewal);
        assert_eq!(std::fs::read(&session_file).unwrap(), original_file);
        lock_state(&state).fail_session_persist = false;
        assert_eq!(
            renew_cookie_checkpoint("cookie-live", &state, renewal).unwrap(),
            CookieRenewal::Renewed,
            "frequent loads cannot postpone the durable checkpoint forever"
        );
        assert_eq!(lock_state(&state).session_issued["cookie-live"], renewal);
        assert_ne!(std::fs::read(&session_file).unwrap(), original_file, "the due checkpoint is persisted");
        let bound_db = lock_state(&state).session_store.as_ref().unwrap().1.clone();
        let remembered = load_session(tmp.path(), &bound_db).unwrap();
        assert_eq!(remembered.sessions["cookie-live"].1, renewal);
        let probe = api_claim_probe(br#"{"token":"pair-secret"}"#, &state);
        let payload: serde_json::Value = serde_json::from_slice(&probe.2).unwrap();
        assert_eq!(payload["durableStateMatches"], true, "memory and disk advance together");
    }

    #[test]
    fn claim_probe_helper_is_exact_read_only_and_detects_cookie_amnesia() {
        let tmp = tempfile::tempdir().unwrap();
        let (_db, db_path) = test_db(tmp.path());
        let pairing = HashMap::from([
            ("pair-hawzhin".to_string(), "Hawzhin".to_string()),
            ("pair-pavel".to_string(), "Pavel".to_string()),
        ]);
        let policy = probe_test_policy();
        let now_seconds = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let issued = UNIX_EPOCH + Duration::from_secs(now_seconds.saturating_sub(60));
        let expired = SystemTime::now() - COUCH_SESSION_TTL - Duration::from_secs(60);
        let mut sessions: HashMap<String, (String, SystemTime)> = (0..MAX_SESSIONS_PER_REVIEWER)
            .map(|index| (format!("cookie-{index}"), ("Hawzhin".to_string(), issued)))
            .collect();
        sessions.insert("expired-cookie".to_string(), ("Hawzhin".to_string(), expired));
        let spot_checks = HashSet::from([
            ("hidden-h".to_string(), "Hawzhin".to_string()),
            ("hidden-p".to_string(), "Pavel".to_string()),
        ]);
        let pilot_spot_checks = spot_checks.clone();
        save_session(tmp.path(), &pairing, &db_path, &spot_checks, &pilot_spot_checks, &sessions, Some(&policy))
            .unwrap();

        let state = Mutex::new(CouchState {
            undo: HashMap::from([(
                "Hawzhin".to_string(),
                vec![UndoEntry {
                    operation_id: "undo-operation".to_string(),
                    seg_id: "undo-segment".to_string(),
                    prev: seg("undo-segment", "undo text"),
                    decided_revision: Some(17),
                }],
            )]),
            in_flight_operations: HashSet::from(["in-flight-operation".to_string()]),
            leases: HashMap::from([("leased-segment".to_string(), ("Pavel".to_string(), Instant::now()))]),
            reviewers: sessions.iter().map(|(token, (reviewer, _))| (token.clone(), reviewer.clone())).collect(),
            pairing_codes: pairing,
            session_issued: sessions.into_iter().map(|(token, (_, at))| (token, at)).collect(),
            spot_checks,
            pilot_spot_checks,
            session_store: Some((tmp.path().to_path_buf(), db_path.clone())),
            skipped: HashMap::from([("Pavel".to_string(), HashSet::from(["skipped-segment".to_string()]))]),
            pilot_policy: Some(policy.clone()),
            ..CouchState::default()
        });
        let session_file = session_path(tmp.path());
        let before_state = snapshot_couch_state(&state);
        let before_file = std::fs::read(&session_file).unwrap();
        let expected_db_binding = sha256_lower_hex(db_path.as_bytes());

        for _ in 0..9 {
            let reply = api_claim_probe(br#"{"token":"pair-hawzhin"}"#, &state);
            assert_eq!(reply.0, 200);
            assert!(reply.3.is_empty());
            let body_text = String::from_utf8(reply.2.clone()).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&reply.2).unwrap();
            let fields: HashSet<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(
                fields,
                HashSet::from(["reviewer", "pilotPolicy", "dbBindingSha256", "durableStateMatches"]),
                "the probe response must expose exactly the release contract"
            );
            assert_eq!(body["reviewer"], "Hawzhin");
            assert_eq!(body["pilotPolicy"], serde_json::to_value(&policy).unwrap());
            assert_eq!(body["dbBindingSha256"], expected_db_binding);
            assert_eq!(body["durableStateMatches"], true);
            assert!(!body_text.contains("pair-hawzhin"), "the credential must never be echoed");
            assert!(!body_text.contains(&db_path), "the private database path must never leave the server");
        }

        let denied = claim_probe_denied();
        let oversized = vec![b'x'; MAX_CLAIM_PROBE_BODY_BYTES + 1];
        for body in [
            b"{".as_slice(),
            br#"{}"#,
            br#"{"token":42}"#,
            br#"{"token":"pair-hawzhin","extra":true}"#,
            br#"{"token":"pair-hawzhin","token":"pair-pavel"}"#,
            br#"{"token":"unknown"}"#,
            br#"{"token":"cookie-0"}"#,
            oversized.as_slice(),
        ] {
            assert_eq!(api_claim_probe(body, &state), denied, "every invalid credential shape is generic");
        }
        assert_eq!(snapshot_couch_state(&state), before_state, "probing must not change any in-memory field");
        assert_eq!(std::fs::read(&session_file).unwrap(), before_file, "probing must not rewrite the session file");

        // The ninth live cookie was never persisted. The link itself still authenticates, but a
        // restart would forget that cookie, so the durability bit must turn red without repairing or
        // evicting anything as a side effect of observation.
        {
            let mut guard = lock_state(&state);
            guard.reviewers.insert("cookie-live-only".to_string(), "Hawzhin".to_string());
            guard.session_issued.insert("cookie-live-only".to_string(), issued);
        }
        let before_mismatch = snapshot_couch_state(&state);
        let reply = api_claim_probe(br#"{"token":"pair-hawzhin"}"#, &state);
        assert_eq!(reply.0, 200);
        let mismatch: serde_json::Value = serde_json::from_slice(&reply.2).unwrap();
        assert_eq!(mismatch["durableStateMatches"], false, "live-only cookie state must never certify");
        assert_eq!(snapshot_couch_state(&state), before_mismatch);
        assert_eq!(std::fs::read(&session_file).unwrap(), before_file);
    }

    #[test]
    fn claim_probe_http_route_precedes_cookie_pruning_and_never_sets_a_cookie() {
        let tmp = tempfile::tempdir().unwrap();
        let (_db, db_path) = test_db(tmp.path());
        let policy = probe_test_policy();
        let pairing = HashMap::from([("pair-hawzhin".to_string(), "Hawzhin".to_string())]);
        let issued = UNIX_EPOCH
            + Duration::from_secs(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs().saturating_sub(60));
        let expired = SystemTime::now() - COUCH_SESSION_TTL - Duration::from_secs(60);
        let sessions = HashMap::from([
            ("cookie-live".to_string(), ("Hawzhin".to_string(), issued)),
            ("cookie-expired".to_string(), ("Hawzhin".to_string(), expired)),
        ]);
        save_session(tmp.path(), &pairing, &db_path, &HashSet::new(), &HashSet::new(), &sessions, Some(&policy))
            .unwrap();
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: sessions.iter().map(|(token, (reviewer, _))| (token.clone(), reviewer.clone())).collect(),
            pairing_codes: pairing,
            session_issued: sessions.into_iter().map(|(token, (_, at))| (token, at)).collect(),
            session_store: Some((tmp.path().to_path_buf(), db_path.clone())),
            pilot_policy: Some(policy),
            ..CouchState::default()
        }));
        let before_state = snapshot_couch_state(&state);
        let session_file = session_path(tmp.path());
        let before_file = std::fs::read(&session_file).unwrap();

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path, state.clone(), shutdown.clone()).unwrap();
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
        let base = format!("http://127.0.0.1:{port}");
        let status = |result: Result<ureq::Response, ureq::Error>| -> u16 {
            match result {
                Ok(response) => response.status(),
                Err(ureq::Error::Status(code, _)) => code,
                Err(other) => panic!("probe transport failed: {other}"),
            }
        };
        let stale_cookie = cookie_for("cookie-expired");

        let live_cookie = cookie_for("cookie-live");
        let page = agent
            .get(&format!("{base}/"))
            .set("Cookie", &live_cookie)
            .call()
            .expect("an active reviewer reloads the page");
        assert!(page.header("Set-Cookie").is_some(), "the browser cookie still receives its sliding Max-Age");
        let after_page = agent
            .post(&format!("{base}/api/claim/probe"))
            .send_json(ureq::json!({"token": "pair-hawzhin"}))
            .expect("the read-only probe follows a real page GET");
        assert!(after_page.header("Set-Cookie").is_none());
        let after_page: serde_json::Value = serde_json::from_reader(after_page.into_reader()).unwrap();
        assert_eq!(
            after_page["durableStateMatches"], true,
            "a sub-threshold GET must not create a false-red memory/disk timestamp mismatch"
        );

        assert_eq!(
            status(agent.get(&format!("{base}/api/claim/probe")).set("Cookie", &stale_cookie).call()),
            404,
            "GET must not be a probe"
        );
        assert_eq!(
            status(
                agent
                    .post(&format!("{base}/api/claim/probe?mode=claim"))
                    .set("Cookie", &stale_cookie)
                    .send_json(ureq::json!({"token": "pair-hawzhin"}))
            ),
            404,
            "a query-bearing lookalike must not be a probe"
        );
        assert_eq!(
            status(agent.post(&format!("{base}/api/claim/probe")).set("Cookie", &stale_cookie).send_bytes(b"{")),
            401
        );
        assert_eq!(
            status(
                agent
                    .post(&format!("{base}/api/claim/probe"))
                    .set("Cookie", &stale_cookie)
                    .send_json(ureq::json!({"token": "unknown"}))
            ),
            401
        );
        assert_eq!(
            status(
                agent
                    .post(&format!("{base}/api/claim/probe"))
                    .set("Cookie", &stale_cookie)
                    .send_json(ureq::json!({"token": "cookie-live"}))
            ),
            401,
            "a valid cookie token is not a durable pairing credential"
        );
        assert_eq!(
            status(agent.post(&format!("{base}/api/claim/probe")).set("Cookie", &stale_cookie).send_bytes(&vec![
                    b'x';
                    MAX_CLAIM_PROBE_BODY_BYTES
                        + 1
                ])),
            401,
            "the probe-specific body bound is enforced before auth"
        );

        for _ in 0..9 {
            let response = agent
                .post(&format!("{base}/api/claim/probe"))
                .set("Cookie", &stale_cookie)
                .send_json(ureq::json!({"token": "pair-hawzhin"}))
                .expect("valid durable pairing token probes");
            assert_eq!(response.status(), 200);
            assert!(response.header("Set-Cookie").is_none(), "a probe must never mint or renew a cookie");
            let body: serde_json::Value = serde_json::from_reader(response.into_reader()).unwrap();
            assert_eq!(body["reviewer"], "Hawzhin");
            assert_eq!(body["durableStateMatches"], true);
            assert_eq!(body.as_object().unwrap().len(), 4);
        }

        assert_eq!(
            snapshot_couch_state(&state),
            before_state,
            "GET, invalid, repeated, and stale-cookie probes must leave every CouchState field byte-for-byte equivalent"
        );
        assert_eq!(std::fs::read(&session_file).unwrap(), before_file, "the encrypted session bytes must be identical");

        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock();
        let _ = join.join();
    }

    // REMOVED with token_from_url itself: the query-string credential path. What it pinned - that an
    // empty `t=` falls through to the cookie while a non-empty wrong one is honoured as a failing
    // claim - describes a mechanism the server no longer has. The cookie is now the only credential,
    // which makes both of those questions unaskable. `a_valid_token_in_the_query_string_is_worthless`
    // replaces it and asserts the property that actually matters now.

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

    /// Whole-state fingerprint for read-only endpoint tests. Every mutable CouchState field is
    /// represented, including the state that is intentionally not durable (leases, undo, skips, and
    /// in-flight operations). Keep this list beside CouchState's field list when that state grows.
    type UndoTestSnapshot = (String, String, String, Option<i64>);

    #[derive(Debug, PartialEq)]
    struct CouchStateTestSnapshot {
        undo: HashMap<String, Vec<UndoTestSnapshot>>,
        in_flight_operations: HashSet<String>,
        leases: HashMap<String, (String, Instant)>,
        served_work: HashSet<(String, String)>,
        reviewers: HashMap<String, String>,
        pairing_codes: HashMap<String, String>,
        session_issued: HashMap<String, SystemTime>,
        spot_checks: HashSet<(String, String)>,
        pilot_spot_checks: HashSet<(String, String)>,
        session_store: Option<(PathBuf, String)>,
        fail_session_persist: bool,
        fail_cookie_protection_for: Option<String>,
        skipped: HashMap<String, HashSet<String>>,
        pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
    }

    fn snapshot_couch_state(state: &Mutex<CouchState>) -> CouchStateTestSnapshot {
        let state = lock_state(state);
        CouchStateTestSnapshot {
            undo: state
                .undo
                .iter()
                .map(|(reviewer, entries)| {
                    (
                        reviewer.clone(),
                        entries
                            .iter()
                            .map(|entry| {
                                (
                                    entry.operation_id.clone(),
                                    entry.seg_id.clone(),
                                    serde_json::to_string(&entry.prev).expect("serialize undo snapshot"),
                                    entry.decided_revision,
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
            in_flight_operations: state.in_flight_operations.clone(),
            leases: state.leases.clone(),
            served_work: state.served_work.clone(),
            reviewers: state.reviewers.clone(),
            pairing_codes: state.pairing_codes.clone(),
            session_issued: state.session_issued.clone(),
            spot_checks: state.spot_checks.clone(),
            pilot_spot_checks: state.pilot_spot_checks.clone(),
            session_store: state.session_store.clone(),
            fail_session_persist: state.fail_session_persist,
            fail_cookie_protection_for: state.fail_cookie_protection_for.clone(),
            skipped: state.skipped.clone(),
            pilot_policy: state.pilot_policy.clone(),
        }
    }

    fn probe_test_policy() -> crate::review_pilot::ReviewPilotPolicy {
        crate::review_pilot::ReviewPilotPolicy {
            schema_version: crate::review_pilot::REVIEW_PILOT_SCHEMA_VERSION,
            after_review_event_id: 863,
            max_total_corpus_actions: crate::review_pilot::REVIEW_PILOT_TOTAL_CORPUS_ACTIONS,
            reviewers: vec![
                crate::review_pilot::ReviewPilotReviewer {
                    name: "Hawzhin".to_string(),
                    max_corpus_actions: crate::review_pilot::REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER,
                },
                crate::review_pilot::ReviewPilotReviewer {
                    name: "Pavel".to_string(),
                    max_corpus_actions: crate::review_pilot::REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER,
                },
            ],
        }
    }

    /// Present-but-broken policy files fail CLOSED (owner instruction 2026-08-20). Before this, a
    /// corrupted `voice_focus.json` or `reviewer_dialects.json` meant "no restriction" — the queue
    /// silently widened to everything the file was written to keep reviewers off, and nobody could
    /// tell until the paid hours were already spent. Now it is a 503: the page already treats >=500
    /// as retryable, the owner fixes the file, and the next fetch works. A MISSING file must still
    /// mean "no restriction", exactly as before either file existed.
    #[test]
    fn a_broken_policy_file_stops_the_queue_instead_of_widening_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("q1", "دەقی کار")).unwrap();
        let state = Mutex::new(CouchState {
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            ..CouchState::default()
        });

        // No policy files on disk: unrestricted, the clip is served.
        assert!(queue_ids(&db, "Sara", &state).contains(&"q1".to_string()), "missing files must not restrict");

        // A corrupted focus serves NOTHING — never the whole library.
        std::fs::write(tmp.path().join("voice_focus.json"), "{ not json").unwrap();
        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 503, "broken focus must 503, got {code}: {}", String::from_utf8_lossy(&body));
        std::fs::remove_file(tmp.path().join("voice_focus.json")).unwrap();

        // Same for a roster whose entry is a typo'd restriction.
        std::fs::write(tmp.path().join("reviewer_dialects.json"), r#"{ "Sara": "hawleri" }"#).unwrap();
        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 503, "broken roster must 503, got {code}: {}", String::from_utf8_lossy(&body));

        // And fixing the file is all it takes — the very next fetch serves work again.
        std::fs::write(
            tmp.path().join("reviewer_dialects.json"),
            r#"{ "_comment": "ok", "Sara": ["hawleri", "sorani"] }"#,
        )
        .unwrap();
        let (code, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 200, "a repaired policy file must serve on the next fetch, no restart");
    }

    #[test]
    fn every_verdict_requires_row_version_while_skip_remains_exempt() {
        for action in ["accept", "edit", "bad"] {
            let tmp = tempfile::tempdir().unwrap();
            let (db, _) = test_db(tmp.path());
            db.insert_segment(&seg("revision_required", "دەقی خاو")).unwrap();
            let state = state();
            let body = serde_json::json!({
                "id": "revision_required",
                "action": action,
                "text": "دەقی خاو",
                "heardMs": 600_000,
            });

            let (code, _, message, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 400, "{action} without rowVersion must be refused");
            assert!(String::from_utf8_lossy(&message).contains("rowVersion"));
            let row = db.get_segment_by_id("revision_required").unwrap().unwrap();
            assert!(!row.verified && row.human_decision.is_none(), "a protocol refusal must not decide the row");
            let receipts: i64 =
                db.connection().query_row("SELECT COUNT(*) FROM playback_receipts", [], |r| r.get(0)).unwrap();
            let events: i64 =
                db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |r| r.get(0)).unwrap();
            assert_eq!((receipts, events), (0, 0), "the fence must run before every verdict-side write");
            assert!(lock_state(&state).leases.is_empty());
            assert!(lock_state(&state).undo.values().all(Vec::is_empty));
        }

        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("skip_without_revision", "دەقی خاو")).unwrap();
        let state = state();
        let skip = serde_json::json!({
            "operationId": uuid::Uuid::new_v4().to_string(),
            "id": "skip_without_revision",
            "action": "skip"
        });
        assert_eq!(
            super::api_decision(&db, skip.to_string().as_bytes(), "Sara", &state).0,
            200,
            "skip writes no verdict and remains the only rowVersion exemption"
        );
        let row = db.get_segment_by_id("skip_without_revision").unwrap().unwrap();
        assert!(!row.verified && row.human_decision.is_none());
    }

    #[test]
    fn a_spot_check_still_requires_row_version_before_it_can_be_scored() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        gold_seg(&db, "spot_revision", "دەقی هەڵە", "دەقی ڕاست");
        let state = Mutex::new(CouchState {
            spot_checks: HashSet::from([("spot_revision".to_string(), "Sara".to_string())]),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            ..CouchState::default()
        });
        let body = serde_json::json!({
            "id": "spot_revision",
            "action": "edit",
            "text": "دەقی ڕاست",
            "heardMs": 600_000,
        });
        let (code, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 400, "a hidden check is still a verdict and must carry the served revision");
        assert!(db.spot_check_report().unwrap().is_empty(), "the missing-field refusal must not score the reviewer");
    }

    #[test]
    fn a_decision_reloads_dialect_and_focus_policy_before_any_write() {
        // A known, UNLEASED Hawleri id cannot be posted directly by a Sorani-only reviewer.
        {
            let tmp = tempfile::tempdir().unwrap();
            let (db, db_path) = test_db(tmp.path());
            db.insert_segment(&seg("KBHP_direct", "دەقی خاو")).unwrap();
            std::fs::write(tmp.path().join("reviewer_dialects.json"), r#"{"Sara":["sorani"]}"#).unwrap();
            let state = Mutex::new(CouchState {
                session_store: Some((tmp.path().to_path_buf(), db_path)),
                ..CouchState::default()
            });
            assert!(lock_state(&state).leases.is_empty(), "the client never fetched this id");
            let body = serde_json::json!({
                "id": "KBHP_direct",
                "action": "accept",
                "text": "دەقی خاو",
                "heardMs": 600_000,
                "rowVersion": db.segment_row_stamp("KBHP_direct").unwrap().unwrap(),
            });
            let (code, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 403, "queue filtering must not be the only dialect authorization check");
            let row = db.get_segment_by_id("KBHP_direct").unwrap().unwrap();
            assert!(!row.verified && row.human_decision.is_none());
            let writes: i64 = db
                .connection()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM playback_receipts) + (SELECT COUNT(*) FROM review_events)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(writes, 0, "denial must precede receipts and audit/pay events");
            assert!(lock_state(&state).leases.is_empty() && lock_state(&state).undo.values().all(Vec::is_empty));
        }

        // A clip that WAS validly served is refused if the owner removes it from focus before submit.
        {
            let tmp = tempfile::tempdir().unwrap();
            let (db, db_path) = test_db(tmp.path());
            db.insert_segment(&seg("focus_old", "دەقی یەک")).unwrap();
            db.insert_segment(&seg("focus_new", "دەقی دوو")).unwrap();
            std::fs::write(tmp.path().join("voice_focus.json"), r#"{"name":"V","segment_ids":["focus_old"]}"#).unwrap();
            let state = Mutex::new(CouchState {
                session_store: Some((tmp.path().to_path_buf(), db_path)),
                ..CouchState::default()
            });
            assert_eq!(queue_ids(&db, "Sara", &state), vec!["focus_old".to_string()]);
            let served = db.segment_row_stamp("focus_old").unwrap().unwrap();
            std::fs::write(tmp.path().join("voice_focus.json"), r#"{"name":"V","segment_ids":["focus_new"]}"#).unwrap();
            let body = serde_json::json!({
                "id": "focus_old",
                "action": "accept",
                "text": "دەقی یەک",
                "heardMs": 600_000,
                "rowVersion": served,
            });
            let (code, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 403, "the decision must obey the focus that is current at submit time");
            let row = db.get_segment_by_id("focus_old").unwrap().unwrap();
            assert!(!row.verified && row.human_decision.is_none());
            assert!(
                lock_state(&state).holder("focus_old", Instant::now()).is_none(),
                "policy denial releases the stale owner's lease for an eligible reviewer"
            );
            let writes: i64 = db
                .connection()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM playback_receipts) + (SELECT COUNT(*) FROM review_events)",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(writes, 0, "focus denial must precede every database side effect");
        }
    }

    /// A drained VOICE FOCUS is a restriction, not completion (2026-08-20 hunt): with the queue
    /// narrowed to one speaker and that set done, the page said "🎉 all clips reviewed" to a
    /// reviewer while thousands of pending clips sat outside the focus.
    #[test]
    fn a_drained_focus_reports_restriction_not_completion() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("out1", "دەرەوەی فۆکەس")).unwrap(); // pending, but OUTSIDE the focus
        let state = Mutex::new(CouchState {
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            ..CouchState::default()
        });
        std::fs::write(
            tmp.path().join("voice_focus.json"),
            r#"{ "name": "V", "segment_ids": ["not-a-pending-clip"] }"#,
        )
        .unwrap();
        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["items"].as_array().map(Vec::len), Some(0), "the focus is drained");
        assert_eq!(
            payload["noWorkInYourDialect"], true,
            "an empty FOCUSED queue must read as 'nothing assigned to you', never 'all reviewed'"
        );
    }

    /// The ids `reviewer` is handed by a queue request (and, as a side effect, leases).
    fn queue_ids(db: &Database, reviewer: &str, state: &Mutex<CouchState>) -> Vec<String> {
        let (code, _, body, ..) = api_queue(db, reviewer, state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["reviewer"], reviewer, "the queue names the reviewer it was served to");
        payload["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_string()).collect()
    }

    /// A SKIP is exempt from the version fence (2026-08-20 hunt): it writes no verdict, so there is
    /// nothing for a revision bump to conflict with — and fencing it boomeranged the clip back to
    /// the same reviewer forever while the offline replay of the skip was reported as lost work.
    /// The receipt is simply declined when the row moved: the skip lands, the evidence does not.
    #[test]
    fn a_skip_on_a_stale_serve_succeeds_without_minting_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("sk1", "دەقی کۆن")).unwrap();
        let state = state();
        let served = db.segment_review_revision("sk1").unwrap().unwrap_or(0);
        db.connection()
            .execute("UPDATE speech_segments SET review_revision = review_revision + 1 WHERE id = 'sk1'", [])
            .unwrap();
        let body = serde_json::json!({
            "id": "sk1", "action": "skip", "heardMs": 9_000, "rowVersion": served.to_string(),
        });
        let (code, _, msg, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a skip cannot be stale — it writes nothing: {}", String::from_utf8_lossy(&msg));
        let receipts: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'sk1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(receipts, 0, "but its evidence is declined — nobody can bind it to the served audio");
    }

    /// A SPOT CHECK is exempt from the version fence too: grading writes nothing to the row, and
    /// bulk metadata runs (a rights stamp) bump every GOLD row's revision at once — one such run
    /// must not 409 every check in flight and thin the honesty measurement.
    #[test]
    fn a_spot_check_is_graded_even_when_a_bulk_stamp_bumped_the_row() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        gold_seg(&db, "sc1", "دەقی هەڵە", "دەقی ڕاست");
        let data_dir = tmp.path().to_path_buf();
        let state = Mutex::new(CouchState {
            spot_checks: HashSet::from([("sc1".to_string(), "Sara".to_string())]),
            session_store: Some((data_dir, db_path)),
            ..CouchState::default()
        });
        let served = db.segment_review_revision("sc1").unwrap().unwrap_or(0);
        // The bulk stamp: touches metadata, bumps the revision, changes no audio and no answer key.
        db.connection()
            .execute("UPDATE speech_segments SET review_revision = review_revision + 1 WHERE id = 'sc1'", [])
            .unwrap();
        let body = serde_json::json!({
            "id": "sc1", "action": "edit", "text": "دەقی ڕاست",
            "heardMs": 600_000, "rowVersion": served.to_string(),
        });
        let (code, _, msg, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "the check is graded, not bounced: {}", String::from_utf8_lossy(&msg));
        let report = db.spot_check_report().unwrap();
        assert_eq!(report.len(), 1, "and SCORED");
        assert_eq!(report[0].noticed, 1, "she corrected the planted error");
    }

    /// The pay-bearing audit row commits WITH the decision, inside its transaction — a verified row
    /// whose work the pay metric cannot see is no longer a representable state (2026-08-20 hunt).
    #[test]
    fn a_phone_decision_and_its_audit_event_are_one_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("au1", "دەقی کار")).unwrap();
        let revision = db.segment_review_revision("au1").unwrap().unwrap_or(0);
        // Straight at the DB layer: no couch closure in sight, so the event can only have come from
        // the decision's own transaction.
        db.record_phone_human_decision_by_at_revision("au1", "accept", Some("دەقی کار"), "Sara", revision)
            .unwrap()
            .expect("decision lands");
        let (n, src): (i64, String) = db
            .connection()
            .query_row(
                "SELECT COUNT(*), MAX(source) FROM review_events WHERE segment_id = 'au1' AND reviewer = 'Sara'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 1, "exactly one audit row rides the decision commit");
        assert_eq!(src, "couch");
        assert!(db.reviewed_audio_ms("Sara").unwrap() > 0, "and the pay basis sees the work immediately");
    }

    /// The version fence must fire BEFORE the receipt mint (audit fix 2026-08-20). A receipt's
    /// revision, fingerprint and duration are all resolved from the CURRENT row, so minting one for
    /// a submit whose serve predates a re-chunk would bind this reviewer's real listening to a row
    /// they were never shown — evidence manufactured purely by ordering. Without the fix this test
    /// fails on the second assertion: the 409 still comes back, but the receipt is already on disk.
    #[test]
    fn a_stale_serve_is_refused_before_its_receipt_can_be_minted() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("vf1", "دەقی کۆن")).unwrap();
        let state = state();
        let served_revision = db.segment_review_revision("vf1").unwrap().unwrap_or(0);
        // The row moves on after the serve — a re-transcribe, refine loop or desktop edit.
        db.connection()
            .execute("UPDATE speech_segments SET review_revision = review_revision + 1 WHERE id = 'vf1'", [])
            .unwrap();
        let body = serde_json::json!({
            "id": "vf1", "action": "accept", "text": "دەقی کۆن",
            "heardMs": 9_000, "clipDurationMs": 9_000, "rowVersion": served_revision.to_string(),
        });
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 409, "a stale serve is refused");
        let receipts: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'vf1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(receipts, 0, "and no receipt exists for a row the reviewer was never shown");
    }

    /// The SERVER half of the phone's playback evidence: what the page reports is only how much
    /// media time it played; the revision and the audio fingerprint are resolved here.
    ///
    /// A client that could name those could mint a receipt for a clip or revision it never loaded,
    /// and the guard would then be comparing the client's claim with the client's claim.
    #[test]
    fn a_phone_decision_mints_a_receipt_with_server_resolved_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("pr1", "دەقی سەرەتایی")).unwrap();
        let state = state();

        let body = serde_json::json!({
            "id": "pr1", "action": "accept", "text": "دەقی سەرەتایی",
            "heardMs": 8_800, "clipDurationMs": 9_000,
            // Deliberately NOT sent: revision and fingerprint are not the client's to assert.
        });
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        let (segment_id, revision, fingerprint, played, coverage): (String, i64, String, i64, f64) = db
            .connection()
            .query_row(
                "SELECT segment_id, segment_revision, audio_fingerprint, played_ms, coverage_ratio                  FROM playback_receipts WHERE segment_id = 'pr1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("a phone decision must leave a listening receipt");

        assert_eq!(segment_id, "pr1");
        assert_eq!(played, 8_800, "the page's reported media time is recorded verbatim");
        assert!(coverage > 0.97, "8.8s of a 9s clip is a full listen, got {coverage}");
        assert!(!fingerprint.is_empty(), "the receipt must name the audio it was minted against");
        assert!(revision >= 0, "the revision is resolved server-side, not supplied");

        // And the evidence actually satisfies the guard for THIS clip at THIS revision.
        db.require_playback_evidence("pr1", revision, &fingerprint, Some("Sara"))
            .expect("a clip heard end to end must be decidable");
    }

    /// The coverage DENOMINATOR is the server's clip length, never the client's claim about it.
    ///
    /// Resolving the revision and the fingerprint server-side stops a client naming a clip it never
    /// loaded, but left the length it is measured against in the client's hands: a page reporting
    /// "I played 100ms of a 100ms clip" scores a perfect 1.0 and clears the bar having played a
    /// tenth of a second of a 1.5s sentence. Both halves of the ratio have to come from the server
    /// or the guard is still comparing the client's claim with the client's claim.
    #[test]
    fn a_client_cannot_shrink_the_clip_to_clear_the_bar() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("pr3", "دەقی سێ")).unwrap();
        let state = state();

        // The fixture clip is 1500ms. The page claims it is 100ms and that it played all of it.
        let body = serde_json::json!({
            "id": "pr3", "action": "accept", "text": "دەقی سێ",
            "heardMs": 100, "clipDurationMs": 100,
        });
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 428, "a tenth of a second cannot buy a verdict by relabelling the clip");

        let (total, coverage): (i64, f64) = db
            .connection()
            .query_row(
                "SELECT clip_duration_ms, coverage_ratio FROM playback_receipts WHERE segment_id = 'pr3'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("the receipt is still minted — it just tells the truth about how little was heard");

        assert_eq!(total, 1_500, "the receipt records the SERVER's clip length, not the claimed one");
        assert!(coverage < 0.10, "100ms of a 1.5s clip is not a listen, got {coverage}");

        let revision = db.segment_review_revision("pr3").unwrap().unwrap_or(0);
        let fingerprint = db.segment_audio_fingerprint("pr3").unwrap().unwrap_or_default();
        assert!(
            !db.has_sufficient_playback_evidence("pr3", revision, &fingerprint, Some("Sara")).unwrap(),
            "a tenth of a second must not satisfy the listening bar"
        );
    }

    /// ENFORCEMENT, end to end: an under-played clip is REFUSED and the row is left untouched.
    ///
    /// Every other test here proves the guard can ANSWER the question. This proves the answer is
    /// acted on — the distinction the whole observe-only period existed to close. A guard that logs
    /// its objection and writes the verdict anyway is decoration, and it looked identical to this one
    /// in every log line it produced.
    ///
    /// A REJECT is included deliberately (owner decision 2026-08-19): an accept injects text, a
    /// reject permanently removes a clip, and both are verdicts on audio nobody heard.
    #[test]
    fn an_under_played_clip_is_refused_and_writes_nothing() {
        for action in ["accept", "edit", "bad"] {
            let tmp = tempfile::tempdir().unwrap();
            let (db, _) = test_db(tmp.path());
            db.insert_segment(&seg("enf1", "دەقی ئەسڵی")).unwrap();
            let state = state();

            // The fixture clip is 1500ms; 300ms is a fifth of it.
            let body = serde_json::json!({
                "id": "enf1", "action": action, "text": "دەقی نوێ",
                "heardMs": 300, "clipDurationMs": 1_500,
            });
            let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 428, "{action} on a fifth of a clip must be refused");

            let row = db.get_segment_by_id("enf1").unwrap().unwrap();
            assert!(!row.verified, "{action} was refused, so the row must not be verified");
            assert_eq!(row.human_decision, None, "{action} was refused, so no decision may be recorded");
            assert_eq!(row.annotated_transcript, None, "{action} was refused, so no text may be written");
            assert_eq!(row.raw_transcript, "دەقی ئەسڵی", "the original draft must survive a refusal");
            assert!(
                lock_state(&state).holder("enf1", Instant::now()).is_none(),
                "a refused request must not leave a lease on a clip it never decided"
            );

            // And the SAME reviewer, having now listened, gets through.
            let body = serde_json::json!({
                "id": "enf1", "action": action, "text": "دەقی نوێ",
                "heardMs": 1_500, "clipDurationMs": 1_500,
            });
            let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 200, "{action} must land once the clip has actually been heard");
        }
    }

    /// A SKIP stays legal on a clip that would not play at all — the one honest move left.
    ///
    /// Gating it would trap a reviewer whose audio is broken: they cannot judge the clip and could no
    /// longer say so, which is precisely the guess the skip path exists to prevent.
    #[test]
    fn a_skip_is_never_refused_for_not_having_listened() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("enf2", "دەقی دوو")).unwrap();
        let state = state();

        let body = serde_json::json!({"id": "enf2", "action": "skip", "heardMs": 0, "clipDurationMs": 1_500});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a reviewer who cannot hear a clip must still be able to say so");

        let row = db.get_segment_by_id("enf2").unwrap().unwrap();
        assert!(!row.verified, "a skip still writes no verdict");
    }

    /// Undo must not orphan the reviewer's own listening: the immediate re-decision has to land.
    ///
    /// Found by the 2026-08-19 hunt: the decision bumps review_revision, the receipt stays keyed to
    /// the pre-decision revision, and undo restores the ROW but not the evidence — so the very next
    /// decision on a clip the reviewer just heard, decided and undid was refused 428 "play the whole
    /// clip first". The reviewer is punished for using Undo, seconds after listening.
    #[test]
    fn undo_then_redecide_does_not_demand_a_second_listen() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("un1", "دەقی ئەسڵی")).unwrap();
        let state = state();

        // Heard in full, decided.
        let body = serde_json::json!({
            "id": "un1", "action": "edit", "text": "ڕاستکراوە",
            "heardMs": 1_500, "clipDurationMs": 1_500,
        });
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        // Undone.
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200, "the undo itself must succeed");

        // Re-decided seconds later, WITHOUT replaying a clip that was just heard end to end.
        let body = serde_json::json!({"id": "un1", "action": "edit", "text": "ڕاستکراوەی دوو"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a re-decision after undo must not demand a second listen of the same audio");

        // But SOMEBODY ELSE still has to listen for themselves.
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        let body = serde_json::json!({"id": "un1", "action": "edit", "text": "دەقی کەسی تر"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Hemn", &state);
        assert_eq!(code, 428, "carried-forward evidence is personal; another reviewer must still listen");
    }

    /// A decision sent with no heard-time reports nothing, and must not fabricate evidence.
    #[test]
    fn a_phone_decision_without_reported_playback_mints_no_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("pr2", "دەقی دوو")).unwrap();
        let state = state();

        let body = serde_json::json!({"id": "pr2", "action": "accept", "text": "دەقی دوو"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 428, "a verdict that reports no listening at all must be refused outright");

        let receipts: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'pr2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(receipts, 0, "silence about listening must never become evidence OF listening");
    }

    #[test]
    fn decision_accept_edit_bad_and_undo_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        db.insert_segment(&seg("s2", "دەق دوو")).unwrap();
        let state = state();

        // EDIT: corrected text becomes the annotated gold; row is verified; decision recorded.
        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": "دەق یەک ڕاستکراوە"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(row.verified);
        assert_eq!(row.annotated_transcript.as_deref(), Some("دەق یەک ڕاستکراوە"));
        assert_eq!(row.reviewed_by.as_deref(), Some("Sara"), "a phone decision is attributed to its reviewer");

        // BAD: reject decision, still marked reviewed.
        let body = serde_json::json!({"heardMs": 600_000, "id": "s2", "action": "bad"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);
        assert!(db.get_segment_by_id("s2").unwrap().unwrap().verified);

        // Queue now empty (both reviewed).
        assert!(queue_ids(&db, "Sara", &state).is_empty(), "both clips reviewed -> empty queue");

        // UNDO restores the LAST decision (s2): unverified again, decision cleared.
        let (code, _, body, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        let undone: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(undone["id"], "s2");
        let row = db.get_segment_by_id("s2").unwrap().unwrap();
        assert!(!row.verified, "undo must reopen the clip");
        assert_eq!(row.reviewed_by, None, "undo retracts the attribution with the decision");

        // Second undo pops s1; third has nothing left.
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "empty undo stack is a 409, not a phantom undo");
    }

    #[test]
    fn a_legacy_interrupted_decision_is_finished_on_replay_and_never_recorded_twice() {
        // Releases before atomic phone finalization could commit verdict/learning effects while leaving
        // verified=0. Construct that durable legacy state directly, then prove an outbox replay repairs
        // it without repeating any learning side effect.
        let tmp = tempfile::tempdir().unwrap();
        let (db, path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        let state = state();
        let fix = "دەق یەک ڕاستکراوە";
        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": fix});

        let side = rusqlite::Connection::open(&path).unwrap();
        let pairs = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = 's1'", [], |r| r.get(0)).unwrap()
        };
        let memory_hits = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COALESCE(SUM(hit_count), 0) FROM correction_memory", [], |r| r.get(0)).unwrap()
        };

        db.record_human_decision_by("s1", "edit", Some(fix), None, Some("Sara")).unwrap();

        let half = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(!half.verified, "legacy desktop-style decision is not phone-finalized");
        assert_eq!(half.reviewed_by.as_deref(), Some("Sara"));
        let pairs_after_failure = pairs(&side);
        let hits_after_failure = memory_hits(&side);
        assert!(pairs_after_failure >= 1, "the legacy decision must have a learning pair to deduplicate");

        // The outbox replays against the current atomic endpoint.
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "the replay must succeed, not 409 or 500");

        let done = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(done.verified, "the replay FINISHES the interrupted write");
        assert_eq!(done.annotated_transcript.as_deref(), Some(fix));

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

        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": "ڕاستکراوەی سارا"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        // The owner corrects the same clip at the desktop afterwards.
        crate::jury::record_human_decision_by(&db, "s1", "edit", Some("ڕاستکراوەی خاوەن"), None, Some("Owner"))
            .unwrap();

        // Sara now taps undo. Her snapshot predates the owner's edit entirely.
        let (code, _, msg, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "undo must not silently revert someone else's later work");
        assert!(String::from_utf8_lossy(&msg).contains("Owner"), "the refusal must name who would lose work: {msg:?}");
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(row.reviewed_by.as_deref(), Some("Owner"), "the owner's edit must survive the refused undo");
        assert_eq!(row.verdict_transcript.as_deref(), Some("ڕاستکراوەی خاوەن"));

        // And the entry is KEPT, not consumed by the refusal — a refused undo must not silently
        // become "nothing to undo" on the next press.
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "still refused, and still present rather than swallowed");
    }

    #[test]
    fn undo_refuses_when_any_writer_touched_the_row_since_not_only_a_re_review() {
        // Audit #2/#3: the old fence keyed ONLY on reviewed_by, which no non-decision writer touches
        // — batch normalize, the refine loop, a desktop text edit and a champion re-draft all rewrite
        // transcript columns while leaving reviewed_by intact, so undo silently reverted every one of
        // them. The fence is now the row's updated_at fingerprint captured post-decision.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        let state = state();

        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": "ڕاستکراوەی سارا"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        // A reviewed_by-PRESERVING writer touches the row (what batch normalize does). The sentinel
        // updated_at makes the change deterministic — datetime('now') has 1 s granularity, so a real
        // writer inside the same second would be invisible to a wall-clock-based assertion.
        db.connection()
            .execute(
                "UPDATE speech_segments SET normalized_transcript='نوێ', updated_at='2030-01-01 00:00:00' WHERE id='s1'",
                [],
            )
            .unwrap();

        let (code, _, msg, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "undo must refuse: a background writer touched the row since the decision");
        assert!(
            String::from_utf8_lossy(&msg).contains("changed since the decision"),
            "the refusal names the reason: {msg:?}"
        );
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(row.normalized_transcript.as_deref(), Some("نوێ"), "the later write must survive");
        assert_eq!(row.reviewed_by.as_deref(), Some("Sara"), "the decision itself is untouched by the refusal");

        // Kept, not consumed: a refused undo must not become \"nothing to undo\".
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409);
    }

    #[test]
    fn a_decision_against_a_replaced_draft_is_refused_not_recorded() {
        // Audit #4: the queue stamps each clip with the row's updated_at (rowVersion) and the page
        // echoes it back. If a background re-draft replaced the text between serve and submit, the
        // reviewer judged text that no longer exists — recording it would misclassify accept-vs-edit
        // against the NEW row and mint a DPO pair anti-training the fresher draft.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەقی کۆن")).unwrap();
        let state = state();
        let served = db.segment_row_stamp("s1").unwrap().expect("row exists");

        // A batch re-draft lands after the clip was served.
        db.connection()
            .execute(
                "UPDATE speech_segments SET raw_transcript='دەقی نوێی چامپیۆن', updated_at='2030-01-01 00:00:00' WHERE id='s1'",
                [],
            )
            .unwrap();

        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "accept", "text": "دەقی کۆن", "rowVersion": served});
        let (code, _, msg, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 409, "a stale-version decision must be refused: {msg:?}");
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(!row.verified, "nothing may be recorded for a refused decision");
        assert_eq!(row.human_decision, None);

        // With the CURRENT version the same decision is accepted.
        let fresh = db.segment_row_stamp("s1").unwrap().unwrap();
        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "accept", "text": "دەقی نوێی چامپیۆن", "rowVersion": fresh});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);
        assert!(db.get_segment_by_id("s1").unwrap().unwrap().verified);
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
        db.record_spot_check("g1", "Sara", "edit", "دەقی ڕاست", "دەقی ڕاست").unwrap();

        // The owner then un-verifies it, so it is pending work again and no longer an answer key.
        let mut reopened = db.get_segment_by_id("g1").unwrap().unwrap();
        reopened.verified = false;
        db.insert_segment(&reopened).unwrap();

        let body = serde_json::json!({"heardMs": 600_000, "id": "g1", "action": "edit", "text": "ڕاستکراوەی سارا"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
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
    fn a_check_whose_answer_key_was_stripped_is_not_graded_against_the_empty_string() {
        // The sibling above covers UN-VERIFYING. This is the case the old `!prev.verified` guard let
        // through: the desktop "mark bad" writes human_decision='reject' / verdict='human_reject' and
        // KEEPS verified = true, so the artefact survives while the human answer key does not.
        // `human_verified_text` then returns None, the old grader did `.unwrap_or_default()`, and
        // compute_cer("", submitted) is 1.0 for any non-empty answer — a reviewer who transcribed the
        // clip CORRECTLY was recorded at a fabricated 1.00 CER and averaged into spot_check_report,
        // with an HTTP reply byte-identical to a real success.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let mut gold = seg("g1", "دەقی خاو");
        gold.verified = true;
        db.insert_segment(&gold).unwrap();
        // Through the REAL decision path, so the key lives in verdict_transcript exactly as a human
        // edit leaves it — the shape `list_spot_check_candidates` admits and the one "mark bad" strips.
        db.record_human_decision("g1", "edit", Some("دەقی ڕاست"), None).unwrap();
        let state = state();
        assert!(
            crate::quality::human_verified_text(&db.get_segment_by_id("g1").unwrap().unwrap()).is_some(),
            "precondition: this clip really is a usable answer key"
        );
        lock_state(&state).spot_checks.insert(("g1".to_string(), "Sara".to_string()));

        // Marked bad at the desktop: the answer key is gone, `verified` is not.
        db.record_human_decision("g1", "reject", None, None).unwrap();
        let after = db.get_segment_by_id("g1").unwrap().unwrap();
        assert!(after.verified, "the artefact the OLD guard tested is still true");
        assert!(
            crate::quality::human_verified_text(&after).is_none(),
            "but the answer key — the thing that made this a check — is gone"
        );

        // Hold the lease as api_queue would have when it served her the clip.
        lock_state(&state).leases.insert("g1".to_string(), ("Sara".to_string(), std::time::Instant::now()));

        let body = serde_json::json!({"heardMs": 600_000, "id": "g1", "action": "edit", "text": "ڕاستکراوەی سارا"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        // The spot-check block runs BEFORE the already-reviewed guard at the end of api_decision, so
        // the contrast is exact: the old code graded against "" and returned 200 from inside that
        // block; the fixed code records nothing and falls through to the honest 409 this clip
        // deserves — it really was decided at the desktop and cannot be reviewed again.
        assert_eq!(code, 409, "a clip decided at the desktop is refused, not silently graded");

        // NOT graded: no fabricated score may exist for this reviewer.
        let report = db.spot_check_report().unwrap();
        assert!(
            report.iter().all(|s| s.checks == 0),
            "a check with no answer key must not be scored at all: {report:?}"
        );
        assert!(
            !lock_state(&state).spot_checks.contains(&("g1".to_string(), "Sara".to_string())),
            "the stale pair must be dropped so it cannot swallow this clip again"
        );
    }

    #[test]
    fn an_undo_never_takes_a_lease_another_reviewer_is_holding() {
        // api_renew and api_decision both refuse when holder() != reviewer; api_undo used to insert
        // unconditionally, so it could hand Sara a lease Hemn legitimately held — and Hemn's save was
        // then refused 409 over a conflict the undo had just created.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق")).unwrap();
        let state = state();

        // Sara decided it, so she has something to undo.
        lock_state(&state).leases.insert("s1".to_string(), ("Sara".to_string(), Instant::now()));
        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "accept", "text": "دەق"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "precondition: Sara's decision landed");

        // Her lease lapses and Hemn's queue legitimately takes the clip.
        lock_state(&state).leases.insert("s1".to_string(), ("Hemn".to_string(), Instant::now()));

        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200, "the undo itself stands — it is her own decision being reversed");
        assert_eq!(
            lock_state(&state).holder("s1", Instant::now()),
            Some("Hemn"),
            "the lease must stay with the reviewer who legitimately holds it"
        );

        // And the consequence that actually bites: Hemn can still save his work.
        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": "ڕاستکراوەی هێمن"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Hemn", &state);
        assert_eq!(code, 200, "the legitimate holder must not be 409'd by someone else's undo");
    }

    #[test]
    fn an_undo_reclaims_a_lease_that_is_free_or_already_the_reviewers_own() {
        // The other half: refusing to steal must not stop a reviewer reclaiming a clip nobody holds,
        // which is the whole point of the line — so the queue cannot grab it back mid-correction.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق")).unwrap();
        let state = state();
        lock_state(&state).leases.insert("s1".to_string(), ("Sara".to_string(), Instant::now()));
        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "accept", "text": "دەق"});
        assert_eq!(api_decision(&db, body.to_string().as_bytes(), "Sara", &state).0, 200);

        lock_state(&state).leases.remove("s1"); // nobody holds it now
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        assert_eq!(
            lock_state(&state).holder("s1", Instant::now()),
            Some("Sara"),
            "an unheld clip must be reclaimed for the reviewer who just reopened it"
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
        let (code, ..) = api_renew(body.to_string().as_bytes(), "Sara", &state);
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
                    let _ = save_session(
                        &dir,
                        &reviewers,
                        &db_path,
                        &HashSet::new(),
                        &HashSet::new(),
                        &HashMap::new(),
                        None,
                    );
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

        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": "ڕاستکراوە"});
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
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

        let sara_work = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": "ڕاستکراوە", "reviewer": "Sara"});
        let (code, _, msg, ..) = api_decision(&db, sara_work.to_string().as_bytes(), "Hemn", &state);
        assert_eq!(code, 409, "Sara's decision must not be written under Hemn's name");
        assert!(String::from_utf8_lossy(&msg).contains("Sara"), "the refusal must name the real author: {msg:?}");
        let untouched = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(!untouched.verified, "a mis-attributed decision must write NOTHING");
        assert_eq!(untouched.reviewed_by, None);

        // The very same queued item, flushed on Sara's own link, lands normally — it was never invalid,
        // only in the wrong hands.
        let (code, ..) = api_decision(&db, sara_work.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);
        assert_eq!(db.get_segment_by_id("s1").unwrap().unwrap().reviewed_by.as_deref(), Some("Sara"));

        // A live submit carries no claim at all, and that ordinary path is unchanged.
        let live = serde_json::json!({"heardMs": 600_000, "id": "s2", "action": "bad"});
        let (code, ..) = api_decision(&db, live.to_string().as_bytes(), "Hemn", &state);
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

        let accept = |id: &str| {
            serde_json::json!({"heardMs": 600_000, "id": id, "action": "accept", "text": "هەر وایە"}).to_string()
        };
        assert_eq!(api_decision(&db, accept("a1").as_bytes(), "Sara", &state).0, 200);
        assert_eq!(api_decision(&db, accept("b1").as_bytes(), "Hemn", &state).0, 200);

        // Hemn undoes. Only b1 may move — a1 is Sara's and she has not asked for it back.
        let (code, _, body, ..) = api_undo(&db, "Hemn", &state);
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
        let (code, _, body, ..) = api_queue(&db, "Ali", &state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(payload["items"].as_array().unwrap().is_empty(), "nothing is free for a third reviewer");
        assert_eq!(payload["heldByOthers"], total, "but all {total} pending clips are reported as held");
    }

    #[test]
    fn a_bad_couch_port_override_falls_back_instead_of_taking_the_link_down() {
        // CORTEX_COUCH_PORT exists so an end-to-end harness can drive the real server without
        // fighting the owner's own Couch Review for 8737. It is therefore a knob that will sometimes
        // be set by something other than a careful operator — a stale shell, a copied launch script,
        // a typo. The safety property is that NONE of those can stop the phone link coming up: the
        // owner would see Couch Review "start" and then find no page at the URL he has bookmarked.
        assert_eq!(port_from(None), COUCH_PORT, "unset means the real, memorable port");
        assert_eq!(port_from(Some("18737")), 18737, "a valid override is honoured");
        assert_eq!(port_from(Some("  18737  ")), 18737, "surrounding whitespace is not a typo worth punishing");
        assert_eq!(port_from(Some("")), COUCH_PORT, "an empty value is not a port");
        assert_eq!(port_from(Some("0")), COUCH_PORT, "port 0 means 'any free port' — the link would be unfindable");
        assert_eq!(port_from(Some("nonsense")), COUCH_PORT, "garbage falls back rather than failing");
        assert_eq!(port_from(Some("70000")), COUCH_PORT, "out of u16 range falls back rather than wrapping");
        assert_eq!(port_from(Some("-1")), COUCH_PORT, "negative falls back rather than wrapping");
    }

    #[test]
    fn a_clip_measured_to_hold_two_speakers_says_so_before_the_reviewer_decides() {
        // 17 of the owner's 144 clips span a turn between two people, and every one of them carries a
        // single authoritative SPEAKER_xx like any other clip — chunks are cut on silence and labelled
        // afterwards. Without this the phone presents them as ordinary work and "Looks good" walks two
        // voices into a single-speaker corpus.
        //
        // Three rows, because the interesting part is the THIRD: NULL must not read as "one speaker".
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("mixed", "دەق")).unwrap();
        db.insert_segment(&seg("solo", "دەق")).unwrap();
        db.insert_segment(&seg("unmeasured", "دەق")).unwrap();
        // Real values from the probe's own calibration set: 0.4121 is a clip the owner HEARD as
        // turn-taking, 0.7530 one he heard as a single speaker.
        db.set_speaker_change_score("mixed", 0.4121).unwrap();
        db.set_speaker_change_score("solo", 0.7530).unwrap();

        let (code, _, body, ..) = api_queue(&db, "Sara", &state());
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let flag = |id: &str| {
            payload["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|it| it["id"] == id)
                .unwrap_or_else(|| panic!("{id} is missing from the queue"))["speakerChange"]
                .clone()
        };
        assert_eq!(flag("mixed"), serde_json::Value::Bool(true), "a measured speaker change must reach the phone");
        assert_eq!(flag("solo"), serde_json::Value::Bool(false), "a single-speaker clip must not be flagged");
        // The honesty case: nothing measured this clip, so the page shows nothing. It must NOT be
        // reported as a clip verified to hold one speaker.
        assert_eq!(
            flag("unmeasured"),
            serde_json::Value::Bool(false),
            "an unmeasured clip is not flagged — and the badge is the only thing this field drives, so \
             it also never claims 'one speaker'"
        );
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
        let body = serde_json::json!({"heardMs": 600_000, "id": "x1", "action": "accept", "text": "دەق"}).to_string();
        let (code, _, msg, ..) = api_decision(&db, body.as_bytes(), "Hemn", &state);
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
        let ok = serde_json::json!({"heardMs": 600_000, "id": "u1", "action": "accept", "text": "دەق"}).to_string();
        assert_eq!(api_decision(&db, ok.as_bytes(), "Sara", &state).0, 200);
        assert_eq!(
            lock_state(&state).leases.get("u1").map(|(who, _)| who.clone()),
            None,
            "a DECIDED clip releases its lease immediately — it is no longer pending work"
        );

        // (b) every rejected shape, none of which may strand a lease on u2.
        for bad in [
            serde_json::json!({"heardMs": 600_000, "id": "u2", "action": "accept", "text": "[Pending WSL 7B ASR]"}),
            serde_json::json!({"heardMs": 600_000, "id": "u2", "action": "edit", "text": ""}),
            serde_json::json!({"heardMs": 600_000, "id": "u2", "action": "explode", "text": "x"}),
        ] {
            let (code, ..) = api_decision(&db, bad.to_string().as_bytes(), "Sara", &state);
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

        let edit =
            serde_json::json!({"heardMs": 600_000, "id": "r1", "action": "edit", "text": "دەقی ڕاستکراوە"}).to_string();
        assert_eq!(api_decision(&db, edit.as_bytes(), "Sara", &state).0, 200);
        let pairs = |id: &str| -> i64 {
            db.connection()
                .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(pairs("r1"), 1, "the edit produced exactly one learning pair");

        // Rolling deployment compatibility: an outbox entry created by the immediately previous
        // page build has no rowVersion. If its response was lost but the write committed, the exact
        // stored decision is safe to ACK — it performs no mutation. A different/new verdict without
        // the stamp is still refused by the mandatory-version test above.
        let legacy_retry = serde_json::json!({
            "heardMs": 600_000,
            "id": "r1",
            "action": "edit",
            "text": "دەقی ڕاستکراوە",
        });
        let events_before: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        let (code, _, body, ..) = super::api_decision(&db, legacy_retry.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a completed identical legacy retry must drain safely after deployment");
        let reply: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply["duplicate"], true);
        assert_eq!(pairs("r1"), 1, "the legacy retry must not add a learning pair");
        let events_after: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(events_after, events_before, "an acknowledged retry must not append an audit/pay event");

        // The identical resubmit — the retry — must be answered as already-done.
        let (code, _, body, ..) = api_decision(&db, edit.as_bytes(), "Sara", &state);
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
        let redo =
            serde_json::json!({"heardMs": 600_000, "id": "r1", "action": "edit", "text": "دەقی دیسان ڕاستکراوە"})
                .to_string();
        assert_eq!(api_decision(&db, redo.as_bytes(), "Sara", &state).0, 200);
        assert_eq!(
            db.get_segment_by_id("r1").unwrap().unwrap().annotated_transcript.as_deref(),
            Some("دەقی دیسان ڕاستکراوە"),
            "changing the text is a real re-review and must be recorded"
        );

        // A REJECT retry must also be absorbed. Only the edit path was covered, so deleting the
        // ("reject", _) arm of is_repeat_of_stored_decision changed nothing any test could see —
        // and a re-sent reject would have been recorded as a second human decision.
        let rej = serde_json::json!({"heardMs": 600_000, "id": "r2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, rej.as_bytes(), "Sara", &state).0, 200);
        let before = lock_state(&state).undo.get("Sara").map(Vec::len).unwrap_or(0);
        let (code, _, body, ..) = api_decision(&db, rej.as_bytes(), "Sara", &state);
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
        let reject = serde_json::json!({"heardMs": 600_000, "id": "r2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, reject.as_bytes(), "Sara", &state).0, 200);
        let (code, _, msg, ..) = api_decision(&db, reject.as_bytes(), "Hemn", &state);
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
    fn a_durable_operation_uuid_replays_after_restart_without_duplicate_event_or_pay() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("operation-replay", "دەقی خاو")).unwrap();
        let operation_id = "11111111-1111-4111-8111-111111111111";
        let body = serde_json::json!({
            "operationId": operation_id,
            "id": "operation-replay",
            "action": "edit",
            "text": "دەقی ڕاستکراوە",
            "reviewer": "Sara",
            "rowVersion": db.segment_row_stamp("operation-replay").unwrap().unwrap(),
            "heardMs": 600_000,
            "clipDurationMs": 1_500,
        });
        let first_state = state();
        assert_eq!(super::api_decision(&db, body.to_string().as_bytes(), "Sara", &first_state).0, 200);

        let counts = || -> (i64, i64, i64) {
            db.connection()
                .query_row(
                    "SELECT
                         (SELECT COUNT(*) FROM review_events),
                         (SELECT COUNT(*) FROM review_compensation_ledger),
                         (SELECT COUNT(*) FROM agent_examples WHERE segment_id = 'operation-replay')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        };
        assert_eq!(counts(), (1, 1, 1), "one client operation has one event, pay row, and learning pair");
        let receipt = db.review_operation(operation_id).unwrap().expect("durable operation receipt");
        assert_eq!(receipt.segment_id, "operation-replay");
        assert_eq!(receipt.reviewer, "Sara");
        assert_eq!(
            receipt.operation_payload_hash,
            decision_operation_payload_hash("operation-replay", "edit", "دەقی ڕاستکراوە", "Sara")
        );

        // A fresh CouchState models the process/session restart that defeated the former in-memory
        // identity. The original stale rowVersion is intentional: the immutable receipt must ACK
        // before mutable row state, leases, or undo memory are consulted.
        let restarted_state = state();
        let (code, _, response, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Sara", &restarted_state);
        assert_eq!(code, 200);
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["duplicate"], true);
        assert_eq!(counts(), (1, 1, 1), "a restart replay must be side-effect free");
        assert!(lock_state(&restarted_state).undo.is_empty(), "a replay is not a second undoable act");
    }

    #[test]
    fn a_bound_operation_uuid_rejects_any_segment_action_text_or_reviewer_change() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("operation-a", "دەقی خاو")).unwrap();
        db.insert_segment(&seg("operation-b", "دەقی دوو")).unwrap();
        let operation_id = "22222222-2222-4222-8222-222222222222";
        let base = serde_json::json!({
            "operationId": operation_id,
            "id": "operation-a",
            "action": "edit",
            "text": "دەقی ڕاستکراوە",
            "reviewer": "Sara",
            "rowVersion": db.segment_row_stamp("operation-a").unwrap().unwrap(),
            "heardMs": 600_000,
        });
        let state = state();
        assert_eq!(super::api_decision(&db, base.to_string().as_bytes(), "Sara", &state).0, 200);

        let count = || -> (i64, i64) {
            db.connection()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM review_events),
                            (SELECT COUNT(*) FROM review_compensation_ledger)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
        };
        let committed = count();
        assert_eq!(committed, (1, 1));

        let mut changed_segment = base.clone();
        changed_segment["id"] = serde_json::json!("operation-b");
        let mut changed_action = base.clone();
        changed_action["action"] = serde_json::json!("accept");
        let mut changed_text = base.clone();
        changed_text["text"] = serde_json::json!("دەقێکی تر");
        let mut changed_reviewer = base.clone();
        changed_reviewer["reviewer"] = serde_json::json!("Hemn");

        for (label, payload, authenticated_reviewer) in [
            ("segment", changed_segment, "Sara"),
            ("action", changed_action, "Sara"),
            ("text", changed_text, "Sara"),
            ("reviewer", changed_reviewer, "Hemn"),
        ] {
            let (code, _, message, ..) =
                super::api_decision(&db, payload.to_string().as_bytes(), authenticated_reviewer, &state);
            assert_eq!(code, 409, "changing {label} while reusing a UUID must conflict");
            assert!(
                String::from_utf8_lossy(&message).contains("operation UUID"),
                "the {label} mismatch must be rejected by the durable operation fence"
            );
            assert_eq!(count(), committed, "a rejected {label} reuse must not add event or pay");
        }
        assert!(!db.get_segment_by_id("operation-b").unwrap().unwrap().verified);
    }

    #[test]
    fn a_new_write_requires_a_canonical_operation_uuid_before_any_side_effect() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("operation-required", "دەقی خاو")).unwrap();
        let base = serde_json::json!({
            "id": "operation-required",
            "action": "accept",
            "text": "دەقی خاو",
            "rowVersion": db.segment_row_stamp("operation-required").unwrap().unwrap(),
            "heardMs": 600_000,
        });
        let state = state();
        let (code, _, message, ..) = super::api_decision(&db, base.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 400);
        assert!(String::from_utf8_lossy(&message).contains("operationId"));

        let mut malformed = base;
        malformed["operationId"] = serde_json::json!("not-a-uuid");
        let (code, ..) = super::api_decision(&db, malformed.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 400);
        let writes: i64 = db
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM playback_receipts)
                      + (SELECT COUNT(*) FROM review_events)
                      + (SELECT COUNT(*) FROM review_compensation_ledger)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(writes, 0, "missing/malformed operation identity must fail before every write");
    }

    #[test]
    fn the_phone_persists_the_operation_uuid_before_its_first_decision_post() {
        let page = include_str!("../assets/couch.html");
        let decide = page.split_once("async function decide(action)").map(|(_, tail)| tail).expect("decision function");
        let persist = decide.find("queueSubmission(submission);").expect("durable outbox write");
        let post = decide.find("await api('/api/decision'").expect("decision POST");
        assert!(persist < post, "the UUID-bearing outbox write must happen before the first POST");
        assert!(decide.contains("operationId: newOperationId()"));
        assert!(page.contains("removeSubmission(item.operationId)"));
        assert!(
            !decide.contains("filter((q) => q.id !== submission.id)"),
            "outbox identity/removal must never collapse two operations on the same segment"
        );
    }

    #[test]
    fn two_tabs_queue_distinct_operations_without_a_shared_array_overwrite() {
        let page = include_str!("../assets/couch.html");
        assert!(page.contains("const OUTBOX_OPERATION_PREFIX = OUTBOX + '.operation.';"));
        assert!(page.contains("const outboxOperationKey = (operationId) => OUTBOX_OPERATION_PREFIX + operationId;"));

        let writer = page
            .split_once("function writeOperationRecord(submission, queuedAtMs, legacyOrder)")
            .and_then(|(_, tail)| tail.split_once("function readLegacyOutbox()"))
            .map(|(body, _)| body)
            .expect("per-operation writer");
        assert!(writer.contains("const key = outboxOperationKey(submission.operationId);"));
        assert!(writer.contains("localStorage.setItem(key, encodeOperationRecord("));
        assert!(!writer.contains("localStorage.setItem(OUTBOX,"));

        let queue = page
            .split_once("function queueSubmission(submission)")
            .and_then(|(_, tail)| tail.split_once("function removeSubmission(operationId)"))
            .map(|(body, _)| body)
            .expect("queue writer");
        assert!(queue.contains("writeOperationRecord(submission, Date.now(), 0);"));
        assert!(!queue.contains("writeOutbox([...queued"));
        assert!(!queue.contains("localStorage.setItem(OUTBOX,"));

        // Model the only fact the two-tab interleaving needs: A and B address disjoint atomic keys.
        // Whichever setItem runs last can replace only its own UUID, never the other operation.
        let prefix = "cortex.couch.outbox.operation.";
        let tab_a = format!("{prefix}aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        let tab_b = format!("{prefix}bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
        assert_ne!(tab_a, tab_b);
        assert!(page.contains("localStorage.removeItem(outboxOperationKey(operationId));"));

        // UUID-less legacy arrays may mint identities only inside the cross-tab migration lock; an
        // unsupported browser fails visibly and preserves the array instead of racing two UUIDs.
        assert!(page.contains("navigator.locks.request("));
        assert!(page.contains("return migrateLegacyOutboxLocked(false);"));
        assert!(page.contains("if (!mayAssignIds) return false;"));
        assert!(page.contains("legacyFingerprint(current) !== legacyFingerprint(identified)"));
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
        let (code, _, reply, ..) = api_renew(body.as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "an active reviewer may extend their own hold");
        let json: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(json["ttlSeconds"], LEASE_TTL.as_secs(), "the page is told the real TTL, not a guess");
        assert!(queue_ids(&db, "Hemn", &state).is_empty(), "the renewed clip is still Sara's");

        // Renewal is NOT a way to take someone else's clip.
        let (code, _, msg, ..) = api_renew(body.as_bytes(), "Hemn", &state);
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
    fn durable_review_pauses_instead_of_serving_work_after_quality_keys_run_dry() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("paid-work", "دەقی کار")).unwrap();
        let state = Mutex::new(CouchState {
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            pairing_codes: HashMap::from([("test-token".to_string(), "Sara".to_string())]),
            ..CouchState::default()
        });

        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 503, "a durable paid-review session must fail closed when its hidden pool is empty");
        assert!(
            String::from_utf8_lossy(&body).contains("hidden-check capacity"),
            "the reviewer gets a retryable operational reason, not a fake empty/success queue"
        );
        assert!(
            lock_state(&state).leases.is_empty(),
            "a batch refused before delivery must release its work leases immediately"
        );

        gold_seg(&db, "quality-key", "دەقی هەڵە", "دەقی ڕاست");
        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 200, "adding a genuine answer key unpauses the next fetch without a restart");
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["items"].as_array().unwrap().len(), 2, "one work clip plus its required hidden check");
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

        let candidates = db.list_spot_check_candidates(10, "Sara", &HashSet::new(), None, None).unwrap();
        assert_eq!(candidates.len(), 2, "both clips have a human answer that differs from the raw draft");
        // A check is graded only because it was SERVED as one; mark both as served to each reviewer,
        // which is exactly what api_queue records when it salts a batch.
        for id in ["g1", "g2"] {
            for who in ["Sara", "Hemn"] {
                lock_state(&state).spot_checks.insert((id.to_string(), who.to_string()));
            }
        }

        // Sara LISTENS: she corrects the wrong draft to the known answer.
        let fix =
            serde_json::json!({"heardMs": 600_000, "id": "g1", "action": "edit", "text": "دەقی ڕاست"}).to_string();
        assert_eq!(api_decision(&db, fix.as_bytes(), "Sara", &state).0, 200);
        // Hemn does NOT: he hands the wrong draft straight back.
        let blind =
            serde_json::json!({"heardMs": 600_000, "id": "g1", "action": "accept", "text": "دەقی هەڵە"}).to_string();
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
        assert!(lock_state(&state).undo.get("Hemn").map_or(true, Vec::is_empty), "nor leave an undo entry");

        // A retry from the immediately previous page build may have no rowVersion. The immutable score
        // is already durable, so acknowledge it without appending another generic pay-shaped event.
        let events_before: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(super::api_decision(&db, blind.as_bytes(), "Hemn", &state).0, 200);
        assert_eq!(of("Hemn").checks, 1, "a retried spot check is still ONE check, not two");
        let events_after: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(events_after, events_before, "a retried hidden check must not append another audit/pay event");

        // This is a known-valid answer key. Rejecting it is wrong, and must not make a reject-spammer
        // look trustworthy merely because they avoided the Accept button.
        let reject = serde_json::json!({"heardMs": 600_000, "id": "g2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, reject.as_bytes(), "Hemn", &state).0, 200);
        let hemn = of("Hemn");
        assert_eq!((hemn.checks, hemn.noticed), (2, 0), "rejecting a known-valid clip must fail the check");

        // The first answer is the measurement. A later replay with a corrected answer cannot erase
        // the evidence that the reviewer originally blind-accepted this hidden check.
        let events_before_improve: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        let improve =
            serde_json::json!({"heardMs": 600_000, "id": "g1", "action": "edit", "text": "دەقی ڕاست"}).to_string();
        assert_eq!(api_decision(&db, improve.as_bytes(), "Hemn", &state).0, 200);
        let hemn = of("Hemn");
        assert_eq!((hemn.checks, hemn.noticed), (2, 0), "a replay cannot improve an immutable first score");
        let events_after_improve: i64 =
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap();
        assert_eq!(
            events_after_improve, events_before_improve,
            "a changed replay is still a no-op after the first score"
        );
    }

    #[test]
    fn a_spot_check_write_failure_is_retryable_and_never_claimed_saved() {
        // A score is the hidden check's only durable result. Returning 200 when that INSERT fails
        // makes the phone delete its outbox entry, permanently loses the first answer, and lets a
        // later attempt become the measurement. Force precisely that INSERT to fail without a busy
        // timeout, then prove the same queued payload lands once the database recovers.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        gold_seg(&db, "score-failure", "دەقی هەڵە", "دەقی ڕاست");
        let state = state();
        lock_state(&state).spot_checks.insert(("score-failure".to_string(), "Sara".to_string()));
        db.connection()
            .execute_batch(
                "CREATE TRIGGER force_spot_score_failure
                 BEFORE INSERT ON spot_checks
                 BEGIN
                   SELECT RAISE(ABORT, 'forced score write failure');
                 END;",
            )
            .unwrap();

        let body = serde_json::json!({
            "heardMs": 600_000,
            "id": "score-failure",
            "action": "edit",
            "text": "دەقی ڕاست",
        })
        .to_string();
        let event_count = || -> i64 {
            db.connection().query_row("SELECT COUNT(*) FROM review_events", [], |row| row.get(0)).unwrap()
        };
        let events_before = event_count();

        let (code, _, response, ..) = api_decision(&db, body.as_bytes(), "Sara", &state);
        assert_eq!(code, 500, "an unpersisted score is a retryable server failure, never success");
        let message = String::from_utf8_lossy(&response);
        assert!(message.contains("decision not recorded"));
        assert!(!message.to_ascii_lowercase().contains("spot"), "the failure response must not reveal a hidden check");
        assert!(db.spot_check_report().unwrap().is_empty(), "the forced failure must leave no partial score");
        assert_eq!(event_count(), events_before, "an unpersisted score must not append an audit/pay event");
        let key = db.get_segment_by_id("score-failure").unwrap().unwrap();
        assert_eq!(key.verdict_transcript.as_deref(), Some("دەقی ڕاست"), "the answer-key row stays untouched");

        db.connection().execute_batch("DROP TRIGGER force_spot_score_failure").unwrap();
        assert_eq!(api_decision(&db, body.as_bytes(), "Sara", &state).0, 200, "the queued retry must land");
        let report = db.spot_check_report().unwrap();
        assert_eq!((report.len(), report[0].checks, report[0].noticed), (1, 1, 1));
        let events_after_success = event_count();
        assert_eq!(events_after_success, events_before + 1, "one landed first answer produces one audit event");

        // Model the rolling-deploy outbox: the original payload has no rowVersion. Once the score is
        // durable, the pre-version duplicate ACK must drain it without another score or event.
        assert_eq!(super::api_decision(&db, body.as_bytes(), "Sara", &state).0, 200);
        let report = db.spot_check_report().unwrap();
        assert_eq!((report[0].checks, report[0].noticed), (1, 1));
        assert_eq!(event_count(), events_after_success, "a completed retry is side-effect free");
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

        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
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
        let (_, _, body, ..) = api_queue(&db, "Hemn", &state);
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

    /// The session cookie header value — the ONLY credential the server accepts.
    ///
    /// Every test below presents its token this way because that is the only way a real reviewer's
    /// browser can present one: the fragment is claimed once via POST /api/claim and lives in an
    /// HttpOnly cookie from then on. These call sites used to put the token in `?t=`, which the
    /// server honoured; when that path was removed the tests had to move with it, or they would have
    /// gone on proving a door that no longer exists.
    fn cookie_for(token: &str) -> String {
        format!("{COUCH_COOKIE}={token}")
    }

    fn tls_agent(data_dir: &Path) -> ureq::Agent {
        let saved: SavedTlsIdentity = serde_json::from_slice(
            &std::fs::read(data_dir.join(TLS_IDENTITY_FILE)).expect("read generated Couch TLS identity"),
        )
        .expect("parse generated Couch TLS identity");
        let mut roots = rustls::RootCertStore::empty();
        let mut pem = std::io::Cursor::new(saved.certificate_pem.as_bytes());
        for certificate in rustls_pemfile::certs(&mut pem) {
            roots
                .add(certificate.expect("parse generated Couch TLS certificate"))
                .expect("trust generated certificate");
        }
        let config = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
        ureq::AgentBuilder::new().tls_config(Arc::new(config)).timeout(std::time::Duration::from_secs(10)).build()
    }

    /// Drive `handle_request`'s auth + throttle path without a live socket, by issuing a real request
    /// against a loopback server and handing it to the handler exactly as an accept thread would.
    ///
    /// Presents the token as the COOKIE, because that is now the only credential the server accepts
    /// and therefore the only shape a real reviewer's request ever has. This used to send `?t=`; when
    /// the query form was removed these tests would have gone on passing a credential the product no
    /// longer honours, and a harness that authenticates by a route real traffic cannot use tests
    /// nothing.
    fn route_for_test(db: &Database, state: &Mutex<CouchState>, token: &str) -> Reply {
        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/api/queue");
        let cookie = format!("{COUCH_COOKIE}={token}");
        let client = std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(5)).build();
            let _ = agent.get(&url).set("Cookie", &cookie).call();
        });
        let mut request = server.recv().unwrap().unwrap();
        let (reply, cookie) = handle_request(&mut request, db, state);
        let _ = respond_with_cookie(request, reply.clone(), cookie);
        let _ = client.join();
        reply
    }

    #[test]
    fn a_valid_token_in_the_query_string_is_worthless() {
        // The last step of REMOTE_PUBLIC_LINKS_PLAN phase 2, and the prerequisite for Funnel.
        //
        // WHAT THIS BUYS. A chat platform's preview bot fetches any pasted link within seconds. While
        // `?t=` authenticated, that fetch returned the reviewer's queue AND a Set-Cookie, handing the
        // platform a durable credential to biometric audio. The token here is the REAL one, presented
        // exactly as a careless paste would present it, and it must buy nothing at all.
        //
        // Fail-before: with the query branch still in token_from_request this returns 200 with the
        // clip list, which is the leak the plan calls "certain, not tail-risk".
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

        // Data route with a VALID token in the query: refused.
        //
        // Reported as a STATUS rather than matched as a Result: under the full 1157-test suite this
        // request can lose its race with the shared runtime and come back as a transport timeout,
        // and `matches!(.., Status(401))` turned that into "the leak is open" - the most alarming
        // possible message for what was really a slow machine. A timeout is not evidence either way,
        // so it says so.
        let data = agent.get(&format!("{base}/api/queue?t=goodtoken")).call();
        let code = match data {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(c, _)) => c,
            Err(other) => {
                panic!("no verdict: the request never completed ({other}) - rerun, do not read this as a pass")
            }
        };
        assert_eq!(code, 401, "a valid token in the QUERY must not open the data - that is the preview-bot leak");

        // The page shell with the same query token: served (it is public), but it must not mint the
        // cookie, or the bot walks away with a session anyway.
        let shell = agent.get(&format!("{base}/?t=goodtoken")).call().expect("the shell stays public");
        assert_eq!(shell.status(), 200);
        assert!(
            shell.header("set-cookie").is_none(),
            "a query token must never mint a session cookie - the bot would keep it for a week"
        );

        // And the legitimate path still works, so this is a closed door and not a broken lock.
        let claim = agent
            .post(&format!("{base}/api/claim"))
            .send_json(ureq::json!({ "token": "goodtoken" }))
            .expect("the fragment claim is how a real reviewer gets in");
        let cookie = claim.header("set-cookie").expect("claim mints the cookie").to_string();
        let jar = cookie.split(';').next().unwrap().to_string();
        let ok = agent.get(&format!("{base}/api/queue")).set("Cookie", &jar).call().expect("cookie opens the data");
        assert_eq!(ok.status(), 200);

        shutdown.store(true, Ordering::SeqCst);
        let _ = agent.get(&format!("{base}/")).call();
        let _ = join.join();
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
        let _ = server.unblock();
        let _ = join.join();
    }

    #[test]
    fn a_returning_reviewer_is_let_in_by_their_cookie_alone() {
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

        // The return visit, exactly as the page now issues it: no token in the URL at all, cookie
        // present. This is the reported bug's fix and it still holds.
        assert_eq!(
            status(format!("{base}/api/queue"), Some(&cookie)),
            200,
            "a returning reviewer must be let in by their cookie alone"
        );
        // A leftover empty `?t=` changes nothing either way - the query is not read at all now.
        assert_eq!(status(format!("{base}/api/queue?t="), Some(&cookie)), 200);
        // And the gate is NOT loosened: no credential still fails.
        assert_eq!(status(format!("{base}/api/queue"), None), 401, "no credential is not a way in");
        assert_eq!(status(format!("{base}/api/queue?t="), None), 401, "nor is an empty token");

        // REVOCATION, asserted where it now lives: in the COOKIE. A revoked reviewer's browser keeps
        // sending the cookie it was given, and that cookie must stop opening anything.
        let revoked = format!("{COUCH_COOKIE}=revoked");
        assert_eq!(status(format!("{base}/api/queue"), Some(&revoked)), 401, "a revoked token in the cookie is dead");

        // DROPPED with the query path: the old assertion that a wrong token in `?t=` must not fall
        // back to a valid cookie. That guarded a real hazard while URLs carried credentials - a
        // revoked link could otherwise ride someone's stale cookie. With the query ignored entirely
        // the hazard is gone rather than unguarded: a URL token is not a credential any more, and the
        // holder of a valid cookie is simply themselves. Recorded rather than silently deleted,
        // because "this assertion disappeared" and "this protection disappeared" are different facts.

        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock();
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
        let request = server.recv().unwrap().unwrap();
        let _ = respond_with_cookie(request, (200, "audio/wav", vec![0u8; BIG], vec![]), None);
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
        assert!(
            !lock_state(&state).served_work.iter().any(|(_, who)| who.eq_ignore_ascii_case("Sara")),
            "revocation must clear queue-issued object grants, not only bearer tokens and leases"
        );
        // Simulate later authorizing a new cookie with the same display name. Identity reuse must not
        // resurrect the old queue receipt; this reviewer has to fetch a fresh queue like anyone else.
        lock_state(&state).reviewers.insert("tok-sara-new".into(), "Sara".into());
        assert_eq!(
            api_renew(serde_json::json!({"id":"rv1"}).to_string().as_bytes(), "Sara", &state).0,
            409,
            "a re-added name cannot renew work delivered before its revocation"
        );
        lock_state(&state).reviewers.remove("tok-sara-new");

        // Her token no longer resolves to anyone, so every route refuses it...
        assert_eq!(route_for_test(&db, &state, "tok-sara").0, 401, "a revoked token must stop working at once");
        // ...while Hemn is entirely unaffected...
        assert_eq!(route_for_test(&db, &state, "tok-hemn").0, 200, "revoking one link must not touch another");
        // ...and the clip she was holding is back in the pool rather than stranded behind her.
        assert_eq!(queue_ids(&db, "Hemn", &state), vec!["rv1"], "a revoked reviewer's held work returns");
    }

    #[test]
    fn revoke_orders_against_an_already_authenticated_slow_request() {
        // Authentication used to copy `(token, reviewer)` out of CouchState and discard the token.
        // A POST that paused after that copy could then commit after Revoke returned. Model the
        // protected operation directly so this is a deterministic interleaving, not a timing-only
        // socket test: revoke must wait for the already-authorized operation, and the same copied
        // identity must be refused if it reaches the commit boundary after revoke.
        let issued = SystemTime::now();
        let state = Arc::new(Mutex::new(CouchState {
            pairing_codes: HashMap::from([
                ("pair-sara".to_string(), "Sara".to_string()),
                ("pair-hemn".to_string(), "Hemn".to_string()),
            ]),
            reviewers: HashMap::from([
                ("session-sara".to_string(), "Sara".to_string()),
                ("session-hemn".to_string(), "Hemn".to_string()),
            ]),
            session_issued: HashMap::from([("session-sara".to_string(), issued), ("session-hemn".to_string(), issued)]),
            ..CouchState::default()
        }));

        let (inside_tx, inside_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let request_state = state.clone();
        let request = std::thread::spawn(move || {
            with_live_reviewer("session-sara", "Sara", &request_state, || {
                inside_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                json_reply(200, serde_json::json!({ "ok": true }))
            })
        });
        inside_rx.recv_timeout(Duration::from_secs(2)).expect("protected request reached its commit boundary");

        let (revoked_tx, revoked_rx) = std::sync::mpsc::channel();
        let revoke_state = state.clone();
        let revoker = std::thread::spawn(move || {
            revoked_tx.send(revoke_in(&revoke_state, "Sara")).unwrap();
        });
        assert!(
            revoked_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "Revoke must not return while an earlier authorized request can still commit"
        );

        release_tx.send(()).unwrap();
        assert_eq!(request.join().unwrap().0, 200, "the operation ordered before revoke may finish");
        revoked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("revoke completes after the protected operation")
            .expect("revoke succeeds");
        revoker.join().unwrap();

        let ran_after_revoke = Arc::new(AtomicBool::new(false));
        let ran = ran_after_revoke.clone();
        let denied = with_live_reviewer("session-sara", "Sara", &state, || {
            ran.store(true, Ordering::SeqCst);
            json_reply(200, serde_json::json!({ "ok": true }))
        });
        assert_eq!(denied.0, 401, "a copied identity reaching the boundary after revoke is unauthorized");
        assert!(!ran_after_revoke.load(Ordering::SeqCst), "the protected operation must not run after revoke");
    }

    #[test]
    fn revoke_waits_until_an_authorized_response_delivery_releases_its_guard() {
        // `request.respond` happens after the route has materialized its Reply. Holding only the
        // operation guard would still allow already-authorized audio bytes to be transmitted after
        // Revoke returned. The production accept loop holds this exact guard through the synchronous
        // send; prove the writer side cannot overtake it.
        let issued = SystemTime::now();
        let state = Arc::new(Mutex::new(CouchState {
            pairing_codes: HashMap::from([
                ("pair-delivery".to_string(), "DeliveryReviewer".to_string()),
                ("pair-other".to_string(), "OtherReviewer".to_string()),
            ]),
            reviewers: HashMap::from([
                ("session-delivery".to_string(), "DeliveryReviewer".to_string()),
                ("session-other".to_string(), "OtherReviewer".to_string()),
            ]),
            session_issued: HashMap::from([
                ("session-delivery".to_string(), issued),
                ("session-other".to_string(), issued),
            ]),
            ..CouchState::default()
        }));
        let delivery = live_session_guard("session-delivery", None, &state).expect("delivery is authorized");

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let revoke_state = state.clone();
        let revoker = std::thread::spawn(move || {
            result_tx.send(revoke_in(&revoke_state, "DeliveryReviewer")).unwrap();
        });
        assert!(
            result_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "Revoke must wait until the synchronous protected response has finished"
        );
        drop(delivery);
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("revoke completes after response delivery")
            .expect("revoke succeeds");
        revoker.join().unwrap();
    }

    #[test]
    fn a_reviewer_who_cannot_judge_a_clip_says_so_instead_of_guessing() {
        // R4.4. A reviewer facing a clip they genuinely cannot call — two people talking over each
        // other, an accent they do not have, audio that will not play — had two ways forward, and BOTH
        // write a judgement they cannot stand behind: "Looks good" promotes an unheard draft to gold,
        // "Reject" permanently excludes a clip that may be perfectly fine. A guess is worse for the
        // corpus than an honest "I don't know", because nothing downstream can tell the two apart.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("sk1", "دەق یەک")).unwrap();
        db.insert_segment(&seg("sk2", "دەق دوو")).unwrap();
        let state = state();
        // SORTED, not insertion order. `get_segments` is `created_at DESC, id ASC` and created_at has
        // one-second resolution, so on a loaded machine these two land in different seconds and come
        // back newest-first — which is correct, and not what this test is about. (Caught by the full
        // suite: it passed serially and failed under parallel load.)
        let mut served = queue_ids(&db, "Sara", &state);
        served.sort();
        assert_eq!(served, vec!["sk1", "sk2"]);

        let before = db.get_segment_by_id("sk1").unwrap().unwrap();
        let skip = serde_json::json!({"heardMs": 600_000, "id": "sk1", "action": "skip"}).to_string();
        assert_eq!(api_decision(&db, skip.as_bytes(), "Sara", &state).0, 200);

        // NOTHING reached the row: not the decision, not the verdict, not `verified`, not the name.
        // A skip is the ABSENCE of a judgement, so the corpus must carry no trace of one.
        let after = db.get_segment_by_id("sk1").unwrap().unwrap();
        assert_eq!(after.human_decision, None, "a skip is not a decision");
        assert_eq!(after.verdict, before.verdict, "a skip does not touch the jury verdict");
        assert!(!after.verified, "a skipped clip has NOT been reviewed");
        assert_eq!(after.reviewed_by, None, "nobody judged it, so nobody's name goes on it");
        assert_eq!(after.annotated_transcript, before.annotated_transcript);

        // It is not handed straight back to the reviewer who could not judge it — otherwise every
        // refill returns the same wall of clips and the button is a treadmill. The page is also told
        // how many she skipped, so the empty state cannot claim "🎉 all clips reviewed" over them.
        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ids: Vec<&str> = payload["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["sk2"], "a skipped clip stops being Sara's work");
        assert_eq!(payload["skippedByYou"], 1, "the page must be able to say what happened to it");
        assert_eq!(
            payload["pendingTotal"], 2,
            "skipping does not shrink the backlog — a human still owes it a verdict"
        );

        // ...and it goes to somebody who CAN judge it, immediately: the lease is released with the
        // skip rather than left to expire, so the next reviewer does not wait out the TTL.
        assert_eq!(queue_ids(&db, "Hemn", &state), vec!["sk1"], "the clip is still everyone else's work");

        // The owner can see that a human met this clip and could not call it. That is the signal —
        // a clip several people skip is telling you something about the clip.
        let rows: Vec<(String, String, String)> = db
            .connection()
            .prepare("SELECT segment_id, reviewer, action FROM review_events ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rows, vec![("sk1".to_string(), "Sara".to_string(), "skip".to_string())]);

        // But a skip is NOT throughput. Counting it would credit a reviewer for work they explicitly
        // did not do, and inflate the one number that says how fast the corpus is really being read.
        assert!(db.reviewer_throughput().unwrap().is_empty(), "an unjudged clip is not a reviewed clip");
    }

    #[test]
    fn a_skipped_spot_check_is_not_handed_back_in_every_batch_forever() {
        // The skip filter lives in the loop over PENDING rows. Spot checks are inserted after that
        // loop, from `list_spot_check_candidates` — so the filter never sees them. And the DB-level
        // exclusion there is `id NOT IN (SELECT segment_id FROM spot_checks WHERE reviewer = ?)`,
        // which lists clips the reviewer was SCORED on; a skip is the absence of an answer and writes
        // no score. Both true at once means the one clip a reviewer has said they cannot judge comes
        // back in every batch, and the button they used to escape it does nothing for this clip.
        //
        // The fix must not let anyone DODGE measurement, only avoid re-serving the same clip: with two
        // answer keys available, skipping one has to yield the other, not zero checks.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        for n in 0..3 {
            db.insert_segment(&seg(&format!("w{n}"), "کاری ڕاستەقینە")).unwrap();
        }
        gold_seg(&db, "gold-a", "دەقی هەڵە", "دەقی ڕاست");
        gold_seg(&db, "gold-b", "دەقی هەڵەی دوو", "دەقی ڕاستی دوو");
        let state = state();

        let first = queue_ids(&db, "Sara", &state);
        let check = ["gold-a", "gold-b"]
            .into_iter()
            .find(|g| first.iter().any(|id| id == g))
            .expect("the batch must carry a spot check to skip");

        let skip = serde_json::json!({"heardMs": 600_000, "id": check, "action": "skip"}).to_string();
        assert_eq!(api_decision(&db, skip.as_bytes(), "Sara", &state).0, 200);

        let second = queue_ids(&db, "Sara", &state);
        assert!(
            !second.iter().any(|id| id == check),
            "the spot check Sara said she could not judge was handed straight back: {second:?}"
        );
        // ...and she is still being measured, on the OTHER key. Dropping her out of measurement
        // entirely would turn the honest exit into a way to never be tested again.
        let other = if check == "gold-a" { "gold-b" } else { "gold-a" };
        assert!(
            second.iter().any(|id| id == other),
            "skipping one check must yield a DIFFERENT one, not none: {second:?}"
        );
    }

    #[test]
    fn every_decision_answers_with_updated_activity_and_compensation() {
        // Full judged audio and weighted pay are distinct, and both used to refresh only when a batch
        // refilled. Every reply that follows real work must carry the same fresh accounting payload, including
        // the two paths that return early: a spot check (audited as real work, so it moves the total
        // — miss it and the badge freezes on roughly every eighth clip) and a duplicate retry (the
        // dropped-response case, when a reviewer is least sure their save landed).
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        for (id, ms) in [("r1", 9000), ("r2", 21000)] {
            let mut s = seg(id, "دەق");
            s.duration_ms = ms;
            db.insert_segment(&s).unwrap();
        }
        let state = state();
        let accounting_of = |reply: Reply| -> (i64, String) {
            assert_eq!(reply.0, 200);
            let payload = serde_json::from_slice::<serde_json::Value>(&reply.2).unwrap();
            assert_eq!(payload["accountingAvailable"], true);
            assert_eq!(payload["payPolicyVersion"], crate::db::REVIEW_PAY_POLICY_VERSION);
            (
                payload["reviewedMs"].as_i64().expect("every decision reply carries reviewedMs"),
                payload["earnedMicroIqd"].as_str().expect("micro-IQD is an exact decimal string").to_string(),
            )
        };

        let first = serde_json::json!({"heardMs": 600_000, "id": "r1", "action": "accept", "text": "دەق"}).to_string();
        assert_eq!(
            accounting_of(api_decision(&db, first.as_bytes(), "Sara", &state)),
            (9_000, "4500000".into()),
            "an unchanged accept is 10%"
        );
        let second = serde_json::json!({"heardMs": 600_000, "id": "r2", "action": "bad"}).to_string();
        assert_eq!(
            accounting_of(api_decision(&db, second.as_bytes(), "Sara", &state)),
            (30_000, "15000000".into()),
            "reject adds full judged-audio progress but only 10% compensation"
        );
        assert_eq!(
            accounting_of(api_decision(&db, first.as_bytes(), "Sara", &state)),
            (30_000, "15000000".into()),
            "the retry path answers with both totals and does not double-count"
        );

        // And the spot-check path, which returns before any of the machinery above.
        gold_seg(&db, "r-gold", "دەقی هەڵە", "دەقی ڕاست");
        lock_state(&state).spot_checks.insert(("r-gold".to_string(), "Sara".to_string()));
        let check = serde_json::json!({"heardMs": 600_000, "id": "r-gold", "action": "accept", "text": "دەقی ڕاست"})
            .to_string();
        let (reviewed, earned) = accounting_of(api_decision(&db, check.as_bytes(), "Sara", &state));
        assert!(reviewed > 30_000, "a spot check is audited as judged work");
        assert_eq!(earned, "22500000", "correcting the 1.5-second hidden check earns the 100% edit rate");
    }

    #[test]
    fn hidden_check_pay_compares_against_the_raw_text_served_while_qc_uses_the_answer_key() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let mut key = seg("pay-hidden", "raw wrong draft");
        key.verified = true;
        key.is_gold = true;
        key.human_decision = Some("edit".into());
        key.verdict = Some("human_edit".into());
        key.annotated_transcript = Some("human answer".into());
        key.verdict_transcript = Some("human answer".into());
        db.insert_segment_full(&key).unwrap();
        let state = state();
        for reviewer in ["Sara", "Hemn", "Rebaz"] {
            lock_state(&state).spot_checks.insert((key.id.clone(), reviewer.to_string()));
        }

        // Sara corrects the RAW draft to the hidden answer: edit/100%, and QC passes.
        let correction = serde_json::json!({
            "heardMs": 600_000,
            "id": key.id.clone(),
            "action": "accept",
            "text": "human answer"
        })
        .to_string();
        assert_eq!(api_decision(&db, correction.as_bytes(), "Sara", &state).0, 200);

        // Hemn returns the exact RAW draft he was shown: accept/10%, even though QC correctly fails.
        let blind_accept = serde_json::json!({
            "heardMs": 600_000,
            "id": key.id.clone(),
            "action": "accept",
            "text": "raw wrong draft"
        })
        .to_string();
        assert_eq!(api_decision(&db, blind_accept.as_bytes(), "Hemn", &state).0, 200);

        // Reject remains reject/10%; its QC result is independently wrong because this key is valid.
        let reject = serde_json::json!({"heardMs": 600_000, "id": key.id.clone(), "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, reject.as_bytes(), "Rebaz", &state).0, 200);

        let ledger: Vec<(String, String, i64, i64)> = db
            .connection()
            .prepare(
                "SELECT reviewer, compensation_action, delta_micro_iqd, delta_corrected_ms
                   FROM review_compensation_ledger WHERE segment_id = 'pay-hidden' ORDER BY reviewer",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            ledger,
            vec![
                ("Hemn".into(), "accept".into(), 750_000, 0),
                ("Rebaz".into(), "reject".into(), 750_000, 0),
                ("Sara".into(), "edit".into(), 7_500_000, 1_500),
            ],
            "hidden-check compensation must use the raw transcript that the queue actually served"
        );

        let report = db.spot_check_report().unwrap();
        let score = |reviewer: &str| {
            report.iter().find(|row| row.reviewer == reviewer).expect("reviewer has a hidden-check score")
        };
        assert_eq!((score("Sara").checks, score("Sara").noticed, score("Sara").mean_cer), (1, 1, 0.0));
        assert_eq!((score("Hemn").checks, score("Hemn").noticed), (1, 0));
        assert_eq!((score("Rebaz").checks, score("Rebaz").noticed), (1, 0));
    }

    #[test]
    fn formatting_only_phone_changes_cannot_mint_edit_rate_compensation() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        for (id, raw) in [("pay-nfc", "é"), ("pay-space", "hello world"), ("pay-real", "hello world")] {
            let mut segment = seg(id, raw);
            segment.duration_ms = 1_000;
            db.insert_segment(&segment).unwrap();
        }
        let state = state();

        let nfd = serde_json::json!({
            "heardMs": 600_000,
            "id": "pay-nfc",
            "action": "edit",
            "text": "e\u{301}"
        })
        .to_string();
        assert_eq!(api_decision(&db, nfd.as_bytes(), "Sara", &state).0, 200);

        let spaces = serde_json::json!({
            "heardMs": 600_000,
            "id": "pay-space",
            "action": "edit",
            "text": "  hello    world  "
        })
        .to_string();
        assert_eq!(api_decision(&db, spaces.as_bytes(), "Sara", &state).0, 200);

        let real = serde_json::json!({
            "heardMs": 600_000,
            "id": "pay-real",
            "action": "edit",
            "text": "hello world!"
        })
        .to_string();
        assert_eq!(api_decision(&db, real.as_bytes(), "Sara", &state).0, 200);

        let rows: Vec<(String, String, i64)> = db
            .connection()
            .prepare(
                "SELECT segment_id, compensation_action, delta_micro_iqd
                   FROM review_compensation_ledger ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            rows,
            vec![
                ("pay-nfc".into(), "accept".into(), 500_000),
                ("pay-space".into(), "accept".into(), 500_000),
                ("pay-real".into(), "edit".into(), 5_000_000),
            ],
            "only a material retained-text correction may earn 100%"
        );
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

        let edit =
            serde_json::json!({"heardMs": 600_000, "id": "e1", "action": "edit", "text": "ڕاستکراوە"}).to_string();
        assert_eq!(api_decision(&db, edit.as_bytes(), "Sara", &state).0, 200);
        let reject = serde_json::json!({"heardMs": 600_000, "id": "e2", "action": "bad"}).to_string();
        assert_eq!(api_decision(&db, reject.as_bytes(), "Hemn", &state).0, 200);
        // A spot check is real work to the reviewer who did it, so it is audited too.
        let check = serde_json::json!({"heardMs": 600_000, "id": "e-gold", "action": "accept", "text": "دەقی ڕاست"})
            .to_string();
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
    fn failed_initial_session_save_publishes_no_server_handle_or_links_and_keeps_revocation() {
        let _serial = GLOBAL_SESSION_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("failed-start.db").to_string_lossy().to_string();
        Database::open(&db_path).unwrap().initialize().unwrap();
        write_session_revocation(tmp.path()).unwrap();
        let marker_before = std::fs::read(session_revocation_path(tmp.path())).unwrap();
        let port_probe = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = port_probe.local_addr().unwrap().port();
        drop(port_probe);

        let result = start_on_port_with_session_lifecycle(
            db_path,
            vec!["Sara".into(), "Hemn".into()],
            port,
            Some(tmp.path().to_path_buf()),
            |_| Err("injected initial session save failure".to_string()),
            clear_session_revocation,
        );
        let error = result.err();
        let was_running = is_running();
        let published = status();
        let marker_after = std::fs::read(session_revocation_path(tmp.path())).ok();
        let session_exists = session_path(tmp.path()).exists();
        // Keep the singleton clean even if this regression breaks, so one lifecycle failure does not
        // turn every later global-session test into noise.
        if was_running {
            let _ = stop_with_data_dir(Some(tmp.path()));
        }

        assert!(
            error.as_deref().is_some_and(|message| {
                message.contains("could not durably start") && message.contains("injected initial session save failure")
            }),
            "Start must surface the durable commit failure: {error:?}"
        );
        assert!(!was_running, "a failed initial durable generation must never enter the global Couch handle");
        assert!(
            !published.running && published.reviewers.is_empty(),
            "status must publish no server or reviewer links"
        );
        assert_eq!(marker_after.as_deref(), Some(marker_before.as_slice()), "the prior Stop marker must remain exact");
        assert!(!session_exists, "the injected failure must not leave a resumable session file");
        assert!(
            std::net::TcpListener::bind(("0.0.0.0", port)).is_ok(),
            "every just-started thread and the listener must be torn down before Start returns"
        );
    }

    #[test]
    fn failed_revocation_clear_refuses_start_and_leaves_the_fresh_session_blocked() {
        let _serial = GLOBAL_SESSION_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("failed-revocation-clear.db").to_string_lossy().to_string();
        Database::open(&db_path).unwrap().initialize().unwrap();
        write_session_revocation(tmp.path()).unwrap();
        let marker_before = std::fs::read(session_revocation_path(tmp.path())).unwrap();
        let port_probe = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = port_probe.local_addr().unwrap().port();
        drop(port_probe);

        let result = start_on_port_with_session_lifecycle(
            db_path.clone(),
            vec!["Sara".into()],
            port,
            Some(tmp.path().to_path_buf()),
            save_session_snapshot,
            |_| Err("injected revocation clear failure".to_string()),
        );
        let error = result.err();
        let was_running = is_running();
        let published = status();
        let marker_after = std::fs::read(session_revocation_path(tmp.path())).ok();
        let fresh_session_exists = session_path(tmp.path()).is_file();
        let fresh_session_is_blocked = load_session(tmp.path(), &db_path).is_none();
        if was_running {
            let _ = stop_with_data_dir(Some(tmp.path()));
        }

        assert!(
            error.as_deref().is_some_and(|message| {
                message.contains("revocation marker could not be cleared")
                    && message.contains("injected revocation clear failure")
            }),
            "Start must surface the marker commit failure: {error:?}"
        );
        assert!(!was_running, "a session whose deny marker remains must never enter the global Couch handle");
        assert!(!published.running && published.reviewers.is_empty(), "status must publish no server or links");
        assert_eq!(marker_after.as_deref(), Some(marker_before.as_slice()), "the deny marker remains authoritative");
        assert!(fresh_session_exists, "the fresh encrypted generation was written before marker cleanup failed");
        assert!(fresh_session_is_blocked, "the remaining marker must make that fresh file non-resumable");
        assert!(
            std::net::TcpListener::bind(("0.0.0.0", port)).is_ok(),
            "the unpublished listener must be synchronously released"
        );
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
        assert!(sara.url.starts_with("https://"), "production Couch links must never use plaintext HTTP");
        assert!(status.certificate_fingerprint.is_some(), "the trusted desktop must expose the TLS fingerprint");
        let agent = tls_agent(tmp.path());
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
        let base = format!("https://127.0.0.1:{bound_port}");
        let unpaired = agent.get(&format!("{base}/api/queue")).set("Cookie", &cookie_for(&token)).call();
        assert!(
            matches!(unpaired, Err(ureq::Error::Status(401, _))),
            "a pairing secret must not work as a reusable data-session cookie: {unpaired:?}"
        );
        let claim = agent
            .post(&format!("{base}/api/claim"))
            .send_json(ureq::json!({ "token": token }))
            .expect("pairing secret must mint a session over TLS");
        let session_cookie = claim
            .header("Set-Cookie")
            .and_then(|value| value.split(';').next())
            .expect("pairing response sets a session cookie")
            .to_string();
        assert!(claim.header("Set-Cookie").is_some_and(|value| value.contains("Secure")));
        let url = format!("{base}/api/queue");
        let code =
            agent.get(&url).set("Cookie", &session_cookie).call().map(|r| r.status()).unwrap_or_else(|e| match e {
                ureq::Error::Status(c, _) => c,
                other => panic!("the issued link did not reach the server: {other}"),
            });
        assert_eq!(code, 200, "a paired TLS session must authenticate against the running server");

        // Two reviewers were named, so each must have a DISTINCT token — one shared credential would
        // erase the attribution the whole design rests on.
        let hemn = status.reviewers.iter().find(|r| r.name == "Hemn").expect("Hemn was issued a link");
        assert_ne!(sara.url, hemn.url, "reviewers must never share a link");

        let stopped = stop().expect("stop succeeds");
        assert!(!stopped.running && stopped.reviewers.is_empty(), "stop reports a stopped server");
        assert!(!is_running(), "and the fence agrees");
        // The token dies with the session: stopping revokes every link at once. Presenting the SAME
        // credential that worked above is what makes this an assertion about the token rather than
        // about an anonymous request - drop the cookie here and the 401 would be trivially true and
        // prove nothing.
        let after = agent.get(&url).set("Cookie", &session_cookie).call();
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
        let remembered = load_session(&data_dir, &db_path).expect("session file readable");
        assert_eq!(remembered.pairing.get("tok-sara").map(String::as_str), Some("Sara"), "the token survived");
        let state2 = Arc::new(Mutex::new(CouchState {
            reviewers: remembered.pairing,
            spot_checks: remembered.spot_checks,
            session_store: Some((data_dir, db_path)),
            ..CouchState::default()
        }));

        // The answer arrives AFTER the restart.
        let body = serde_json::json!({"heardMs": 600_000,  "id": "trap1", "action": "edit", "text": "دەقی ڕاست" });
        let (code, _, msg, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state2);
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
            let _ = h.server.unblock();
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
    fn a_restored_session_authenticates_a_real_request_after_a_restart() {
        // The end-to-end half of `a_cookie_session_survives_the_restart_that_used_to_forget_it`.
        //
        // That test proves the FILE round-trips. This one proves the WIRING: that what start_on_port
        // rebuilds from the file actually authenticates a request over HTTP. Delete `reviewers` and
        // `session_issued` from the CouchState it constructs and the round-trip test still passes
        // while every reviewer is locked out again — which is exactly the shape of the bug being
        // fixed, so the round-trip alone is not enough cover. Here the SAME cookie is presented to a
        // server built the way a restart builds one, and 200 is the only honest answer.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەقی تاقیکردنەوە")).unwrap();
        drop(db);

        // A session claimed before the restart, written the way the running server writes it.
        let issued = SystemTime::now();
        let pairing = HashMap::from([("pair-secret".to_string(), "Sara".to_string())]);
        let sessions = HashMap::from([("cookie-from-before".to_string(), ("Sara".to_string(), issued))]);
        save_session(tmp.path(), &pairing, &db_path, &HashSet::new(), &HashSet::new(), &sessions, None).unwrap();

        // THE RESTART: rebuild state from the file exactly as start_on_port does.
        let remembered = load_session(tmp.path(), &db_path).expect("session file readable");
        let names: Vec<String> = remembered.pairing.values().cloned().collect();
        let restored: HashMap<String, (String, SystemTime)> =
            remembered.sessions.into_iter().filter(|(_, (n, _))| names.contains(n)).collect();
        let state = Arc::new(Mutex::new(CouchState {
            pairing_codes: remembered.pairing,
            reviewers: restored.iter().map(|(t, (n, _))| (t.clone(), n.clone())).collect(),
            session_issued: restored.into_iter().map(|(t, (_, at))| (t, at)).collect(),
            ..CouchState::default()
        }));

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path.clone(), state, shutdown.clone())
            .expect("test server thread spawns");
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build();
        let status = |token: &str| -> u16 {
            agent
                .get(&format!("http://127.0.0.1:{port}/api/queue"))
                .set("Cookie", &cookie_for(token))
                .call()
                .map(|r| r.status())
                .unwrap_or_else(|e| match e {
                    ureq::Error::Status(c, _) => c,
                    other => panic!("transport error: {other}"),
                })
        };

        assert_eq!(
            status("cookie-from-before"),
            200,
            "the cookie the phone still holds must authenticate against a server that just restarted"
        );
        assert_eq!(status("some-other-cookie"), 401, "and an unknown cookie must still be refused");

        shutdown.store(true, Ordering::SeqCst);
        let _ = ureq::get(&format!("http://127.0.0.1:{port}/")).call();
        let _ = join.join();
    }

    #[test]
    fn a_cookie_session_survives_the_restart_that_used_to_forget_it() {
        // MEASURED 2026-08-20, and it silenced six of eight paid reviewers. The pairing tokens were
        // durable, but the COOKIE the page actually authenticates with lived only in memory, so every
        // app restart — 4 to 9 a day, 48 in the nine days measured — made the server forget every
        // session it had issued. The browser went on presenting a cookie valid for its full 24 h
        // Max-Age, got 401, and the page showed the terminal "link expired". Nothing had expired.
        //
        // The reviewer could not recover: the page strips `#t=` from the address bar after claiming,
        // so a bookmark (or the installed PWA, whose start_url is "/") carries no token to re-claim
        // with. One restart turned a working shortcut into a dead end.
        //
        // Fails without the fix: `sessions` is empty on reload, so the cookie authenticates nobody.
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let db_path = data_dir.join("library.db").to_string_lossy().to_string();
        let pairing = HashMap::from([("pair-secret".to_string(), "Alle".to_string())]);
        let issued = SystemTime::now();
        let sessions = HashMap::from([("cookie-abc".to_string(), ("Alle".to_string(), issued))]);
        save_session(data_dir, &pairing, &db_path, &HashSet::new(), &HashSet::new(), &sessions, None).unwrap();

        let back = load_session(data_dir, &db_path).expect("session file readable");
        assert_eq!(
            back.sessions.get("cookie-abc").map(|(name, _)| name.as_str()),
            Some("Alle"),
            "the cookie the browser still holds must still name its reviewer after a restart"
        );
        assert_eq!(back.pairing.get("pair-secret").map(String::as_str), Some("Alle"), "and the link too");

        // A session already past its TTL stays dead: restoring resumes, it never extends.
        let stale = SystemTime::now() - COUCH_SESSION_TTL - Duration::from_secs(60);
        let expired = HashMap::from([("cookie-old".to_string(), ("Alle".to_string(), stale))]);
        save_session(data_dir, &pairing, &db_path, &HashSet::new(), &HashSet::new(), &expired, None).unwrap();
        let back = load_session(data_dir, &db_path).expect("session file readable");
        assert!(back.sessions.is_empty(), "an expired session must not be resurrected by a restart");

        // A reviewer dropped from the roster loses their session even if the file still holds it —
        // mirrors the name filter start_on_port applies when it rehydrates.
        let names = ["Rubar".to_string()];
        save_session(data_dir, &pairing, &db_path, &HashSet::new(), &HashSet::new(), &sessions, None).unwrap();
        let back = load_session(data_dir, &db_path).expect("session file readable");
        let kept: HashMap<String, (String, SystemTime)> =
            back.sessions.into_iter().filter(|(_, (name, _))| names.contains(name)).collect();
        assert!(kept.is_empty(), "a removed reviewer's session must not survive the roster change");
    }

    #[test]
    fn durable_stop_marker_blocks_a_stale_session_and_cleanup_errors_are_not_swallowed() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();
        let db_path = data_dir.join("library.db").to_string_lossy().to_string();
        let reviewers = HashMap::from([("old-secret".to_string(), "Sara".to_string())]);
        save_session(data_dir, &reviewers, &db_path, &HashSet::new(), &HashSet::new(), &HashMap::new(), None).unwrap();
        assert!(load_session(data_dir, &db_path).is_some(), "precondition: stale credentials can resume");

        write_session_revocation(data_dir).unwrap();
        assert!(session_path(data_dir).is_file(), "the stale credential file intentionally remains");
        assert!(
            load_session(data_dir, &db_path).is_none(),
            "the revocation marker, not successful cleanup, is the authority on restart"
        );

        clear_session(data_dir).unwrap();
        std::fs::create_dir(session_path(data_dir)).unwrap();
        let error = clear_session(data_dir).expect_err("a failed session-file removal must be surfaced");
        assert!(error.contains("could not be removed"));
        std::fs::remove_dir(session_path(data_dir)).unwrap();
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
            let _ = h.server.unblock();
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
    fn an_older_session_snapshot_cannot_resurrect_a_concurrently_revoked_reviewer() {
        // The previous revoke fix made the one obvious path durable, but save sites still followed
        // this order: snapshot state -> write later. If another request took its snapshot before the
        // revoke, paused, and wrote after the revoke's newer file, it resurrected the lost phone on
        // restart. Hold that exact old snapshot in the test while revoke tries to run.
        let tmp = tempfile::tempdir().unwrap();
        let (_db, db_path) = test_db(tmp.path());
        let issued = SystemTime::now();
        let state = Arc::new(Mutex::new(CouchState {
            pairing_codes: HashMap::from([
                ("pair-sara".to_string(), "Sara".to_string()),
                ("pair-hemn".to_string(), "Hemn".to_string()),
            ]),
            reviewers: HashMap::from([
                ("session-sara".to_string(), "Sara".to_string()),
                ("session-hemn".to_string(), "Hemn".to_string()),
            ]),
            session_issued: HashMap::from([("session-sara".to_string(), issued), ("session-hemn".to_string(), issued)]),
            session_store: Some((tmp.path().to_path_buf(), db_path.clone())),
            ..CouchState::default()
        }));
        persist_session_state(&state).unwrap();

        let (snapshotted_tx, snapshotted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let old_writer_state = state.clone();
        let old_writer = std::thread::spawn(move || {
            persist_session_state_with_hook(&old_writer_state, || {
                snapshotted_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap();
        });
        snapshotted_rx.recv_timeout(Duration::from_secs(2)).expect("old save reached its paused snapshot");

        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let revoke_state = state.clone();
        let revoker = std::thread::spawn(move || {
            attempted_tx.send(()).unwrap();
            result_tx.send(revoke_in(&revoke_state, "Sara")).unwrap();
        });
        attempted_rx.recv_timeout(Duration::from_secs(2)).expect("revoke thread started");
        assert!(
            result_rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "revoke must wait behind an older in-flight persistence transaction"
        );

        release_tx.send(()).unwrap();
        old_writer.join().unwrap();
        result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("revoke completed after old save")
            .expect("revoke succeeds");
        revoker.join().unwrap();

        let remembered = load_session(tmp.path(), &db_path).expect("final session file loads");
        assert!(
            !remembered.pairing.values().any(|name| name == "Sara"),
            "the stale snapshot must never restore the revoked pairing token"
        );
        assert!(
            !remembered.sessions.values().any(|(name, _)| name == "Sara"),
            "the stale snapshot must never restore the revoked cookie session"
        );
        assert_eq!(remembered.pairing.values().filter(|name| name.as_str() == "Hemn").count(), 1);
        assert_eq!(remembered.sessions.values().filter(|(name, _)| name == "Hemn").count(), 1);
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
    fn controlled_pilot_hidden_key_budget_is_two_and_survives_refills_without_growth() {
        let dir = tempfile::tempdir().unwrap();
        crate::review_pilot::install_test_focus(dir.path(), ["test-focus"]);
        std::fs::write(
            dir.path().join(crate::review_pilot::REVIEW_PILOT_FILE),
            r#"{
              "schema_version": 1,
              "after_review_event_id": 0,
              "max_total_corpus_actions": 20,
              "reviewers": [
                {"name":"Hawzhin","max_corpus_actions":10},
                {"name":"Pavel","max_corpus_actions":10}
              ]
            }"#,
        )
        .unwrap();
        let policy = crate::review_pilot::load(dir.path()).unwrap().unwrap();
        assert_eq!(pilot_spot_check_quota(&policy, "Hawzhin").unwrap(), 2);
        assert_eq!(pilot_spot_check_plan(10, 2, 0, 0, false), (0, 2), "first batch mints exactly two");
        assert_eq!(
            pilot_spot_check_plan(10, 2, 2, 2, false),
            (2, 0),
            "a reload re-serves the same two keys and mints none"
        );
        assert_eq!(
            pilot_spot_check_plan(10, 2, 2, 0, false),
            (0, 0),
            "answered/declined keys permanently consume the bounded budget"
        );
        assert_eq!(pilot_spot_check_plan(2, 2, 1, 0, false), (0, 1), "a short final refill needs only one");
        assert_eq!(
            pilot_spot_check_plan(0, 2, 0, 0, true),
            (0, 2),
            "after the corpus cap, hidden-only catch-up still mints the two required keys"
        );
        assert_eq!(
            pilot_spot_check_plan(0, 2, 2, 2, true),
            (2, 0),
            "catch-up re-serves two unanswered keys without minting a third"
        );

        let old: SavedSession =
            serde_json::from_str(r#"{"reviewers":{},"db_path":"x","spot_checks":[],"sessions":[]}"#).unwrap();
        assert!(old.pilot_spot_checks.is_empty() && old.pilot_policy.is_none());
    }

    #[test]
    fn corpus_cap_serves_exactly_two_hidden_only_catchup_checks_and_never_a_third() {
        let dir = tempfile::tempdir().unwrap();
        crate::review_pilot::install_test_focus(dir.path(), ["pilot-work", "pilot-key-1", "pilot-key-2"]);
        let (db, db_path) = test_db(dir.path());
        db.insert_segment(&seg("pilot-work", "کاری پایلۆت")).unwrap();
        gold_seg(&db, "pilot-key-1", "هەڵەی یەک", "ڕاستی یەک");
        gold_seg(&db, "pilot-key-2", "هەڵەی دوو", "ڕاستی دوو");
        for timestamp in 1..=10 {
            db.record_review_event("pilot-work", "Hawzhin", "skip", "couch", timestamp).unwrap();
        }
        std::fs::write(
            dir.path().join(crate::review_pilot::REVIEW_PILOT_FILE),
            r#"{
              "schema_version": 1,
              "after_review_event_id": 0,
              "max_total_corpus_actions": 20,
              "reviewers": [
                {"name":"Hawzhin","max_corpus_actions":10},
                {"name":"Pavel","max_corpus_actions":10}
              ]
            }"#,
        )
        .unwrap();
        let policy = crate::review_pilot::load(dir.path()).unwrap().unwrap();
        let state = Mutex::new(CouchState {
            session_store: Some((dir.path().to_path_buf(), db_path)),
            pairing_codes: HashMap::from([
                ("hawzhin-pair".to_string(), "Hawzhin".to_string()),
                ("pavel-pair".to_string(), "Pavel".to_string()),
            ]),
            pilot_policy: Some(policy.clone()),
            ..CouchState::default()
        });

        // A stale/direct outbox knows the baseline but never fetched this pilot queue, so it has no
        // live served lease. It must not become an eleventh corpus act or mint listening evidence.
        let direct = serde_json::json!({
            "id": "pilot-work",
            "action": "accept",
            "text": "کاری پایلۆت",
            "rowVersion": db.segment_row_stamp("pilot-work").unwrap().unwrap(),
            "pilotAfterReviewEventId": policy.after_review_event_id,
            "heardMs": 1_500,
            "clipDurationMs": 1_500,
        });
        assert_eq!(api_decision(&db, direct.to_string().as_bytes(), "Hawzhin", &state).0, 409);
        assert_eq!(
            db.review_decision_progress(&build_pilot_decision_limit(&policy).unwrap()).unwrap().total_review_actions,
            10
        );

        let fetch = || {
            let (code, _, body, ..) = api_queue(&db, "Hawzhin", &state);
            assert_eq!(code, 200);
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        };
        let first = fetch();
        let first_items = first["items"].as_array().unwrap();
        assert_eq!(first_items.len(), 2, "cap catch-up serves only the two hidden checks");
        assert_eq!(lock_state(&state).pilot_spot_checks.len(), 2);
        let reload = fetch();
        assert_eq!(reload["items"].as_array().unwrap().len(), 2, "reload re-serves, it does not mint replacements");
        assert_eq!(lock_state(&state).pilot_spot_checks.len(), 2);

        for item in reload["items"].as_array().unwrap() {
            let id = item["id"].as_str().unwrap();
            let corrected = if id == "pilot-key-1" { "ڕاستی یەک" } else { "ڕاستی دوو" };
            let body = serde_json::json!({
                "id": id,
                "action": "edit",
                "text": corrected,
                "rowVersion": item["rowVersion"],
                "pilotAfterReviewEventId": item["pilotAfterReviewEventId"],
                "heardMs": 1_500,
                "clipDurationMs": 1_500,
            });
            assert_eq!(api_decision(&db, body.to_string().as_bytes(), "Hawzhin", &state).0, 200);
        }
        assert!(fetch()["items"].as_array().unwrap().is_empty(), "2/2 durable results end catch-up");
        assert_eq!(lock_state(&state).pilot_spot_checks.len(), 2, "a third key is never authorized");
    }

    #[test]
    fn pilot_never_returns_a_batch_until_its_distinct_key_budget_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        crate::review_pilot::install_test_focus(dir.path(), ["persist-work", "persist-key-1", "persist-key-2"]);
        let (db, db_path) = test_db(dir.path());
        db.insert_segment(&seg("persist-work", "کاری پایلۆت")).unwrap();
        gold_seg(&db, "persist-key-1", "هەڵەی یەک", "ڕاستی یەک");
        gold_seg(&db, "persist-key-2", "هەڵەی دوو", "ڕاستی دوو");
        for timestamp in 1..=10 {
            db.record_review_event("persist-work", "Hawzhin", "skip", "couch", timestamp).unwrap();
        }
        std::fs::write(
            dir.path().join(crate::review_pilot::REVIEW_PILOT_FILE),
            r#"{
              "schema_version": 1,
              "after_review_event_id": 0,
              "max_total_corpus_actions": 20,
              "reviewers": [
                {"name":"Hawzhin","max_corpus_actions":10},
                {"name":"Pavel","max_corpus_actions":10}
              ]
            }"#,
        )
        .unwrap();
        let policy = crate::review_pilot::load(dir.path()).unwrap().unwrap();
        let make_state = |fail_session_persist| {
            Mutex::new(CouchState {
                session_store: Some((dir.path().to_path_buf(), db_path.clone())),
                pairing_codes: HashMap::from([
                    ("hawzhin-pair".to_string(), "Hawzhin".to_string()),
                    ("pavel-pair".to_string(), "Pavel".to_string()),
                ]),
                pilot_policy: Some(policy.clone()),
                fail_session_persist,
                ..CouchState::default()
            })
        };

        // The in-memory planner may select two, but a failed durable save returns no batch and rolls
        // its provisional registration back. A test hook is used because Windows can rename a target
        // directory aside and make the old filesystem-based "failure" unexpectedly succeed.
        let failed_state = make_state(true);
        let (code, ..) = api_queue(&db, "Hawzhin", &failed_state);
        assert_eq!(code, 503);
        assert!(lock_state(&failed_state).leases.is_empty(), "failed durability releases all work leases");
        assert!(
            lock_state(&failed_state).pilot_spot_checks.is_empty(),
            "a same-process retry must not inherit keys that were never durably authorized"
        );
        assert!(!session_path(dir.path()).exists());
        assert!(db.spot_check_report().unwrap().is_empty(), "an unreturned batch cannot create paid QC results");

        // Simulate a process restart. Since the failed batch was never returned, starting from the
        // last durable (empty) key budget is safe; the next successful save may now return exactly two.
        drop(failed_state);
        let restarted = make_state(false);
        let (code, _, body, ..) = api_queue(&db, "Hawzhin", &restarted);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["items"].as_array().unwrap().len(), 2);
        assert_eq!(lock_state(&restarted).pilot_spot_checks.len(), 2);
        assert!(session_path(dir.path()).is_file(), "the returned two-key budget is durable before response");
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
            let body = serde_json::json!({"heardMs": 600_000,  "id": "d1", "action": action, "text": text });
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
        // rationale/evidence/agreement_score) + insert_segment (a 17-col subset that OMITS every jury/
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
        let body = serde_json::json!({"heardMs": 600_000,  "id": "esc1", "action": "accept", "text": "دەق یەک" });
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "the accept decision must succeed");
        assert!(db.get_segment_by_id("esc1").unwrap().unwrap().verified, "accept verifies the clip");

        let (code, ..) = api_undo(&db, "Sara", &state);
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
            (serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": ""}), 400u16),
            (
                serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "accept", "text": "[Pending WSL 7B ASR]"}),
                400,
            ),
            (serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "explode", "text": "x"}), 400),
            (serde_json::json!({"heardMs": 600_000, "id": "../etc", "action": "accept", "text": "x"}), 400),
            (serde_json::json!({"heardMs": 600_000, "id": "missing", "action": "accept", "text": "x"}), 404),
        ] {
            let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, expect, "body: {body}");
        }
        assert!(lock_state(&state).undo.values().all(Vec::is_empty), "no failed decision may leave an undo entry");

        // Both brackets are required TOGETHER. With `||` in that guard, any transcript merely ENDING
        // in ']' — or merely STARTING with '[' — would be refused as a placeholder, silently blocking
        // legitimate text. Only "[...]" on both sides is a placeholder, so this half-bracketed text
        // must be ACCEPTED. (A rejects-only loop could never have caught this: the bug refuses MORE.)
        db.insert_segment(&seg("s2", "دەق")).unwrap();
        let half =
            serde_json::json!({"heardMs": 600_000, "id": "s2", "action": "accept", "text": "[ناتەواو"}).to_string();
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
            let body =
                serde_json::json!({"heardMs": 600_000,  "id": format!("u{n:03}"), "action": "accept", "text": "دەق" });
            let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 200, "decision u{n:03} must land");
        }

        let depth = lock_state(&state).undo.get("Sara").map(|s| s.len()).unwrap_or(0);
        assert_eq!(depth, UNDO_DEPTH, "the stack must stop growing at the cap, not follow the session");

        // The MOST RECENT decision is still undoable — the cap trimmed the far end.
        let (code, _, body, ..) = api_undo(&db, "Sara", &state);
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
    fn a_desktop_decision_is_never_silently_overwritten_by_the_phone() {
        // The owner reviews on BOTH surfaces. A clip the phone is holding can be decided at the desktop
        // in the meantime, and the phone's submit then arrives against a row that already carries
        // somebody's judgement. Silently overwriting it is the destruction leases exist to prevent,
        // just arriving from the other direction — and the desktop path attributes differently
        // (annotator None), so this leans on the late-submit guard rather than the collision guard.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("dk1", "دەقی خاو")).unwrap();
        let state = state();

        // The phone leases it as part of a batch.
        let held = queue_ids(&db, "Hawzhin", &state);
        assert!(held.contains(&"dk1".to_string()), "the phone must be holding the clip");

        // The DESKTOP decides it first, the way the desktop review does: no phone reviewer name.
        crate::jury::record_human_decision_by(&db, "dk1", "edit", Some("ڕاستکراوەی دێسکتۆپ"), None, None).unwrap();
        let mut row = db.get_segment_by_id("dk1").unwrap().unwrap();
        row.annotated_transcript = Some("ڕاستکراوەی دێسکتۆپ".into());
        row.verified = true;
        db.insert_segment(&row).unwrap();

        // Now the phone's submit lands, minutes late, carrying a different correction.
        let body =
            serde_json::json!({"heardMs": 600_000,  "id": "dk1", "action": "edit", "text": "ڕاستکراوەی مۆبایل" });
        let (code, _, msg, ..) = api_decision(&db, body.to_string().as_bytes(), "Hawzhin", &state);
        assert_eq!(code, 409, "a late phone submit must be refused, not written over the desktop's work");
        assert!(
            String::from_utf8_lossy(&msg).contains("desktop"),
            "the refusal must name where the existing decision came from: {msg:?}"
        );

        let after = db.get_segment_by_id("dk1").unwrap().unwrap();
        assert_eq!(
            after.annotated_transcript.as_deref(),
            Some("ڕاستکراوەی دێسکتۆپ"),
            "the desktop correction must survive intact"
        );
        assert!(after.verified);
    }

    #[test]
    fn reloading_mid_batch_returns_the_remainder_and_never_a_decided_clip() {
        // A phone reload mid-batch is the single most common thing a reviewer does — a stutter, a
        // rotation, a background/foreground cycle, or simply pulling to refresh — and nothing covered
        // it. Two things could go wrong and both would be quiet: getting a FRESH batch (abandoning the
        // in-progress work the leases were meant to protect) or getting back a clip already decided
        // (asking the reviewer to judge the same audio twice, which corrupts the honest count of what
        // they did).
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        const CLIPS: usize = 60;
        for n in 0..CLIPS {
            db.insert_segment(&seg(&format!("m{n:03}"), "دەقی سەرەتایی")).unwrap();
        }
        drop(db);

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: [("tok".to_string(), "Hawzhin".to_string())].into_iter().collect(),
            ..CouchState::default()
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path.clone(), state.clone(), shutdown.clone()).unwrap();
        let base = format!("http://127.0.0.1:{port}");
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
        let fetch = || -> Vec<serde_json::Value> {
            let q: serde_json::Value = agent
                .get(&format!("{base}/api/queue"))
                .set("Cookie", &cookie_for("tok"))
                .call()
                .unwrap()
                .into_json()
                .unwrap();
            q["items"].as_array().unwrap().clone()
        };

        let first = fetch();
        assert!(first.len() >= 10, "need a real batch, got {}", first.len());
        // Decide the first five, then "reload" — a bare re-fetch, which is exactly what load() does.
        for item in first.iter().take(5) {
            let id = item["id"].as_str().unwrap();
            let body = serde_json::json!({
                "operationId": uuid::Uuid::new_v4().to_string(),
                "heardMs": 600_000,
                "id": id,
                "action": "accept",
                "text": "دەقی سەرەتایی",
                "rowVersion": item["rowVersion"],
            })
            .to_string();
            let status = agent
                .post(&format!("{base}/api/decision"))
                .set("Cookie", &cookie_for("tok"))
                .set("content-type", "application/json")
                .send_string(&body)
                .map(|r| r.status())
                .unwrap_or(0);
            assert_eq!(status, 200, "{id} must land before the reload");
        }
        let after = fetch();

        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock();
        let _ = join.join();

        // NO decided clip may come back. This is the assertion that protects the reviewer from being
        // asked to judge the same audio twice.
        let after_ids: HashSet<&str> = after.iter().filter_map(|item| item["id"].as_str()).collect();
        for item in first.iter().take(5) {
            let id = item["id"].as_str().unwrap();
            assert!(!after_ids.contains(id), "{id} was decided, yet the reload served it again");
        }
        // The UNDECIDED remainder of the same batch must still be theirs — the reload must not abandon
        // in-progress work for a fresh batch, which is the whole reason api_queue preserves own leases.
        let kept =
            first.iter().skip(5).filter_map(|item| item["id"].as_str()).filter(|id| after_ids.contains(id)).count();
        assert_eq!(
            kept,
            first.len() - 5,
            "a reload must hand back the rest of the in-progress batch, kept {kept} of {}",
            first.len() - 5
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
        let operation_ids = Mutex::new(HashMap::<String, String>::new());

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
        let submit = |port: u16, id: &str, row_version: &serde_json::Value| -> u16 {
            let operation_id = operation_ids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(id.to_string())
                .or_insert_with(|| uuid::Uuid::new_v4().to_string())
                .clone();
            let body = serde_json::json!({
                "operationId": operation_id,
                "heardMs": 600_000,
                "id": id,
                "action": "accept",
                "text": "دەقی سەرەتایی",
                "rowVersion": row_version,
            })
            .to_string();
            match agent
                .post(&format!("http://127.0.0.1:{port}/api/decision"))
                .set("Cookie", &cookie_for("tok"))
                .set("content-type", "application/json")
                .send_string(&body)
            {
                Ok(r) => r.status(),
                Err(ureq::Error::Status(c, _)) => c,
                Err(e) => panic!("transport: {e}"),
            }
        };

        let (server, port, shutdown, join) = boot(db_path.clone());
        let queue: serde_json::Value = agent
            .get(&format!("http://127.0.0.1:{port}/api/queue"))
            .set("Cookie", &cookie_for("tok"))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        let held: Vec<(String, serde_json::Value)> = queue["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (i["id"].as_str().unwrap().to_string(), i["rowVersion"].clone()))
            .collect();
        assert!(held.len() >= 10, "need a real batch to interrupt, got {}", held.len());
        for (id, row_version) in held.iter().take(5) {
            assert_eq!(submit(port, id, row_version), 200, "pre-restart decision {id} must land");
            decided.push(id.clone());
        }

        // THE RESTART, mid-batch, with the phone still holding the rest of its queue.
        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock();
        let _ = join.join();
        drop(server);

        let (server2, port2, shutdown2, join2) = boot(db_path.clone());
        // The phone does NOT refetch — it works through the queue it already had, which is exactly what
        // a page mid-batch does. Every remaining clip must still be accepted.
        for (id, row_version) in held.iter().skip(5) {
            let code = submit(port2, id, row_version);
            assert_eq!(code, 200, "post-restart decision {id} must still land, got {code}");
            decided.push(id.clone());
        }
        // And a REPLAY of a pre-restart decision must be recognised as already-done, not recorded twice
        // — the retry guard reads the row, not process memory, so it has to survive the restart.
        let replay = submit(port2, &decided[0], &held[0].1);
        assert_eq!(replay, 200, "an outbox replay across a restart must be answered as already-done");

        shutdown2.store(true, Ordering::SeqCst);
        let _ = server2.unblock();
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
                match agent.get(&format!("{base}/api/queue")).set("Cookie", &cookie_for("tok-solo")).call() {
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
                let body = serde_json::json!({
                    "operationId": uuid::Uuid::new_v4().to_string(),
                    "heardMs": 600_000,
                    "id": id,
                    "action": "accept",
                    "text": item["text"],
                    "rowVersion": item["rowVersion"],
                })
                .to_string();
                // THROTTLING IS PART OF THE TEST, not an obstacle to it. A machine-speed drain burns
                // the couch limiter's 60-token burst and then rides its 120/second refill, which is
                // exactly what a phone in a reload loop does — and the first run of this soak hit 429
                // at clip 73. A 429 means LATER, never NO, so the correct behaviour (client and test
                // alike) is to wait and re-submit the same decision, and the drain must still finish.
                let mut code = 0;
                for attempt in 0..40 {
                    code = match agent
                        .post(&format!("{base}/api/decision"))
                        .set("Cookie", &cookie_for("tok-solo"))
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
                    std::thread::sleep(Duration::from_millis(600)); // refills the full 60-token burst
                }
                assert_eq!(code, 200, "every submit in a solo session must eventually land; {id} got {code}");
                served.push(id);
            }
        }

        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock();
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
                            match agent.get(&format!("{base}/api/queue")).set("Cookie", &cookie_for(&token)).call() {
                                Ok(r) => r.into_json().unwrap(),
                                Err(_) => continue,
                            };
                        let items = queue["items"].as_array().cloned().unwrap_or_default();
                        if items.is_empty() {
                            break;
                        }
                        for item in items {
                            let body = serde_json::json!({
                                "operationId": uuid::Uuid::new_v4().to_string(),
                                "heardMs": 600_000,
                                "id": item["id"],
                                "action": "edit",
                                "text": format!("{} ✓", item["text"].as_str().unwrap_or("x")),
                                "rowVersion": item["rowVersion"],
                            })
                            .to_string();
                            // Returns (ok, throttled) rather than ureq's Result: clippy flags a
                            // closure whose Err variant is 272 bytes, and the two flags are all this
                            // loop needs from the response.
                            let post = || match agent
                                .post(&format!("{base}/api/decision"))
                                .set("Cookie", &cookie_for(&token))
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
        let _ = server.unblock();
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
        // across three reviewers so none of them approaches the 120/second budget. That is a real
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
            session_issued: HashMap::from([
                ("saratoken123".to_string(), SystemTime::now()),
                ("hemntoken456".to_string(), SystemTime::now()),
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
            // This test runs alongside CPU/SQLite-heavy Couch soaks in the full suite. Ten seconds
            // produced a false transport timeout while the same route passed in isolation; thirty
            // still fails a wedged server quickly without grading scheduler contention as routing.
            .timeout(std::time::Duration::from_secs(30))
            .build();

        let base = format!("http://127.0.0.1:{port}");
        // Token is passed as the COOKIE, never in the URL - the server accepts nothing else.
        let status_of_url = |url: String, token: Option<&str>| -> u16 {
            let mut req = agent.get(&url);
            if let Some(t) = token {
                req = req.set("Cookie", &cookie_for(t));
            }
            req.call().map(|r| r.status()).unwrap_or_else(|e| match e {
                ureq::Error::Status(c, _) => c,
                other => panic!("{url} transport error (timeout/hang?): {other}"),
            })
        };
        // No token -> 401 on every DATA route. The page shell itself is deliberately public since
        // phase 2 (fragment links): it embeds nothing, and gating it would force the token back into
        // a server-visible part of the URL. The dedicated shell/claim test pins that side.
        for path in ["/api/queue", "/api/audio/s1"] {
            assert_eq!(status_of_url(format!("{base}{path}"), None), 401, "{path} must be token-gated");
        }
        assert_eq!(status_of_url(format!("{base}/"), None), 200, "the empty shell is public by design");
        // An unrecognised token has no reviewer, so it has no way in either.
        assert_eq!(
            status_of_url(format!("{base}/api/queue"), Some("notatoken")),
            401,
            "an unknown token resolves to nobody"
        );

        // With a token: the page serves, and the queue identifies WHICH reviewer it answered.
        assert_eq!(status_of_url(format!("{base}/"), Some("saratoken123")), 200);
        let sara: serde_json::Value = agent
            .get(&format!("{base}/api/queue"))
            .set("Cookie", &cookie_for("saratoken123"))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(sara["reviewer"], "Sara", "the token, not the request, decides the identity");
        let sara_ids: Vec<&str> = sara["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(!sara_ids.is_empty());

        let hemn: serde_json::Value = agent
            .get(&format!("{base}/api/queue"))
            .set("Cookie", &cookie_for("hemntoken456"))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(hemn["reviewer"], "Hemn");
        let hemn_ids: Vec<&str> = hemn["items"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        for id in &sara_ids {
            assert!(!hemn_ids.contains(id), "clip {id} was served to both reviewers over real HTTP");
        }

        // A phone decision over real HTTP verifies the row in the real db file, under the right name.
        let mine = sara_ids[0];
        let mine_item = sara["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == mine)
            .expect("the selected queue item still carries its rowVersion");
        let resp = agent
            .post(&format!("{base}/api/decision"))
            .set("Cookie", &cookie_for("saratoken123"))
            .send_string(
                &serde_json::json!({
                    "operationId": uuid::Uuid::new_v4().to_string(),
                    "heardMs": 600_000,
                    "id": mine,
                    "action": "accept",
                    "text": "دەقی تاقیکردنەوە",
                    "rowVersion": mine_item["rowVersion"],
                })
                .to_string(),
            )
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
            agent
                .post(&format!("{base}{path}"))
                .set("Cookie", &cookie_for("saratoken123"))
                .send_string(body)
                .map(|r| r.status())
                .unwrap_or_else(|e| match e {
                    ureq::Error::Status(c, _) => c,
                    other => panic!("{path} transport error: {other}"),
                })
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
        let page = agent.get(&format!("{base}/")).set("Cookie", &cookie_for("saratoken123")).call().unwrap();
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
            status_of_url(format!("{base}/api/audio/..%2Fetc"), Some("saratoken123")),
            400,
            "/api/audio is wired — this 400 comes from its id validation, not from the router"
        );
        // And an unrelated GET must still reach the router's 404, not be swallowed by a widened guard.
        assert_eq!(status_of_url(format!("{base}/nonsense"), Some("saratoken123")), 404, "unknown paths stay 404");
        // And the body cap rejects an over-sized payload with ITS OWN message, so a truncating `take`
        // (which would leave a short body to fail later as bad json) is distinguishable from a refusal.
        let huge = "x".repeat(MAX_BODY_BYTES + 4096);
        let resp =
            agent.post(&format!("{base}/api/decision")).set("Cookie", &cookie_for("saratoken123")).send_string(&huge);
        match resp {
            Err(ureq::Error::Status(413, r)) => assert!(
                r.into_string().unwrap_or_default().contains("body too large"),
                "an over-cap body must be REFUSED, not silently truncated into a parse error"
            ),
            other => panic!("expected 413 body-too-large, got {other:?}"),
        }

        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock(); // the fix under test: unblock() (not a bare connect) ends the accept loops
        for join in joins {
            join.join().expect("every server thread joins cleanly after unblock()");
        }
    }

    // ── R1: transport ─────────────────────────────────────────────────────────

    #[test]
    fn a_byte_range_is_parsed_by_the_spec_or_refused_outright() {
        // The whole point of answering 206 is that the client can trust the offsets. Every form below
        // is one a real media stack sends, and every off-by-one here would hand back the wrong audio
        // while claiming success — silent corruption, not a visible failure.
        assert_eq!(parse_range("bytes=0-1", 100), Some((0, 1)), "Safari's opening 2-byte probe");
        assert_eq!(parse_range("bytes=0-99", 100), Some((0, 99)), "the whole body, explicitly");
        assert_eq!(parse_range("bytes=50-", 100), Some((50, 99)), "open-ended runs to the last byte");
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)), "suffix range is the LAST n bytes");
        assert_eq!(parse_range("bytes=-500", 100), Some((0, 99)), "a suffix longer than the body is the body");
        assert_eq!(parse_range("bytes=0-500", 100), Some((0, 99)), "an over-long end CLAMPS, it does not fail");
        assert_eq!(parse_range("bytes=99-99", 100), Some((99, 99)), "the final byte alone is satisfiable");
        assert_eq!(parse_range(" bytes=2-4 ", 100), Some((2, 4)), "whitespace tolerated");
        // Unsatisfiable or unsupported: None, so the caller answers 416 rather than guessing. A 200
        // full body here is the dangerous answer — the client believes its range was honoured.
        assert_eq!(parse_range("bytes=100-200", 100), None, "start at or past the end is unsatisfiable");
        assert_eq!(parse_range("bytes=5-3", 100), None, "reversed range");
        assert_eq!(parse_range("bytes=0-1", 0), None, "nothing is satisfiable in an empty body");
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None, "multi-range is not supported, and says so");
        assert_eq!(parse_range("items=0-1", 100), None, "only the bytes unit exists");
        assert_eq!(parse_range("bytes=abc-def", 100), None, "garbage");
        assert_eq!(parse_range("bytes=-", 100), None, "no numbers at all");
        // RFC 9110: a suffix-length of zero is unsatisfiable. Found by self-audit — a `.max(1)` here
        // had been silently turning "give me the last 0 bytes" into "give me the last byte".
        assert_eq!(parse_range("bytes=-0", 100), None, "a zero-length suffix is unsatisfiable");
    }

    #[test]
    fn the_audio_etag_tracks_everything_that_changes_the_bytes() {
        // The ETag is derived from the fingerprint rather than from the bytes, which is what lets a 304
        // skip the decode entirely. That trade is only sound if the fingerprint moves whenever the
        // bytes would: a stale ETag could validate OLD audio for a clip that was re-aligned even
        // though every cache reuse now crosses live authorization.
        let base = seg("s1", "دەق");
        let fp = audio_fingerprint(&base);
        assert_eq!(fp, audio_fingerprint(&seg("s1", "دەق")), "same segment, same fingerprint");
        let mut other_text = base.clone();
        other_text.raw_transcript = "دەقێکی تر".into();
        assert_eq!(audio_fingerprint(&other_text), fp, "the TRANSCRIPT does not change the audio");
        for (label, mutate) in [
            ("id", (|s: &mut SpeechSegment| s.id = "s2".into()) as fn(&mut SpeechSegment)),
            ("audio_path", |s: &mut SpeechSegment| s.audio_path = "/audio/b.wav".into()),
            ("alignment", |s: &mut SpeechSegment| s.alignment_json = Some(r#"{"start":1.0}"#.into())),
            ("duration", |s: &mut SpeechSegment| s.duration_ms = 9000),
        ] {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(audio_fingerprint(&changed), fp, "changing {label} must invalidate the ETag");
        }
    }

    #[test]
    fn a_matching_etag_is_answered_without_ever_touching_the_audio() {
        // A 304 that decoded first would be pointless, and this proves it does not: the segment's
        // fixture is only a four-byte RIFF stub, so ANY materialisation attempt fails with 500.
        // Reaching 304 is therefore proof that authorization ran and the conditional then
        // short-circuited above the decoder.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let s = seg("s1", "دەق");
        db.insert_segment(&s).unwrap();
        let etag = format!("\"{:016x}\"", audio_fingerprint(&s));
        let state = state();
        {
            let mut guard = lock_state(&state);
            guard.leases.insert("s1".into(), ("Sara".into(), Instant::now()));
            guard.served_work.insert(("s1".into(), "Sara".into()));
        }

        let (code, _, body, headers) = api_audio(&db, "s1", "Sara", &state, None, Some(&etag));
        assert_eq!(code, 304, "a matching If-None-Match must be answered 304, not re-sent");
        assert!(body.is_empty(), "a 304 carries no body");
        let hv = |n: &str| headers.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone());
        assert_eq!(hv("ETag").as_deref(), Some(etag.as_str()), "the 304 restates the validator");
        assert_eq!(
            hv("Cache-Control").as_deref(),
            Some("private, no-cache"),
            "every cache reuse must cross live reviewer/object authorization"
        );
        // `private` is not a nicety here: this is biometric audio (GDPR Art. 9) and must never be
        // retained by a shared/intermediary cache.
        assert!(hv("Cache-Control").unwrap().contains("private"));
        assert_eq!(hv("Vary").as_deref(), Some("Cookie"), "one reviewer's cache entry cannot key another's response");
        assert_eq!(hv("Accept-Ranges").as_deref(), Some("bytes"));

        // Wildcard and weak forms are the same answer; a DIFFERENT validator is not.
        assert_eq!(api_audio(&db, "s1", "Sara", &state, None, Some("*")).0, 304, "* matches anything");
        assert_eq!(api_audio(&db, "s1", "Sara", &state, None, Some(&format!("W/{etag}"))).0, 304, "weak comparison");
        assert_eq!(
            api_audio(&db, "s1", "Sara", &state, None, Some(&format!("\"nope\", {etag}"))).0,
            304,
            "multi-value"
        );
        assert_eq!(
            api_audio(&db, "s1", "Sara", &state, None, Some("\"stale\"")).0,
            500,
            "a NON-matching validator must fall through to materialisation (500 here only because \
             the fixture has no real audio file — the point is that it tried)"
        );
    }

    /// A real, decodable 16 kHz mono WAV on disk, so the audio route can be driven end to end.
    /// symphonia decodes this in-process — no ffmpeg, nothing environment-dependent.
    fn write_test_wav(path: &std::path::Path, samples: usize) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for n in 0..samples {
            // A ramp rather than silence: a wrong byte offset shows up as a wrong VALUE, so the
            // range assertions below are checking real content and not just a length.
            w.write_sample(((n % 1000) as i16).wrapping_mul(30)).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn audio_requires_queue_delivery_and_the_exact_reviewers_current_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let wav = tmp.path().join("object-auth.wav");
        write_test_wav(&wav, 1_600);
        for id in ["audio-own", "audio-other", "audio-known", "audio-forged", "audio-expired"] {
            let mut segment = seg(id, "دەقی کار");
            segment.audio_path = wav.to_string_lossy().into_owned();
            db.insert_segment(&segment).unwrap();
        }
        let state = state();
        let expired = Instant::now().checked_sub(LEASE_TTL).unwrap();
        {
            let mut guard = lock_state(&state);
            guard.leases.insert("audio-own".into(), ("Sara".into(), Instant::now()));
            guard.served_work.insert(("audio-own".into(), "Sara".into()));
            guard.leases.insert("audio-other".into(), ("Hemn".into(), Instant::now()));
            guard.served_work.insert(("audio-other".into(), "Hemn".into()));
            // A lease without the queue-issued receipt models the old `/api/renew` bypass.
            guard.leases.insert("audio-forged".into(), ("Sara".into(), Instant::now()));
            guard.leases.insert("audio-expired".into(), ("Sara".into(), expired));
            guard.served_work.insert(("audio-expired".into(), "Sara".into()));
        }

        assert_eq!(api_audio(&db, "audio-own", "Sara", &state, None, None).0, 200);
        assert_eq!(
            api_audio(&db, "audio-other", "Sara", &state, None, None).0,
            403,
            "another reviewer's delivered object must remain unreadable"
        );
        assert_eq!(api_audio(&db, "audio-other", "Hemn", &state, None, None).0, 200);
        assert_eq!(
            api_audio(&db, "audio-known", "Sara", &state, None, None).0,
            403,
            "knowing a real segment id is not authorization"
        );
        assert_eq!(
            api_audio(&db, "audio-forged", "Sara", &state, None, None).0,
            403,
            "a lease that was not issued with a successful queue response is not authorization"
        );
        assert_eq!(
            api_renew(serde_json::json!({"id":"audio-known"}).to_string().as_bytes(), "Sara", &state,).0,
            409,
            "renew must not turn an arbitrary known id into an audio grant"
        );

        assert_eq!(
            api_audio(&db, "audio-expired", "Sara", &state, None, None).0,
            403,
            "an expired lease is stale even when the clip was once delivered"
        );
        assert_eq!(
            api_renew(serde_json::json!({"id":"audio-expired"}).to_string().as_bytes(), "Sara", &state,).0,
            200,
            "the reviewer who was actually served the still-pending item may reclaim it"
        );
        assert_eq!(api_audio(&db, "audio-expired", "Sara", &state, None, None).0, 200);
    }

    #[test]
    fn audio_rechecks_hot_focus_and_completion_before_honouring_an_etag() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let wav = tmp.path().join("policy-auth.wav");
        write_test_wav(&wav, 1_600);
        let mut segment = seg("audio-policy", "دەقی کار");
        segment.audio_path = wav.to_string_lossy().into_owned();
        db.insert_segment(&segment).unwrap();
        let etag = format!("\"{:016x}\"", audio_fingerprint(&segment));
        std::fs::write(
            tmp.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
            r#"{"name":"current","segment_ids":["audio-policy"]}"#,
        )
        .unwrap();
        let state = Mutex::new(CouchState {
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            leases: HashMap::from([("audio-policy".into(), ("Sara".into(), Instant::now()))]),
            served_work: HashSet::from([("audio-policy".into(), "Sara".into())]),
            ..CouchState::default()
        });

        assert_eq!(
            api_audio(&db, "audio-policy", "Sara", &state, None, Some(&etag)).0,
            304,
            "an assigned, in-focus conditional replay remains bandwidth-free"
        );
        std::fs::write(tmp.path().join(crate::voice_focus::VOICE_FOCUS_FILE), "{ broken").unwrap();
        assert_eq!(
            api_audio(&db, "audio-policy", "Sara", &state, None, Some(&etag)).0,
            503,
            "a present-but-broken focus must fail closed before cache validation"
        );
        std::fs::write(
            tmp.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
            r#"{"name":"current","segment_ids":["some-other-id"]}"#,
        )
        .unwrap();
        assert_eq!(
            api_audio(&db, "audio-policy", "Sara", &state, None, Some(&etag)).0,
            403,
            "a hot focus change must beat a matching browser validator"
        );

        std::fs::write(
            tmp.path().join(crate::voice_focus::VOICE_FOCUS_FILE),
            r#"{"name":"current","segment_ids":["audio-policy"]}"#,
        )
        .unwrap();
        {
            let mut guard = lock_state(&state);
            guard.leases.insert("audio-policy".into(), ("Sara".into(), Instant::now()));
            guard.served_work.insert(("audio-policy".into(), "Sara".into()));
        }
        assert!(db.update_verified("audio-policy", true).unwrap());
        let denied = api_audio(&db, "audio-policy", "Sara", &state, None, Some(&etag));
        assert_eq!(denied.0, 403, "completion revokes object access before a matching ETag can produce 304");
        let denied_header = |name: &str| denied.3.iter().find(|(key, _)| *key == name).map(|(_, value)| value.as_str());
        assert_eq!(
            denied_header("Cache-Control"),
            Some("private, no-store"),
            "a denial must never become a reusable cross-session cache entry"
        );
        assert_eq!(denied_header("Vary"), Some("Cookie"));
    }

    #[test]
    fn pilot_cap_blocks_work_audio_but_preserves_only_outstanding_pilot_checks() {
        let tmp = tempfile::tempdir().unwrap();
        crate::review_pilot::install_test_focus(tmp.path(), ["audio-pilot-work", "audio-pilot-key", "audio-old-key"]);
        let (db, db_path) = test_db(tmp.path());
        let wav = tmp.path().join("pilot-auth.wav");
        write_test_wav(&wav, 1_600);

        let mut work = seg("audio-pilot-work", "کاری پایلۆت");
        work.audio_path = wav.to_string_lossy().into_owned();
        db.insert_segment(&work).unwrap();
        for (id, wrong, answer) in
            [("audio-pilot-key", "هەڵەی پایلۆت", "ڕاستی پایلۆت"), ("audio-old-key", "هەڵەی کۆن", "ڕاستی کۆن")]
        {
            let mut key = seg(id, wrong);
            key.audio_path = wav.to_string_lossy().into_owned();
            key.verified = true;
            key.is_gold = true;
            key.human_decision = Some("edit".into());
            key.verdict = Some("human_edit".into());
            key.verdict_transcript = Some(answer.into());
            db.insert_segment_full(&key).unwrap();
        }
        for timestamp in 1..=10 {
            db.record_review_event("audio-pilot-work", "Hawzhin", "skip", "couch", timestamp).unwrap();
        }
        std::fs::write(
            tmp.path().join(crate::review_pilot::REVIEW_PILOT_FILE),
            r#"{
              "schema_version": 1,
              "after_review_event_id": 0,
              "max_total_corpus_actions": 20,
              "reviewers": [
                {"name":"Hawzhin","max_corpus_actions":10},
                {"name":"Pavel","max_corpus_actions":10}
              ]
            }"#,
        )
        .unwrap();
        let policy = crate::review_pilot::load(tmp.path()).unwrap().unwrap();
        let state = Mutex::new(CouchState {
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            pairing_codes: HashMap::from([
                ("hawzhin-pair".into(), "Hawzhin".into()),
                ("pavel-pair".into(), "Pavel".into()),
            ]),
            pilot_policy: Some(policy),
            leases: HashMap::from([("audio-pilot-work".into(), ("Hawzhin".into(), Instant::now()))]),
            served_work: HashSet::from([("audio-pilot-work".into(), "Hawzhin".into())]),
            spot_checks: HashSet::from([
                ("audio-pilot-key".into(), "Hawzhin".into()),
                ("audio-old-key".into(), "Hawzhin".into()),
            ]),
            pilot_spot_checks: HashSet::from([("audio-pilot-key".into(), "Hawzhin".into())]),
            ..CouchState::default()
        });

        assert_eq!(
            api_audio(&db, "audio-pilot-work", "Hawzhin", &state, None, None).0,
            403,
            "the eleventh corpus action cannot be enabled by fetching its audio"
        );
        assert_eq!(
            api_audio(&db, "audio-old-key", "Hawzhin", &state, None, None).0,
            403,
            "a hidden key from outside the bound pilot is not a pilot audio grant"
        );
        assert_eq!(
            api_audio(&db, "audio-pilot-key", "Hawzhin", &state, None, None).0,
            200,
            "the outstanding hidden-only catch-up check remains playable at the corpus cap"
        );
        db.record_spot_check("audio-pilot-key", "Hawzhin", "edit", "ڕاستی پایلۆت", "ڕاستی پایلۆت").unwrap();
        assert_eq!(
            api_audio(&db, "audio-pilot-key", "Hawzhin", &state, None, None).0,
            403,
            "a completed hidden check cannot be replay-fetched"
        );
    }

    #[test]
    fn a_phone_asking_for_part_of_a_clip_gets_exactly_that_part_over_real_http() {
        // The wire contract, driven through tiny_http rather than asserted about it. Before R1 the
        // route answered every request with an unconditional 200 full body carrying only a
        // Content-Type: a Range header was ignored (so a client that trusts 206 reads wrong offsets),
        // HEAD fell through to 404 (mobile stacks probe with it), and nothing was cacheable, so every
        // replay re-downloaded 300-500 KB over cellular.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let wav = tmp.path().join("clip.wav");
        write_test_wav(&wav, 16_000); // one second
        let mut s = seg("s1", "دەق");
        s.audio_path = wav.to_string_lossy().to_string();
        db.insert_segment(&s).unwrap();
        drop(db);

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(CouchState {
            reviewers: [("tok".to_string(), "AudioRangeReviewer".to_string())].into_iter().collect(),
            ..CouchState::default()
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path, state.clone(), shutdown.clone()).unwrap();
        let url = format!("http://127.0.0.1:{port}/api/audio/s1");
        let jar = cookie_for("tok");
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();

        // Object authorization is queue-issued, not inferred from knowing an id. Drive the same
        // successful delivery boundary the phone crosses before it asks the media element to play.
        let queue_url = format!("http://127.0.0.1:{port}/api/queue");
        let queue: serde_json::Value =
            serde_json::from_reader(agent.get(&queue_url).set("Cookie", &jar).call().unwrap().into_reader()).unwrap();
        assert!(
            queue["items"].as_array().unwrap().iter().any(|item| item["id"] == "s1"),
            "the audio id must first be delivered in this reviewer's queue"
        );

        // Full GET first: the baseline bytes every later assertion is compared against.
        let full = agent.get(&url).set("Cookie", &jar).call().unwrap();
        assert_eq!(full.status(), 200);
        assert_eq!(full.header("Accept-Ranges"), Some("bytes"), "ranges must be ADVERTISED, not just honoured");
        let etag = full.header("ETag").expect("a validator, or conditional requests are impossible").to_string();
        assert_eq!(full.header("Cache-Control"), Some("private, no-cache"));
        assert_eq!(full.header("Vary"), Some("Cookie"));
        let mut whole = Vec::new();
        full.into_reader().read_to_end(&mut whole).unwrap();
        assert!(whole.len() > 30_000, "a second of 16 kHz 16-bit audio, got {} bytes", whole.len());

        // Safari's opening probe. 206 with a correct Content-Range, and the two bytes must be the
        // FIRST two — a server that ignores Range answers 200 with everything.
        let probe = agent.get(&url).set("Cookie", &jar).set("Range", "bytes=0-1").call().unwrap();
        assert_eq!(probe.status(), 206, "a Range request must be answered 206, not 200-with-everything");
        assert_eq!(probe.header("Content-Range"), Some(format!("bytes 0-1/{}", whole.len()).as_str()));
        let mut two = Vec::new();
        probe.into_reader().read_to_end(&mut two).unwrap();
        assert_eq!(two, &whole[0..=1], "the probe must get the first two bytes, not the first two of nothing");

        // A middle slice, which is what seeking produces. Content matters, not just length.
        let mid = agent.get(&url).set("Cookie", &jar).set("Range", "bytes=1000-1999").call().unwrap();
        assert_eq!(mid.status(), 206);
        assert_eq!(mid.header("Content-Range"), Some(format!("bytes 1000-1999/{}", whole.len()).as_str()));
        let mut slice = Vec::new();
        mid.into_reader().read_to_end(&mut slice).unwrap();
        assert_eq!(slice.len(), 1000);
        assert_eq!(slice, &whole[1000..=1999], "a seek must return the audio actually at that offset");

        // Conditional re-request: zero bytes on the wire, which is the cellular win.
        match agent.get(&url).set("Cookie", &jar).set("If-None-Match", &etag).call() {
            Ok(r) => {
                assert_eq!(r.status(), 304, "an unchanged clip must be 304, never re-sent");
                let mut body = Vec::new();
                r.into_reader().read_to_end(&mut body).unwrap();
                assert!(body.is_empty(), "a 304 must not carry the clip it just said was unchanged");
            }
            Err(e) => panic!("conditional request failed: {e}"),
        }

        // HEAD: the length without the body. Used to be a flat 404 while the GET beside it worked.
        let head = agent.head(&url).set("Cookie", &jar).call().unwrap();
        assert_eq!(head.status(), 200, "HEAD must reach the audio route, not fall through to 404");
        assert_eq!(
            head.header("Content-Length").and_then(|v| v.parse::<usize>().ok()),
            Some(whole.len()),
            "a HEAD is only useful if it reports the real length"
        );
        let mut head_body = Vec::new();
        head.into_reader().read_to_end(&mut head_body).unwrap();
        assert!(head_body.is_empty(), "a HEAD response must have no body at all");

        // Unsatisfiable must be refused, not silently answered with everything.
        match agent.get(&url).set("Cookie", &jar).set("Range", "bytes=99999999-").call() {
            Err(ureq::Error::Status(416, r)) => {
                assert_eq!(r.header("Content-Range"), Some(format!("bytes */{}", whole.len()).as_str()))
            }
            other => panic!("an unsatisfiable range must be 416 with the real length, got {other:?}"),
        }

        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock();
        let _ = join.join();
    }

    #[test]
    fn a_header_only_huge_content_length_is_rejected_and_the_server_survives() {
        use std::io::Write as _;

        let tmp = tempfile::tempdir().unwrap();
        let (_db, db_path) = test_db(tmp.path());
        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let state = Arc::new(Mutex::new(CouchState::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(0, server.clone(), db_path, state, shutdown.clone()).unwrap();

        let mut hostile = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        hostile.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(
            hostile,
            "POST /api/claim HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            usize::MAX
        )
        .unwrap();
        hostile.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = String::new();
        hostile.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 413"), "oversized declaration must be rejected: {response}");

        // A second connection proves the accept loop and host process survived the hostile header.
        let shell = ureq::get(&format!("http://127.0.0.1:{port}/")).call().unwrap();
        assert_eq!(shell.status(), 200);

        shutdown.store(true, Ordering::SeqCst);
        let _ = server.unblock();
        let _ = join.join();
    }

    #[test]
    fn the_clip_cache_serves_identical_bytes_and_stays_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let wav = tmp.path().join("clip.wav");
        write_test_wav(&wav, 8_000);
        let mut s = seg("s1", "دەق");
        s.audio_path = wav.to_string_lossy().to_string();

        let fp = audio_fingerprint(&s);
        let first = cached_audio(fp, &s).expect("materialises on a miss");
        let second = cached_audio(fp, &s).expect("serves from the cache on a hit");
        assert_eq!(first, second, "a cache hit must be byte-identical to the miss it replaced");
        assert!(Arc::ptr_eq(&first, &second), "a hit must return the SAME buffer, not decode again");
        let _ = &db;

        // Bounded by bytes: force the cap with synthetic entries and confirm the oldest go first and
        // the newest survives. An unbounded cache on a long review session is a slow memory leak on
        // the owner's machine, which is the same machine running the desktop app.
        {
            let mut cache = lock_audio_cache();
            cache.clear();
            cache.push((fp, Arc::new(vec![0u8; 8])));
        }
        let big = vec![0u8; AUDIO_CACHE_BYTES / 4];
        for k in 1..=6u64 {
            let mut cache = lock_audio_cache();
            cache.push((k + 1000, Arc::new(big.clone())));
            let mut total: usize = cache.iter().map(|(_, b)| b.len()).sum();
            while total > AUDIO_CACHE_BYTES && cache.len() > 1 {
                total -= cache.remove(0).1.len();
            }
        }
        let cache = lock_audio_cache();
        let total: usize = cache.iter().map(|(_, b)| b.len()).sum();
        assert!(total <= AUDIO_CACHE_BYTES, "the cache must stay inside its cap, held {total} bytes");
        assert!(!cache.iter().any(|(k, _)| *k == fp), "the OLDEST entry is the one evicted");
        assert_eq!(cache.last().map(|(k, _)| *k), Some(1006), "the newest entry always survives");
    }

    #[test]
    fn spot_check_accounting_survives_a_long_multi_batch_session() {
        // Everything about spot checks had been tested on ONE batch. A real sitting is a dozen batches,
        // and the accounting has to hold across all of them: the served-set is in-memory AND persisted,
        // the candidate query excludes only clips with a SCORE ROW, and the ratio is recomputed per
        // batch. Any of those could drift over a session without a single-batch test noticing.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        const WORK: usize = QUEUE_BATCH * 5;
        for n in 0..WORK {
            db.insert_segment(&seg(&format!("w{n:03}"), "کاری ڕاستەقینە")).unwrap();
        }
        // Plenty of answer keys, so `wanted` is never starved and the ratio is the binding constraint.
        for n in 0..30 {
            gold_seg(&db, &format!("g{n:02}"), "دەقی هەڵە", "دەقی ڕاست");
        }
        let state = state();

        let mut served_checks: Vec<String> = Vec::new();
        let mut work_done = 0usize;
        let mut tail_was_a_check = 0usize;
        for round in 0..6 {
            let (code, _, body, ..) = api_queue(&db, "Sara", &state);
            assert_eq!(code, 200, "round {round}");
            let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let items = payload["items"].as_array().unwrap().clone();
            if items.is_empty() {
                break;
            }
            let ids: Vec<String> = items.iter().map(|s| s["id"].as_str().unwrap().to_string()).collect();
            let checks: Vec<String> = ids.iter().filter(|id| id.starts_with('g')).cloned().collect();
            let work: Vec<String> = ids.iter().filter(|id| id.starts_with('w')).cloned().collect();

            // THE RATIO, per batch and rounded UP, so a short final batch is still measured.
            assert_eq!(
                checks.len(),
                work.len().div_ceil(SPOT_CHECK_EVERY),
                "round {round}: {} work clips must carry {} checks, got {}",
                work.len(),
                work.len().div_ceil(SPOT_CHECK_EVERY),
                checks.len()
            );
            // A trap at the tail of every batch is a pattern a reviewer learns within a session, which
            // is the one thing that makes the whole measurement worthless. Counted, not asserted per
            // round, so the failure message can say how often it happened.
            if ids.last().is_some_and(|id| id.starts_with('g')) {
                tail_was_a_check += 1;
            }
            // Answer everything, checks included — a check is real work to the reviewer.
            for id in &ids {
                let text = if id.starts_with('g') { "دەقی ڕاست" } else { "کاری ڕاستەقینە" };
                let body =
                    serde_json::json!({"heardMs": 600_000,  "id": id, "action": "edit", "text": text }).to_string();
                let (dc, ..) = api_decision(&db, body.as_bytes(), "Sara", &state);
                assert_eq!(dc, 200, "round {round}: submitting {id}");
            }
            served_checks.extend(checks);
            work_done += work.len();
        }

        assert!(work_done >= QUEUE_BATCH * 4, "the session must span several batches, did {work_done}");
        assert_eq!(
            tail_was_a_check, 0,
            "a spot check landed last in {tail_was_a_check} of the batches — the tail is the one \
             position a reviewer can learn without trying"
        );

        // NEVER THE SAME CHECK TWICE to the same reviewer. Re-serving one they have already answered
        // teaches them the answer and then scores them on knowing it.
        let mut sorted = served_checks.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), served_checks.len(), "a check was served twice to the same reviewer");

        // ONE SCORE PER CHECK ANSWERED. Not per submit, not per batch.
        let report = db.spot_check_report().unwrap();
        let sara = report.iter().find(|r| r.reviewer == "Sara").expect("Sara was scored");
        assert_eq!(
            sara.checks,
            served_checks.len(),
            "every check served and answered must appear exactly once in the score"
        );
        assert_eq!(sara.noticed, served_checks.len(), "she corrected every one, so all noticed");

        // And the corpus got the WORK, not the checks: a spot check must never claim attribution.
        for id in &served_checks {
            let row = db.get_segment_by_id(id).unwrap().unwrap();
            assert_eq!(row.reviewed_by, None, "{id} is an answer key, not a review");
        }
    }

    #[test]
    fn a_lease_that_lapsed_while_the_phone_slept_is_reclaimable_but_never_stolen_silently() {
        // The backgrounded-phone case, which nothing covered. A reviewer's phone sleeps or their browser
        // tab is backgrounded: the renew heartbeat stops, and after LEASE_TTL the lease lapses. Two
        // things must then be true, and they pull in opposite directions —
        //   * nobody else has taken it -> the reviewer who still has it open KEEPS it. Refusing here
        //     would destroy a correction they typed before the phone slept, for no benefit at all.
        //   * somebody else HAS taken it -> the reviewer is told, at renew time, while their text can
        //     still be copied, rather than at save time when it is already unsaveable.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        db.insert_segment(&seg("s2", "دەق دوو")).unwrap();
        let state = state();
        assert_eq!(queue_ids(&db, "Sara", &state).len(), 2, "Sara holds both");

        // Age Sara's leases past the TTL, which is what a sleeping phone produces. Instant cannot be
        // fabricated, but it can be walked backwards.
        let stale = Instant::now().checked_sub(LEASE_TTL + Duration::from_secs(60)).expect("clock has headroom");
        {
            let mut guard = lock_state(&state);
            for (_, (_, granted)) in guard.leases.iter_mut() {
                *granted = stale;
            }
        }

        // NOBODY took it: renewing must succeed and hand the clip back, not refuse it.
        let renew = serde_json::json!({ "id": "s1" }).to_string();
        let (code, ..) = api_renew(renew.as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a lapsed-but-unclaimed lease must be reclaimable by the reviewer holding it");
        // ...and the decision she typed before the phone slept must still land.
        let decide =
            serde_json::json!({"heardMs": 600_000,  "id": "s1", "action": "edit", "text": "دەستکاری" }).to_string();
        assert_eq!(api_decision(&db, decide.as_bytes(), "Sara", &state).0, 200);

        // Now the other half: age the remaining lease again and let HEMN pick it up.
        {
            let mut guard = lock_state(&state);
            for (_, (_, granted)) in guard.leases.iter_mut() {
                *granted = stale;
            }
        }
        assert!(queue_ids(&db, "Hemn", &state).contains(&"s2".to_string()), "an expired lease is available");

        // Sara's phone wakes up. Renewing s2 must be REFUSED — now, not at save time.
        let renew2 = serde_json::json!({ "id": "s2" }).to_string();
        let (code2, _, msg, ..) = api_renew(renew2.as_bytes(), "Sara", &state);
        assert_eq!(code2, 409, "a clip somebody else now holds must be refused at renew");
        assert!(
            String::from_utf8_lossy(&msg).contains("another reviewer"),
            "and the refusal must say why, so the reviewer can copy their text: {}",
            String::from_utf8_lossy(&msg)
        );
        // The refusal must not have quietly handed her the lease back as a side effect.
        let holder = lock_state(&state).holder("s2", Instant::now()).map(str::to_string);
        assert_eq!(holder.as_deref(), Some("Hemn"), "a refused renew must not steal the clip back");
    }

    #[test]
    fn the_queue_serves_clips_in_the_order_it_leased_them() {
        // P1.3: api_queue used to read every pending row IN ORDER and build the payload in the same
        // pass, so the served order could not disagree with the leased order. It now selects IDs first
        // and hydrates the batch with get_segments_by_ids — which re-imposes its OWN global ordering
        // and would hand the reviewer a differently-ordered batch than the one it leased, silently
        // changing which clip is "clip 1 of 25". The re-index by id is what prevents that, and this is
        // what proves the re-index is actually doing something.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        for n in 0..QUEUE_BATCH {
            db.insert_segment(&seg(&format!("q{n:04}"), "دەقی سەرەتایی")).unwrap();
        }
        let state = state();

        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let served: Vec<String> =
            payload["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect();

        // The order the SELECT hands them out in is the order the reviewer must see them in.
        let expected: Vec<String> = db.pending_segment_ids().unwrap().into_iter().take(QUEUE_BATCH).collect();
        assert_eq!(served, expected, "the served batch must be in the order the clips were leased");

        // Every served clip carries its real text, i.e. the batch was genuinely hydrated rather than
        // emitted from ids alone.
        for item in payload["items"].as_array().unwrap() {
            assert_eq!(item["text"].as_str(), Some("دەقی سەرەتایی"), "a served clip must carry its transcript");
        }
    }

    #[test]
    fn the_queue_reports_the_whole_backlog_not_just_the_batch_in_hand() {
        // Without a total the page can only count the batch it holds, so a reviewer working a long
        // backlog watched a progress bar fill and reset over and over with no way to tell a
        // nearly-finished corpus from a barely-started one.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        let total = QUEUE_BATCH * 3 + 7;
        for n in 0..total {
            db.insert_segment(&seg(&format!("p{n:04}"), "دەقی سەرەتایی")).unwrap();
        }
        let state = state();

        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = payload["items"].as_array().unwrap().len();
        assert_eq!(
            payload["pendingTotal"].as_u64(),
            Some(total as u64),
            "pendingTotal must be the whole pending backlog, not the {items} clips in this batch"
        );
        assert!(items < total, "the batch is deliberately smaller than the backlog, or this proves nothing");

        // A SECOND reviewer sees the same total. It is a property of the corpus, not of who asked:
        // a per-reviewer total would make two people report different progress on one backlog.
        let (_, _, body2, ..) = api_queue(&db, "Hemn", &state);
        let p2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert_eq!(p2["pendingTotal"].as_u64(), Some(total as u64), "the backlog total is not per-reviewer");
        assert!(p2["heldByOthers"].as_u64().unwrap() > 0, "Sara's lease is visible to Hemn");

        // And it SHRINKS as work lands — a total that never moved would be a decoration, not progress.
        let first = payload["items"][0]["id"].as_str().unwrap();
        let decision =
            serde_json::json!({"heardMs": 600_000,  "id": first, "action": "accept", "text": "دەقی سەرەتایی" });
        let (dc, ..) = api_decision(&db, decision.to_string().as_bytes(), "Sara", &state);
        assert_eq!(dc, 200);
        let (_, _, body3, ..) = api_queue(&db, "Sara", &state);
        let p3: serde_json::Value = serde_json::from_slice(&body3).unwrap();
        assert_eq!(p3["pendingTotal"].as_u64(), Some(total as u64 - 1), "a reviewed clip must leave the pending total");
    }
}
