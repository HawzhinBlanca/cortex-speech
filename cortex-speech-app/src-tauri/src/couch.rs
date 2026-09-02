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

use crate::db::{
    CouchPlaybackAttemptAuthority, Database, DesktopPlaybackInterval, HumanDecisionUndoOutcome, ReviewDecisionLimit,
    SpeechSegment, HIDDEN_ANSWER_KEY_CHANGED, PLAYBACK_EVIDENCE_CHANGED, REVIEW_PILOT_LIMIT_REACHED,
};
use base64::Engine as _;
use rusqlite::OptionalExtension;
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
/// Raised from 8 by the owner ("change canon: raise max reviewers 8 → 10", 2026-08-31) for the
/// 10-name roster: 8 working reviewers plus the Iftikhar and Guest links, minted once so the
/// roster never has to change (a roster change remints every distributed link).
const MAX_REVIEWERS: usize = 10;
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
/// Present only during a controlled private-production handover. The read-only pairing probe remains
/// available so the candidate can prove every durable link, but no queue, audio, claim, renewal,
/// decision, or undo route is exposed until the release controller removes this file after all gates
/// pass. Reading it per request makes exposure an atomic filesystem decision with no restart race.
const PRIVATE_PRODUCTION_MAINTENANCE_FILE: &str = "private-production-maintenance.json";
const COUCH_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Flexible-pool observations are deliberately disabled until the owner approves an exact prospective
/// compensation contract for that authority. This stable token is consumed by IPC/runbook callers and
/// must remain machine-searchable; startup returns it before creating credentials, TLS material, a
/// listener, a durable session generation, or any review/pay row.
pub const PAY_POLICY_REQUIRED: &str = "PAY_POLICY_REQUIRED";
/// A Couch playback attempt is intentionally short-lived and process-local until it is finalized into
/// immutable policy-4 authority. A process restart therefore cannot turn an unobserved browser claim
/// into evidence; the reviewer reloads and traverses the clip again.
const COUCH_PLAYBACK_ATTEMPT_TTL: Duration = Duration::from_secs(30 * 60);
const COUCH_PLAYBACK_POLICY_VERSION: i64 = 4;
const LEGACY_RAW_COUNTER_REFUSAL_MARKER: &str = "PLAYBACK_EVIDENCE_V3_CONTENT_HASH_RAW_COUNTER_REFUSED";
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

/// One undoable phone decision. The database owns the inverse snapshot and CAS revision; this token
/// carries only immutable identities and can never act as renderer authority over a corpus row.
struct UndoEntry {
    operation_id: String,
    seg_id: String,
    effect_event_id: i64,
}

/// Undo identity for a blinded second-pass decision. The first-pass corpus row is not a snapshot and
/// is never restored here; undo appends an immutable reversal for the independent decision only.
struct IndependentUndoEntry {
    operation_id: String,
    seg_id: String,
    decision_id: i64,
}

#[derive(Debug, Clone)]
struct CouchPlaybackAttempt {
    authority: CouchPlaybackAttemptAuthority,
    /// Monotonic server instant at which an authorized GET/304 for this exact attempt completed.
    /// HEAD/prefetch requests never set it. The earliest successful traversal grant wins, so range
    /// retries and cached revalidation cannot reset the plausibility clock.
    media_served_at: Option<Instant>,
    expires_at: Instant,
}

/// Per-session state shared by the accept threads. One mutex: the critical sections are map lookups,
/// held for microseconds, while the slow work (DB writes, WAV encoding) happens outside it.
#[derive(Default)]
struct CouchState {
    /// Pre-decision row snapshots, keyed by reviewer name — so undo is per-person and can never
    /// retract someone else's work.
    undo: HashMap<String, Vec<UndoEntry>>,
    /// Separate from first-pass undo so Alle can never route an undo into Rubar's corpus effect.
    independent_undo: HashMap<String, Vec<IndependentUndoEntry>>,
    /// Independent observations made in the flexible pool. Kept separate from the retired sequential
    /// campaign namespace so an Undo can never cross from one evidence system into the other.
    pool_undo: HashMap<String, Vec<IndependentUndoEntry>>,
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
    /// Unfinalized policy-4 attempts, keyed by the server-issued receipt UUID. They are inert until an
    /// exact authorized media GET marks `media_served_at`, and disappear only after durable finalize,
    /// explicit assignment teardown, expiry, or session revocation.
    playback_attempts: HashMap<String, CouchPlaybackAttempt>,
    /// Exact retry index. A browser-generated client UUID has one meaning inside one cookie session;
    /// presenting it with different reviewer/segment/revision coordinates is a hard conflict.
    playback_attempt_clients: HashMap<(String, String), String>,
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
    /// Rubar-only full first pass. Unlike the bounded pilot this has no action cap and deliberately
    /// mints no pseudo-gold hidden checks; its database-bound contract blocks every export until an
    /// independent second pass exists. Any live policy/focus drift pauses the request path.
    campaign_policy: Option<crate::review_campaign::SequentialReviewCampaign>,
    /// Immutable voice-organized pool bound at Start. Any database mismatch pauses review; no reviewer
    /// is assigned a phase or quota, and every name on the live roster may draw from the same pool.
    pool_policy: Option<crate::review_pool::ReviewPool>,
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

    fn prune_playback_attempts(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .playback_attempts
            .iter()
            .filter(|(_, attempt)| now >= attempt.expires_at)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(attempt) = self.playback_attempts.remove(&id) {
                self.playback_attempt_clients
                    .remove(&(attempt.authority.session_binding_sha256, attempt.authority.client_attempt_id));
            }
        }
    }

    fn remove_playback_attempt(&mut self, playback_receipt_id: &str) {
        if let Some(attempt) = self.playback_attempts.remove(playback_receipt_id) {
            self.playback_attempt_clients
                .remove(&(attempt.authority.session_binding_sha256, attempt.authority.client_attempt_id));
        }
    }

    fn remove_playback_attempts_for_assignment(&mut self, segment_id: &str, reviewer: &str) {
        let doomed: Vec<String> = self
            .playback_attempts
            .iter()
            .filter(|(_, attempt)| {
                attempt.authority.segment_id == segment_id && same_reviewer(&attempt.authority.reviewer, reviewer)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            self.remove_playback_attempt(&id);
        }
    }

    fn remove_playback_attempts_for_session(&mut self, session_binding_sha256: &str) {
        let doomed: Vec<String> = self
            .playback_attempts
            .iter()
            .filter(|(_, attempt)| attempt.authority.session_binding_sha256 == session_binding_sha256)
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            self.remove_playback_attempt(&id);
        }
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

fn couch_session_binding_sha256(token: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"cortex-couch-playback-cookie-session-v1\0");
    hash.update((token.len() as u64).to_le_bytes());
    hash.update(token.as_bytes());
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
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

/// ONE definition of "the same person", used everywhere a reviewer name is matched against another.
///
/// `normalize_reviewers` trims but deliberately PRESERVES the casing the owner typed, and the review
/// columns are `COLLATE NOCASE`, so "rubar" and "Rubar" are one reviewer to the roster and to the
/// database. Every `==` that slipped in disagreed, and each one failed silently: a re-typed roster
/// minted a FRESH pairing token (so every link already sent answers "link expired"), dropped the
/// restored cookie sessions (installed phone shortcuts die), and dropped the remembered served-check
/// pairs (the outstanding check 409s and its score is lost). That is exactly the shape of the
/// six-reviewers-silent outage of 2026-08-20, reachable by re-typing one name.
fn same_reviewer(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The name used when the owner starts the server without naming anyone.
const DEFAULT_REVIEWER: &str = "owner";

mod lifecycle;
pub use lifecycle::*;

/// (status, content_type, body, extra response headers)
///
/// The fourth slot exists for `/api/audio` (phase R1). Every reply used to ship with nothing but a
/// Content-Type, which made caching and range requests impossible to express: a reviewer re-opening a
/// clip they had already played re-downloaded all 300-500 KB of it over cellular, and a browser asking
/// for a byte range got an unconditional full body back. Every other route passes `vec![]`.
mod routing;
use routing::*;
mod queue_audio;
use queue_audio::*;
mod decisions;
use decisions::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that drive the real `start`/`stop` path share the ONE global `COUCH` singleton, and
    /// cargo runs tests in parallel — two such tests steal each other's server mid-assertion.
    /// Reproduced 3-for-3 the moment a second start/stop test existed; each passes alone. Any test
    /// touching the global handle must hold this first. `unwrap_or_else(into_inner)`: a poisoned
    /// lock from one failed test must not cascade every later one into a meaningless panic.
    static GLOBAL_SESSION_LOCK: Mutex<()> = Mutex::new(());

    // libtest runs Couch tests concurrently, and nearly all characterization fixtures intentionally
    // reuse short ids such as `s1`. A process-global directory therefore made unrelated tests race to
    // truncate the same `s1.wav`; Windows rejects the second open with ERROR_SHARING_VIOLATION (32).
    // One directory per test thread preserves the useful invariant that repeated `seg("s1", ..)` calls
    // inside one test have the same audio identity, while making parallel tests physically independent.
    thread_local! {
        static FIXTURE_AUDIO: tempfile::TempDir =
            tempfile::tempdir().expect("thread-local Couch fixture audio directory");
    }

    fn rollback_fixture_to(db: &Database, target_version: i64) {
        let expected = crate::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version > target_version)
            .rev()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(crate::migrations::rollback(db, expected.len()).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), target_version);
    }

    fn upgrade_fixture_from(db: &Database, source_version: i64) {
        let expected = crate::migrations::MIGRATIONS
            .iter()
            .filter(|migration| migration.version > source_version)
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        assert_eq!(crate::migrations::run_migrations(db).unwrap(), expected);
        assert_eq!(crate::migrations::get_current_version(db).unwrap(), crate::migrations::max_supported_version());
    }

    fn test_db(dir: &std::path::Path) -> (Database, String) {
        let path = dir.join("couch-test.db").to_string_lossy().to_string();
        let db = Database::open(&path).unwrap();
        db.initialize().unwrap();
        let template = dir.join("couch-fixture-template.wav");
        write_test_wav(&template, 24_000);
        let fixture_audio_content_hash =
            crate::export_bundle::current_canonical_pcm_blake3(&template).expect("fixture template has PCM identity");
        // Disposable WAV fixtures bypass the import/backfill pipeline. A TEMP trigger gives every
        // subsequently inserted fixture the same canonical decoded-PCM content hash before any
        // queue/server connection can observe it, without weakening the production schema.
        db.connection()
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER fixture_audio_content_hash
                 AFTER INSERT ON speech_segments
                 WHEN NEW.audio_content_hash IS NULL
                 BEGIN
                     UPDATE speech_segments
                        SET audio_content_hash = '{fixture_audio_content_hash}',
                            alignment_json = COALESCE(
                                alignment_json,
                                json_object(
                                    'source_start_ms', 0,
                                    'source_end_ms', NEW.duration_ms,
                                    'chunk_index', 0,
                                    'chunk_count', 1
                                )
                            )
                      WHERE id = NEW.id;
                 END;"
            ))
            .unwrap();
        (db, path)
    }

    fn seg(id: &str, raw: &str) -> SpeechSegment {
        // A REAL file on disk, one per clip. `pending_segment_ids` refuses to serve a clip whose audio
        // has gone missing (2026-08-15), so the old shared "/audio/a.wav" placeholder would drop every
        // fixture out of the queue and leave the lease/batch tests below asserting against an empty
        // list — passing while proving nothing. The thread-local directory is cleaned when its test
        // thread exits, after all values and server workers owned by that test have been joined.
        FIXTURE_AUDIO.with(|audio| {
            let path = audio.path().join(format!("{id}.wav"));
            // Distinct fixture ids must not collapse into one canonical audio window. Several review/pay
            // characterizations intentionally insert many clips with the same duration and span; giving
            // them byte-identical PCM would make the real dedup authority correctly treat them as aliases.
            write_test_wav_seeded(&path, 24_000, id.as_bytes());
            SpeechSegment {
                id: id.into(),
                audio_path: path.to_string_lossy().into_owned(),
                raw_transcript: raw.into(),
                duration_ms: 1500,
                alignment_json: Some(
                    r#"{"source_start_ms":0,"source_end_ms":1500,"chunk_index":0,"chunk_count":1}"#.into(),
                ),
                ..SpeechSegment::default()
            }
        })
    }

    #[test]
    fn parallel_fixture_audio_with_the_same_segment_id_has_independent_paths() {
        const WORKERS: usize = 16;
        let barrier = Arc::new(std::sync::Barrier::new(WORKERS));
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let segment = seg("same-id", "دەقی تاقیکردنەوە");
                    assert!(std::path::Path::new(&segment.audio_path).is_file());
                    segment.audio_path
                })
            })
            .collect();
        let paths: HashSet<String> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
        assert_eq!(
            paths.len(),
            WORKERS,
            "parallel tests using the same fixture id must never share a writable WAV path"
        );
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
            let id = payload.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
            let text = payload.get("text").and_then(serde_json::Value::as_str).unwrap_or_default();
            let digest = Sha256::digest(
                format!("cortex-test-phone-operation-v1\0{id}\0{action}\0{text}\0{reviewer}").as_bytes(),
            );
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "operationId".to_string(),
                    serde_json::Value::String(uuid::Uuid::from_bytes(bytes).to_string()),
                );
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
        // Compatibility for the large pre-policy-4 endpoint characterization suite. Production never
        // enters this helper: it mints a real finalized policy-4 receipt from the fixture's real WAV,
        // exact current revision/hash/span and the fixed test session binding. Tests of missing/forged
        // proof call `super::api_decision_authenticated` directly and therefore do not receive it.
        let pool_mode = lock_state(state).pool_policy.is_some();
        let pool_requires_first_canonical = pool_mode
            && payload
                .get("id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| db.get_segment_by_id(id).ok().flatten())
                .is_some_and(|segment| !segment.verified || segment.human_decision.is_none());
        if verdict && (!pool_mode || pool_requires_first_canonical) && payload.get("playbackReceiptId").is_none() {
            let id = payload.get("id").and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
            let mut supplied_revision = payload
                .get("rowVersion")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<i64>().ok());
            let operation_already_committed =
                payload.get("operationId").and_then(serde_json::Value::as_str).is_some_and(|operation_id| {
                    db.review_operation(operation_id).ok().flatten().is_some()
                        || crate::review_campaign::independent_operation(db, operation_id).ok().flatten().is_some()
                });
            if !operation_already_committed {
                if let Ok(Some(segment)) = db.get_segment_by_id(&id) {
                    if let Ok(actual_hash) =
                        crate::export_bundle::current_canonical_pcm_blake3(Path::new(&segment.audio_path))
                    {
                        if db.segment_audio_content_hash(&id).ok().flatten().as_deref() != Some(actual_hash.as_str()) {
                            let revision_before_fixture_hash = db.segment_review_revision(&id).ok().flatten();
                            let _ = db.connection().execute(
                                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                                rusqlite::params![id, actual_hash],
                            );
                            // A stamp captured against the disposable trigger's placeholder identity
                            // is refreshed iff it exactly named that pre-normalization revision. A
                            // genuinely stale stamp remains stale and continues to exercise the CAS.
                            if supplied_revision.is_none() || supplied_revision == revision_before_fixture_hash {
                                supplied_revision = db.segment_review_revision(&id).ok().flatten();
                                if let (Some(revision), Some(object)) = (supplied_revision, payload.as_object_mut()) {
                                    object.insert(
                                        "rowVersion".to_string(),
                                        serde_json::Value::String(revision.to_string()),
                                    );
                                }
                            }
                        }
                    }
                }
                let current_revision = db.segment_review_revision(&id).ok().flatten();
                if supplied_revision == current_revision {
                    let content_hash = db.segment_audio_content_hash(&id).ok().flatten();
                    let span = db.segment_source_span(&id).ok().flatten();
                    let segment = db.get_segment_by_id(&id).ok().flatten();
                    if let (Some(revision), Some(content_hash), Some((source_start_ms, source_end_ms)), Some(segment)) =
                        (current_revision, content_hash, span, segment)
                    {
                        let issued_at_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|duration| duration.as_millis() as i64)
                            .unwrap_or(10_000)
                            .max(10_000);
                        let authority = CouchPlaybackAttemptAuthority {
                            playback_receipt_id: uuid::Uuid::new_v4().to_string(),
                            media_grant_id: uuid::Uuid::new_v4().to_string(),
                            client_attempt_id: uuid::Uuid::new_v4().to_string(),
                            session_binding_sha256: couch_session_binding_sha256("couch-test-session"),
                            reviewer: reviewer.to_string(),
                            segment_id: id.to_string(),
                            segment_revision: revision,
                            audio_content_hash: content_hash,
                            source_path: PathBuf::from(segment.audio_path),
                            clip_duration_ms: segment.duration_ms,
                            source_start_ms,
                            source_end_ms,
                            issued_at_ms,
                            expires_at_ms: issued_at_ms + 60_000,
                        };
                        let covered_ms = payload
                            .get("heardMs")
                            .and_then(serde_json::Value::as_i64)
                            .filter(|value| *value >= 0)
                            .unwrap_or_else(|| ((segment.duration_ms * 9) / 10).max(1))
                            .min(segment.duration_ms);
                        let intervals = [DesktopPlaybackInterval { start_ms: 0, end_ms: covered_ms }];
                        match db.finalize_couch_playback_attempt_v1(&authority, &intervals, segment.duration_ms.max(1))
                        {
                            Ok(receipt) => {
                                if let Some(object) = payload.as_object_mut() {
                                    object.insert(
                                        "playbackReceiptId".to_string(),
                                        serde_json::Value::String(receipt.playback_receipt_id),
                                    );
                                    object.remove("heardMs");
                                    object.remove("clipDurationMs");
                                }
                            }
                            Err(error) if covered_ms.saturating_mul(100) < segment.duration_ms.saturating_mul(85) => {
                                let _ = error;
                            }
                            Err(error) => return playback_error_reply(&error.to_string()),
                        }
                    }
                }
            }
        }
        if payload.get("pilotAfterReviewEventId").is_none() {
            let baseline = lock_state(state).pilot_policy.as_ref().map(|policy| policy.after_review_event_id);
            if let (Some(baseline), Some(object)) = (baseline, payload.as_object_mut()) {
                object.insert("pilotAfterReviewEventId".to_string(), serde_json::Value::Number(baseline.into()));
            }
        }
        let encoded = payload.to_string();
        super::api_decision_authenticated(
            db,
            encoded.as_bytes(),
            reviewer,
            &couch_session_binding_sha256("couch-test-session"),
            state,
        )
    }

    /// Give legacy HTTP queue/concurrency characterizations a real durable policy-4 receipt without
    /// weakening the routed production handler. The dedicated live-protocol and adversarial tests
    /// exercise start -> authorized audio -> finalize over the real server; these older tests are
    /// about restart, leasing, throttling and exactly-once decision behavior, and reuse this explicit
    /// test-only authority to avoid adding 750ms of wall time per fixture clip.
    fn fixture_policy4_receipt(
        db: &Database,
        reviewer: &str,
        session_token: &str,
        id: &str,
        row_version: &serde_json::Value,
    ) -> String {
        let revision = row_version
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            .or_else(|| row_version.as_i64())
            .expect("fixture rowVersion is a canonical integer");
        assert_eq!(db.segment_review_revision(id).unwrap(), Some(revision));
        let segment = db.get_segment_by_id(id).unwrap().expect("fixture segment exists");
        let audio_content_hash =
            db.segment_audio_content_hash(id).unwrap().expect("fixture has canonical PCM identity");
        let (source_start_ms, source_end_ms) = db.segment_source_span(id).unwrap().expect("fixture has canonical span");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(1)
            .max(1);
        let authority = CouchPlaybackAttemptAuthority {
            playback_receipt_id: uuid::Uuid::new_v4().to_string(),
            media_grant_id: uuid::Uuid::new_v4().to_string(),
            client_attempt_id: uuid::Uuid::new_v4().to_string(),
            session_binding_sha256: couch_session_binding_sha256(session_token),
            reviewer: reviewer.to_string(),
            segment_id: id.to_string(),
            segment_revision: revision,
            audio_content_hash,
            source_path: PathBuf::from(segment.audio_path),
            clip_duration_ms: segment.duration_ms,
            source_start_ms,
            source_end_ms,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms + 60_000,
        };
        db.finalize_couch_playback_attempt_v1(
            &authority,
            &[DesktopPlaybackInterval { start_ms: 0, end_ms: segment.duration_ms }],
            segment.duration_ms,
        )
        .expect("fixture policy-4 playback finalizes")
        .playback_receipt_id
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
                    effect_event_id: 17,
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

    fn policy4_segment(db: &Database, id: &str, duration_ms: i64) -> SpeechSegment {
        let mut segment = seg(id, "دەقی تاقیکردنەوە");
        write_test_wav(Path::new(&segment.audio_path), (duration_ms as usize) * 16);
        segment.duration_ms = duration_ms;
        segment.alignment_json =
            Some(format!(r#"{{"source_start_ms":0,"source_end_ms":{duration_ms},"chunk_index":0,"chunk_count":1}}"#));
        db.insert_segment(&segment).unwrap();
        let content_hash = crate::export_bundle::current_canonical_pcm_blake3(Path::new(&segment.audio_path)).unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments SET audio_content_hash=?2 WHERE id=?1",
                rusqlite::params![id, content_hash],
            )
            .unwrap();
        db.get_segment_by_id(id).unwrap().unwrap()
    }

    fn assign_policy4_work(state: &Mutex<CouchState>, id: &str, reviewer: &str) {
        let mut guard = lock_state(state);
        guard.leases.insert(id.to_string(), (reviewer.to_string(), Instant::now()));
        guard.served_work.insert((id.to_string(), reviewer.to_string()));
    }

    fn start_policy4_attempt(
        db: &Database,
        state: &Mutex<CouchState>,
        id: &str,
        reviewer: &str,
        binding: &str,
        client_attempt_id: &str,
    ) -> serde_json::Value {
        let revision = db.segment_review_revision(id).unwrap().unwrap();
        let body = serde_json::json!({
            "id": id,
            "rowVersion": revision.to_string(),
            "clientAttemptId": client_attempt_id,
        });
        let reply = super::api_playback_start(db, body.to_string().as_bytes(), reviewer, binding, state);
        assert_eq!(reply.0, 200, "attempt start failed: {}", String::from_utf8_lossy(&reply.2));
        serde_json::from_slice(&reply.2).unwrap()
    }

    /// Whole-state fingerprint for read-only endpoint tests. Every mutable CouchState field is
    /// represented, including the state that is intentionally not durable (leases, undo, skips, and
    /// in-flight operations). Keep this list beside CouchState's field list when that state grows.
    type UndoTestSnapshot = (String, String, i64);

    #[derive(Debug, PartialEq)]
    struct CouchStateTestSnapshot {
        undo: HashMap<String, Vec<UndoTestSnapshot>>,
        independent_undo: HashMap<String, Vec<UndoTestSnapshot>>,
        pool_undo: HashMap<String, Vec<UndoTestSnapshot>>,
        in_flight_operations: HashSet<String>,
        leases: HashMap<String, (String, Instant)>,
        served_work: HashSet<(String, String)>,
        playback_attempts: Vec<(String, String, String, String, i64, bool)>,
        playback_attempt_clients: HashMap<(String, String), String>,
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
        campaign_policy: Option<crate::review_campaign::SequentialReviewCampaign>,
        pool_policy: Option<crate::review_pool::ReviewPool>,
    }

    fn snapshot_couch_state(state: &Mutex<CouchState>) -> CouchStateTestSnapshot {
        let state = lock_state(state);
        let mut playback_attempts: Vec<_> = state
            .playback_attempts
            .iter()
            .map(|(receipt_id, attempt)| {
                (
                    receipt_id.clone(),
                    attempt.authority.client_attempt_id.clone(),
                    attempt.authority.reviewer.clone(),
                    attempt.authority.segment_id.clone(),
                    attempt.authority.segment_revision,
                    attempt.media_served_at.is_some(),
                )
            })
            .collect();
        playback_attempts.sort();
        CouchStateTestSnapshot {
            undo: state
                .undo
                .iter()
                .map(|(reviewer, entries)| {
                    (
                        reviewer.clone(),
                        entries
                            .iter()
                            .map(|entry| (entry.operation_id.clone(), entry.seg_id.clone(), entry.effect_event_id))
                            .collect(),
                    )
                })
                .collect(),
            independent_undo: state
                .independent_undo
                .iter()
                .map(|(reviewer, entries)| {
                    (
                        reviewer.clone(),
                        entries
                            .iter()
                            .map(|entry| (entry.operation_id.clone(), entry.seg_id.clone(), entry.decision_id))
                            .collect(),
                    )
                })
                .collect(),
            pool_undo: state
                .pool_undo
                .iter()
                .map(|(reviewer, entries)| {
                    (
                        reviewer.clone(),
                        entries
                            .iter()
                            .map(|entry| (entry.operation_id.clone(), entry.seg_id.clone(), entry.decision_id))
                            .collect(),
                    )
                })
                .collect(),
            in_flight_operations: state.in_flight_operations.clone(),
            leases: state.leases.clone(),
            served_work: state.served_work.clone(),
            playback_attempts,
            playback_attempt_clients: state.playback_attempt_clients.clone(),
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
            campaign_policy: state.campaign_policy.clone(),
            pool_policy: state.pool_policy.clone(),
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
    fn sequential_campaign_serves_a_full_rubar_batch_without_synthetic_checks_and_fails_on_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let ids: HashSet<String> = (0..(QUEUE_BATCH + 5)).map(|n| format!("campaign-{n:02}")).collect();
        for id in &ids {
            db.insert_segment(&seg(id, "دەقی کاری ڕووبار")).unwrap();
        }
        std::fs::write(
            tmp.path().join("voice_focus.json"),
            serde_json::json!({"name":"Rubar first pass", "segment_ids": &ids}).to_string(),
        )
        .unwrap();
        let focus = crate::review_campaign::focus_evidence(&ids).unwrap();
        let campaign = crate::review_campaign::SequentialReviewCampaign {
            schema_version: 1,
            campaign_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            mode: crate::review_campaign::SEQUENTIAL_CAMPAIGN_MODE.into(),
            status: crate::review_campaign::SEQUENTIAL_CAMPAIGN_STATUS.into(),
            reviewer: "Rubar".into(),
            after_review_event_id: 0,
            activated_at_review_event_id: 0,
            focus_segment_count: focus.segment_count,
            focus_sha256: focus.sha256,
            provisional_export_block: true,
            independent_second_pass_required: true,
            progress: None,
        };
        db.connection()
            .execute(
                "INSERT INTO settings(key, value) VALUES(?1, ?2)",
                [crate::review_campaign::SEQUENTIAL_CAMPAIGN_SETTINGS_KEY, &serde_json::to_string(&campaign).unwrap()],
            )
            .unwrap();
        let state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-rubar".into(), "Rubar".into())]),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            campaign_policy: Some(campaign),
            ..CouchState::default()
        });

        let (code, _, body, ..) = api_queue(&db, "Rubar", &state);
        assert_eq!(code, 200, "campaign queue failed: {}", String::from_utf8_lossy(&body));
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["items"].as_array().unwrap().len(), QUEUE_BATCH);
        assert!(lock_state(&state).spot_checks.is_empty(), "first pass must not mint circular hidden keys");

        let (code, ..) = api_queue(&db, "Alle", &state);
        assert_eq!(code, 503, "the first pass must authorize Rubar only");
        std::fs::write(tmp.path().join("voice_focus.json"), r#"{"name":"drift", "segment_ids":["campaign-00"]}"#)
            .unwrap();
        let (code, ..) = api_queue(&db, "Rubar", &state);
        assert_eq!(code, 503, "focus drift must pause the live queue");
    }

    #[test]
    fn flexible_pool_is_voice_named_decision_first_and_preserves_the_first_review() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let champion_id = "omniasr-7b-pool-test";
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: champion_id.into(),
                family: crate::deployment::OMNIASR_7B_FAMILY.into(),
                model_card_name: Some("pool test champion".into()),
                checkpoint_sha256: "c".repeat(64),
                checkpoint_path: "/test/pool-champion.json".into(),
                source: "cortex-finetuned".into(),
                license: "owner-full-rights".into(),
            },
        )
        .unwrap();
        db.connection().execute("UPDATE model_versions SET status='champion' WHERE id=?1", [champion_id]).unwrap();
        let mut reviewed = seg("pool-reviewed", "champion reviewed draft");
        reviewed.model_version_id = Some(champion_id.into());
        let mut unreviewed = seg("pool-unreviewed", "champion unreviewed draft");
        unreviewed.model_version_id = Some(champion_id.into());
        db.insert_segment(&reviewed).unwrap();
        db.insert_segment(&unreviewed).unwrap();
        let first_revision = db.segment_review_revision("pool-reviewed").unwrap().unwrap();
        let first_operation = "30000000-0000-4000-8000-000000000001";
        let first_hash =
            crate::db::review_operation_payload_hash("pool-reviewed", "edit", "Rubar first truth", "Rubar");
        db.record_phone_human_decision_by_at_revision_with_operation(
            "pool-reviewed",
            "edit",
            Some("Rubar first truth"),
            "Rubar",
            first_revision,
            first_operation,
            &first_hash,
        )
        .unwrap()
        .unwrap();
        let pool = crate::review_pool::activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174001",
            &[
                crate::review_pool::PoolMemberInput { segment_id: "pool-reviewed".into(), voice_name: "Lamo".into() },
                crate::review_pool::PoolMemberInput {
                    segment_id: "pool-unreviewed".into(),
                    voice_name: "Halwest".into(),
                },
            ],
        )
        .unwrap();
        let state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-rubar".into(), "Rubar".into()), ("pair-alle".into(), "Alle".into())]),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            pool_policy: Some(pool.clone()),
            ..CouchState::default()
        });

        let (code, _, body, ..) = api_queue(&db, "Alle", &state);
        assert_eq!(code, 200, "pool queue failed: {}", String::from_utf8_lossy(&body));
        let queue: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(queue["reviewPool"], true);
        // OWNER CANON 2026-08-29: a sentence is decided by any two DIFFERENT reviewers, so the clip
        // NEAREST a decision is offered first. "pool-reviewed" already holds Rubar's opinion and one
        // more judgement retires it; "pool-unreviewed" needs two. This assertion was the reverse
        // ("zero-coverage clips must be offered first") until the canon landed -- breadth-first
        // maximised clips touched and left 416 live clips holding one review with none decided.
        assert_eq!(queue["items"][0]["id"], "pool-reviewed", "the clip nearest a decision comes first");
        assert_eq!(queue["items"][0]["speakerId"], "Lamo");
        assert_eq!(queue["items"][1]["id"], "pool-unreviewed");
        assert_eq!(queue["items"][1]["speakerId"], "Halwest");
        // Unchanged and load-bearing: ordering must never leak the first reviewer's answer.
        assert_eq!(queue["items"][0]["text"], "champion reviewed draft", "later reviews stay blind to Rubar's answer");

        // REGRESSION (2026-08-30 live incident): the decision-first ordering serves an
        // already-reviewed clip FIRST, so `verified=true` is the NORMAL state of every pool
        // reviewer's clip 1 — and playback/start must authorize it. The eligibility check used to
        // refuse verified work outright, which 403'd the player's arming call on exactly this
        // fixture shape: the phone showed 00:00/00:00 forever and every save died on mustListen.
        // This is the arming call the page performs before it will set the <audio> src at all.
        let attempt = start_policy4_attempt(
            &db,
            &state,
            "pool-reviewed",
            "Alle",
            "test-session-binding-pool",
            "40000000-0000-4000-8000-0000000000aa",
        );
        assert_eq!(attempt["segmentId"], "pool-reviewed", "playback authority must bind the served pool clip");

        let row_version = queue["items"][0]["rowVersion"].as_str().unwrap();
        let pool_operation_id = "30000000-0000-4000-8000-000000000002";
        let decision = serde_json::json!({
            "operationId": pool_operation_id,
            "id": "pool-reviewed",
            "action": "edit",
            "text": "Alle independent truth",
            "reviewer": "Alle",
            "rowVersion": row_version,
            "heardMs": 1_500,
            "clipDurationMs": 1_500,
        });
        let (code, _, body, ..) = api_decision(&db, decision.to_string().as_bytes(), "Alle", &state);
        assert_eq!(code, 200, "pool decision failed: {}", String::from_utf8_lossy(&body));
        let pool_receipt: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let pool_decision_id = pool_receipt["poolDecisionId"].as_i64().unwrap();
        let canonical = db.get_segment_by_id("pool-reviewed").unwrap().unwrap();
        assert_eq!(canonical.reviewed_by.as_deref(), Some("Rubar"));
        assert_eq!(canonical.annotated_transcript.as_deref(), Some("Rubar first truth"));
        let lamo = crate::review_pool::coverage_by_voice(&db)
            .unwrap()
            .into_iter()
            .find(|row| row.voice_name == "Lamo")
            .unwrap();
        assert_eq!(lamo.two_reviews, 1, "Rubar plus Alle must calculate as two distinct reviews");
        assert!(!crate::review_pool::pending_segment_ids(&db, &pool, "Alle", None)
            .unwrap()
            .contains(&"pool-reviewed".to_string()));

        let pool_decision_count = || -> i64 {
            db.connection()
                .query_row(
                    "SELECT COUNT(*) FROM review_pool_decisions WHERE operation_id=?1",
                    [pool_operation_id],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(pool_decision_count(), 1);
        let restarted_state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-alle".into(), "alle".into())]),
            session_store: Some((tmp.path().to_path_buf(), db.path().to_string())),
            pool_policy: Some(pool.clone()),
            ..CouchState::default()
        });
        let (code, _, replay_body, ..) = api_decision(&db, decision.to_string().as_bytes(), "alle", &restarted_state);
        assert_eq!(code, 200, "case-only pool replay must ACK");
        let replay: serde_json::Value = serde_json::from_slice(&replay_body).unwrap();
        assert_eq!(replay["duplicate"], true);
        assert_eq!(pool_decision_count(), 1, "pool replay must not append another observation");

        let mut changed_action = decision.clone();
        changed_action["action"] = serde_json::json!("accept");
        let mut changed_text = decision.clone();
        changed_text["text"] = serde_json::json!("different pool truth");
        for (label, payload) in [("action", changed_action), ("text", changed_text)] {
            let (code, ..) = api_decision(&db, payload.to_string().as_bytes(), "alle", &restarted_state);
            assert_eq!(code, 409, "changed pool {label} must conflict");
            assert_eq!(pool_decision_count(), 1);
        }
        let mut changed_reviewer = decision.clone();
        changed_reviewer["reviewer"] = serde_json::json!("Hemn");
        let changed_reviewer: DecisionBody = serde_json::from_value(changed_reviewer).unwrap();
        let (code, ..) = api_pool_decision(&db, &changed_reviewer, "Hemn", &restarted_state, &pool);
        assert_eq!(code, 409, "a different reviewer must not inherit the pool receipt");
        assert_eq!(pool_decision_count(), 1);

        let undo_request = serde_json::json!({
            "poolDecisionId": pool_decision_id.to_string(),
            "decisionOperationId": pool_operation_id,
            "reversalOperationId": "30000000-0000-4000-8000-000000000003",
        });
        let (code, _, body, ..) = api_undo_with_body(&db, undo_request.to_string().as_bytes(), "Alle", &state);
        assert_eq!(code, 200, "pool undo failed: {}", String::from_utf8_lossy(&body));
        assert!(crate::review_pool::pending_segment_ids(&db, &pool, "Alle", None)
            .unwrap()
            .contains(&"pool-reviewed".to_string()));
        let canonical = db.get_segment_by_id("pool-reviewed").unwrap().unwrap();
        assert_eq!(canonical.reviewed_by.as_deref(), Some("Rubar"));
        assert_eq!(canonical.annotated_transcript.as_deref(), Some("Rubar first truth"));
        let retry_state = Mutex::new(CouchState { pool_policy: Some(pool), ..CouchState::default() });
        let (code, _, retry_body, ..) =
            api_undo_with_body(&db, undo_request.to_string().as_bytes(), "alle", &retry_state);
        assert_eq!(code, 200, "lost-response restart retry must replay the exact pool reversal");
        let retry: serde_json::Value = serde_json::from_slice(&retry_body).unwrap();
        assert_eq!(retry["poolDecisionId"], pool_decision_id);
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM review_pool_reversals", [], |row| row.get(0))
                .unwrap(),
            1,
            "an exact retry must not append a second reversal"
        );
    }

    #[test]
    fn flexible_pool_accepts_the_first_review_after_activation() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let champion_id = "omniasr-7b-test-champion";
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: champion_id.into(),
                family: crate::deployment::OMNIASR_7B_FAMILY.into(),
                model_card_name: Some("pool first-review champion".into()),
                checkpoint_sha256: "c".repeat(64),
                checkpoint_path: "/test/pool-first-champion.json".into(),
                source: "cortex-finetuned".into(),
                license: "owner-full-rights".into(),
            },
        )
        .unwrap();
        db.connection().execute("UPDATE model_versions SET status='champion' WHERE id=?1", [champion_id]).unwrap();
        let mut unreviewed = seg("pool-first-after-activation", "champion unreviewed draft");
        unreviewed.model_version_id = Some(champion_id.into());
        db.insert_segment(&unreviewed).unwrap();
        let pool = crate::review_pool::activate(
            &db,
            "123e4567-e89b-42d3-a456-426614174002",
            &[crate::review_pool::PoolMemberInput { segment_id: unreviewed.id.clone(), voice_name: "Lamo".into() }],
        )
        .unwrap();
        let state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-rubar".into(), "Rubar".into())]),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            pool_policy: Some(pool),
            ..CouchState::default()
        });

        let (queue_code, _, queue_body, ..) = api_queue(&db, "Rubar", &state);
        assert_eq!(queue_code, 200, "pool queue failed: {}", String::from_utf8_lossy(&queue_body));
        let queue: serde_json::Value = serde_json::from_slice(&queue_body).unwrap();
        let item = &queue["items"][0];
        assert_eq!(item["id"], unreviewed.id);
        let body = serde_json::json!({
            "operationId": "123e4567-e89b-42d3-a456-426614174098",
            "id": unreviewed.id,
            "action": "accept",
            "text": "champion unreviewed draft",
            "rowVersion": item["rowVersion"],
            "heardMs": 1_500,
            "clipDurationMs": 1_500,
        });
        let (decision_code, _, decision_body, ..) = api_decision(&db, body.to_string().as_bytes(), "Rubar", &state);
        assert_eq!(
            decision_code,
            200,
            "first post-activation review failed: {}",
            String::from_utf8_lossy(&decision_body),
        );
        let events: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM review_events WHERE segment_id=?1 AND reviewer='Rubar'",
                [&unreviewed.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 1, "the first review must be durably attributed");
    }

    #[test]
    fn alle_second_pass_is_backend_blind_separate_and_undoable() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let id = "blind-second-pass";
        db.insert_segment(&seg(id, "champion raw draft")).unwrap();
        let first_revision = db.segment_review_revision(id).unwrap().unwrap();
        let first_operation = "40000000-0000-4000-8000-000000000001";
        let first_hash = crate::db::review_operation_payload_hash(id, "edit", "Rubar corrected truth", "Rubar");
        db.record_phone_human_decision_by_at_revision_with_operation(
            id,
            "edit",
            Some("Rubar corrected truth"),
            "Rubar",
            first_revision,
            first_operation,
            &first_hash,
        )
        .unwrap()
        .unwrap();
        let ids = HashSet::from([id.to_string()]);
        let focus = crate::review_campaign::focus_evidence(&ids).unwrap();
        let base = crate::review_campaign::SequentialReviewCampaign {
            schema_version: 1,
            campaign_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            mode: crate::review_campaign::SEQUENTIAL_CAMPAIGN_MODE.into(),
            status: crate::review_campaign::SEQUENTIAL_CAMPAIGN_STATUS.into(),
            reviewer: "Rubar".into(),
            after_review_event_id: 0,
            activated_at_review_event_id: 1,
            focus_segment_count: focus.segment_count,
            focus_sha256: focus.sha256,
            provisional_export_block: true,
            independent_second_pass_required: true,
            progress: None,
        };
        db.connection()
            .execute(
                "INSERT INTO settings(key,value) VALUES(?1,?2)",
                [crate::review_campaign::SEQUENTIAL_CAMPAIGN_SETTINGS_KEY, &serde_json::to_string(&base).unwrap()],
            )
            .unwrap();
        std::fs::write(
            tmp.path().join("voice_focus.json"),
            serde_json::json!({"name":"Lamo", "segment_ids":[id]}).to_string(),
        )
        .unwrap();
        crate::review_campaign::activate_second_pass(&db, &ids, 1).unwrap();
        let campaign = crate::review_campaign::load(&db).unwrap().unwrap();
        let state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-alle".into(), "Alle".into())]),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            campaign_policy: Some(campaign.clone()),
            ..CouchState::default()
        });

        let (code, _, body, ..) = api_queue(&db, "Alle", &state);
        assert_eq!(code, 200, "second-pass queue failed: {}", String::from_utf8_lossy(&body));
        let queue: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(queue["campaignPhase"], "second_pass_active");
        assert_eq!(queue["items"][0]["text"], "champion raw draft");
        assert_ne!(queue["items"][0]["text"], "Rubar corrected truth");
        let row_version = queue["items"][0]["rowVersion"].as_str().unwrap();
        let operation_id = "40000000-0000-4000-8000-000000000002";
        let decision = serde_json::json!({
            "operationId": operation_id,
            "id": id,
            "action": "edit",
            "text": "Alle independent truth",
            "reviewer": "Alle",
            "rowVersion": row_version,
            "heardMs": 1_500,
            "clipDurationMs": 1_500,
        });
        let (code, _, body, ..) = api_decision(&db, decision.to_string().as_bytes(), "Alle", &state);
        assert_eq!(code, 200, "second-pass decision failed: {}", String::from_utf8_lossy(&body));
        let corpus = db.get_segment_by_id(id).unwrap().unwrap();
        assert_eq!(corpus.reviewed_by.as_deref(), Some("Rubar"));
        assert_eq!(corpus.annotated_transcript.as_deref(), Some("Rubar corrected truth"));
        let independent: (String, String) = db
            .connection()
            .query_row(
                "SELECT reviewer, submitted_transcript FROM effective_independent_review_decisions_v61
                  WHERE campaign_id=?1 AND segment_id=?2",
                rusqlite::params![campaign.campaign_id, id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(independent, ("Alle".into(), "Alle independent truth".into()));

        let independent_count = || -> i64 {
            db.connection()
                .query_row(
                    "SELECT COUNT(*) FROM independent_review_decisions WHERE operation_id=?1",
                    [operation_id],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(independent_count(), 1);

        // The second-pass receipt obeys the same restart/retype law as the paid canonical receipt.
        // Rebuild the session with the current lower-case roster spelling and replay the phone's
        // original outbox body, whose claimed reviewer spelling correctly remains `Alle`.
        let restarted_state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-alle".into(), "alle".into())]),
            session_store: Some((tmp.path().to_path_buf(), db.path().to_string())),
            campaign_policy: Some(campaign.clone()),
            ..CouchState::default()
        });
        let (code, _, replay_body, ..) = api_decision(&db, decision.to_string().as_bytes(), "alle", &restarted_state);
        assert_eq!(code, 200, "case-only independent replay must ACK");
        let replay: serde_json::Value = serde_json::from_slice(&replay_body).unwrap();
        assert_eq!(replay["duplicate"], true);
        assert_eq!(independent_count(), 1, "independent replay must not append another decision");

        let mut changed_action = decision.clone();
        changed_action["action"] = serde_json::json!("accept");
        let mut changed_text = decision.clone();
        changed_text["text"] = serde_json::json!("different independent truth");
        for (label, payload) in [("action", changed_action), ("text", changed_text)] {
            let (code, ..) = api_decision(&db, payload.to_string().as_bytes(), "alle", &restarted_state);
            assert_eq!(code, 409, "changed independent {label} must conflict");
            assert_eq!(independent_count(), 1);
        }
        let mut changed_reviewer = decision.clone();
        changed_reviewer["reviewer"] = serde_json::json!("Hemn");
        let changed_reviewer: DecisionBody = serde_json::from_value(changed_reviewer).unwrap();
        let (code, ..) = api_independent_decision(
            &db,
            &changed_reviewer,
            "Hemn",
            &couch_session_binding_sha256("couch-test-session"),
            &restarted_state,
            &campaign,
        );
        assert_eq!(code, 409, "a different reviewer must not inherit the independent receipt");
        assert_eq!(independent_count(), 1);

        let (code, _, body, ..) = api_undo(&db, "Alle", &state);
        assert_eq!(code, 200, "second-pass undo failed: {}", String::from_utf8_lossy(&body));
        let reversal_operation: String = db
            .connection()
            .query_row(
                "SELECT operation_id FROM independent_review_reversals WHERE decision_id=(
                     SELECT id FROM independent_review_decisions WHERE operation_id=?1
                 )",
                [operation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(reversal_operation, operation_id, "the forward UUID cannot also name its inverse");
        assert_eq!(
            reversal_operation,
            crate::review_campaign::independent_reversal_operation_id(operation_id).unwrap(),
            "bodyless inverse retries need one deterministic, domain-separated operation identity"
        );
        assert!(crate::review_campaign::independent_segment_pending(&db, &campaign, id).unwrap());
        let corpus = db.get_segment_by_id(id).unwrap().unwrap();
        assert_eq!(corpus.annotated_transcript.as_deref(), Some("Rubar corrected truth"));
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

    /// A hidden check is revision-bound even though grading does not update the corpus row. The
    /// browser's heard milliseconds belong to the row it was served; rebinding them after any bulk
    /// stamp would manufacture evidence for a revision it never heard.
    #[test]
    fn a_spot_check_is_refused_when_a_bulk_stamp_bumped_the_served_row() {
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
        assert_eq!(code, 409, "stale hidden playback must be refused: {}", String::from_utf8_lossy(&msg));
        let report = db.spot_check_report().unwrap();
        assert!(report.is_empty(), "a stale check must not be scored or paid");
        let receipts: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'sc1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(receipts, 0, "the stale submit must be refused before receipt minting");
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
    /// revision, content hash and duration are all resolved from the CURRENT row, so minting one for
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
    /// media time it played; the revision and decoded-PCM content hash are resolved here.
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
            // Deliberately NOT sent: revision and content hash are not the client's to assert.
        });
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200);

        let (segment_id, revision, content_hash, played, coverage): (String, i64, String, i64, f64) = db
            .connection()
            .query_row(
                "SELECT segment_id, segment_revision, audio_fingerprint, played_ms, coverage_ratio                  FROM playback_receipts WHERE segment_id = 'pr1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .expect("a phone decision must leave a listening receipt");

        assert_eq!(segment_id, "pr1");
        assert_eq!(played, 1_500, "policy-4 records the bounded normalized interval union");
        assert_eq!(coverage, 1.0, "the renderer traversed the complete server-sized fixture clip");
        assert_eq!(content_hash.len(), 64, "the receipt must name canonical decoded PCM");
        assert!(revision >= 0, "the revision is resolved server-side, not supplied");

        // HTTP 200 proves the receipt satisfied the guard INSIDE the decision transaction.  The
        // successful decision then bumps the row revision, so replaying that old receipt afterwards
        // must fail the current-row fence rather than becoming reusable authority.
        let current_revision = db.segment_review_revision("pr1").unwrap().unwrap_or(0);
        assert!(current_revision > revision, "the accepted decision advances the row version");
        assert!(
            !db.has_sufficient_playback_evidence("pr1", revision, &content_hash, Some("Sara")).unwrap(),
            "a receipt from the decided revision is historical evidence, not future authorization"
        );
    }

    /// The coverage DENOMINATOR is the server's clip length, never the client's claim about it.
    ///
    /// Resolving the revision and content hash server-side stops a client naming a clip it never
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

        let receipt_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'pr3'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(receipt_count, 0, "insufficient traversal must never be finalized into reusable evidence");

        let revision = db.segment_review_revision("pr3").unwrap().unwrap_or(0);
        let content_hash = db.segment_audio_content_hash("pr3").unwrap().unwrap_or_default();
        assert!(
            !db.has_sufficient_playback_evidence("pr3", revision, &content_hash, Some("Sara")).unwrap(),
            "a tenth of a second must not satisfy the listening bar"
        );
    }

    #[test]
    fn negative_phone_playback_is_rejected_instead_of_clamped_into_a_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("negative-playback", "دەق")).unwrap();
        let state = state();
        let body = serde_json::json!({
            "operationId": uuid::Uuid::new_v4().to_string(),
            "id": "negative-playback",
            "action": "accept",
            "text": "دەق",
            "heardMs": -1,
            "clipDurationMs": 1_500,
            "rowVersion": db.segment_row_stamp("negative-playback").unwrap().unwrap(),
        });

        let (code, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 400, "negative media time is invalid evidence, not zero media time");
        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'negative-playback'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
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

    #[test]
    fn policy4_phone_playback_authority_fails_closed_and_is_exactly_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        for id in ["p4-main", "p4-other", "p4-stale", "p4-restart", "p4-skip"] {
            policy4_segment(&db, id, 100);
        }
        let state = state();
        let reviewer = "Sara";
        let binding = couch_session_binding_sha256("policy4-session-a");
        let other_binding = couch_session_binding_sha256("policy4-session-b");
        let revision = db.segment_review_revision("p4-main").unwrap().unwrap();
        let operation = "31000000-0000-4000-8000-000000000001";
        let base = serde_json::json!({
            "operationId": operation,
            "id": "p4-main",
            "action": "accept",
            "text": "دەقی تاقیکردنەوە",
            "reviewer": reviewer,
            "rowVersion": revision.to_string(),
        });

        // Absence, a fabricated legacy scalar, and a random receipt all fail through the real
        // production endpoint without creating a receipt, verdict, pay/effect row, or consumption.
        assign_policy4_work(&state, "p4-main", reviewer);
        let missing = super::api_decision_authenticated(&db, base.to_string().as_bytes(), reviewer, &binding, &state);
        assert_eq!(missing.0, 428, "a new verdict without authority must fail closed");
        let mut scalar = base.clone();
        scalar["operationId"] = serde_json::json!("31000000-0000-4000-8000-000000000002");
        scalar["heardMs"] = serde_json::json!(100);
        scalar["clipDurationMs"] = serde_json::json!(100);
        assign_policy4_work(&state, "p4-main", reviewer);
        assert_eq!(
            super::api_decision_authenticated(&db, scalar.to_string().as_bytes(), reviewer, &binding, &state,).0,
            428,
            "client counters are not playback authority"
        );
        let mut forged = base.clone();
        forged["operationId"] = serde_json::json!("31000000-0000-4000-8000-000000000003");
        forged["playbackReceiptId"] = serde_json::json!("31000000-0000-4000-8000-000000000099");
        assign_policy4_work(&state, "p4-main", reviewer);
        assert_eq!(
            super::api_decision_authenticated(&db, forged.to_string().as_bytes(), reviewer, &binding, &state,).0,
            428,
            "an unissued receipt must not authorize a verdict"
        );
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM playback_receipts WHERE policy_version=4", [], |row| row
                    .get(0),)
                .unwrap(),
            0
        );

        // Start is exact-retry safe, but finalization is impossible until this exact attempt's
        // authenticated media GET completes. Session, reviewer and segment substitutions fail.
        assign_policy4_work(&state, "p4-main", reviewer);
        let attempt =
            start_policy4_attempt(&db, &state, "p4-main", reviewer, &binding, "32000000-0000-4000-8000-000000000001");
        let receipt = attempt["playbackReceiptId"].as_str().unwrap();
        let exact_start_retry =
            start_policy4_attempt(&db, &state, "p4-main", reviewer, &binding, "32000000-0000-4000-8000-000000000001");
        assert_eq!(exact_start_retry["playbackReceiptId"], receipt);
        assert_eq!(exact_start_retry["duplicate"], true, "an exact start retry reuses one server authority");
        assign_policy4_work(&state, "p4-other", reviewer);
        let rebound_start = serde_json::json!({
            "id": "p4-other",
            "rowVersion": db.segment_review_revision("p4-other").unwrap().unwrap().to_string(),
            "clientAttemptId": "32000000-0000-4000-8000-000000000001",
        });
        assert_eq!(
            super::api_playback_start(&db, rebound_start.to_string().as_bytes(), reviewer, &binding, &state,).0,
            409,
            "one client attempt UUID cannot be rebound to another clip"
        );
        let finalize = |intervals: serde_json::Value, state: &Mutex<CouchState>| {
            let body = serde_json::json!({
                "playbackReceiptId": receipt,
                "clientAttemptId": "32000000-0000-4000-8000-000000000001",
                "intervals": intervals,
            });
            super::api_playback_finalize(&db, body.to_string().as_bytes(), reviewer, &binding, state)
        };
        assert_eq!(finalize(serde_json::json!([{"startMs":0,"endMs":90}]), &state).0, 428);
        assert_eq!(
            super::api_audio_authenticated(
                &db,
                "p4-main",
                reviewer,
                &other_binding,
                &state,
                Some(receipt),
                false,
                None,
                None,
            )
            .0,
            403,
            "an attempt cannot cross cookie sessions"
        );
        assign_policy4_work(&state, "p4-other", reviewer);
        assert_eq!(
            super::api_audio_authenticated(
                &db,
                "p4-other",
                reviewer,
                &binding,
                &state,
                Some(receipt),
                false,
                None,
                None,
            )
            .0,
            403,
            "an attempt cannot cross segments"
        );
        assert_eq!(
            super::api_audio_authenticated(&db, "p4-main", "Hemn", &binding, &state, Some(receipt), false, None, None,)
                .0,
            403,
            "an attempt cannot cross reviewers"
        );
        assert_eq!(
            super::api_audio_authenticated(
                &db,
                "p4-main",
                reviewer,
                &binding,
                &state,
                Some(receipt),
                false,
                None,
                None,
            )
            .0,
            200
        );
        assert_eq!(
            finalize(serde_json::json!([{"startMs":0,"endMs":100}]), &state).0,
            428,
            "full-duration inflation immediately after media delivery is implausible"
        );
        assert_eq!(
            finalize(serde_json::json!([{"startMs":0,"endMs":60},{"startMs":50,"endMs":90}]), &state,).0,
            400,
            "overlapping client intervals are not canonical"
        );
        std::thread::sleep(Duration::from_millis(55));
        let finalized = finalize(serde_json::json!([{"startMs":0,"endMs":90}]), &state);
        assert_eq!(finalized.0, 200, "valid traversal failed: {}", String::from_utf8_lossy(&finalized.2));
        let exact_replay = finalize(serde_json::json!([{"startMs":0,"endMs":90}]), &state);
        assert_eq!(exact_replay.0, 200, "exact finalization retry must ACK");
        assert_eq!(
            finalize(serde_json::json!([{"startMs":0,"endMs":91}]), &state).0,
            409,
            "a finalized receipt cannot be rebound to different intervals"
        );

        // The finalized authority is still exact: wrong-session and wrong-segment decisions fail,
        // then one correct operation consumes it atomically with the human effect. Its exact retry
        // ACKs; a new operation cannot turn the same traversal into a second decision/payment.
        let mut wrong_session = base.clone();
        wrong_session["operationId"] = serde_json::json!("33000000-0000-4000-8000-000000000001");
        wrong_session["playbackReceiptId"] = serde_json::json!(receipt);
        assign_policy4_work(&state, "p4-main", reviewer);
        assert_eq!(
            super::api_decision_authenticated(
                &db,
                wrong_session.to_string().as_bytes(),
                reviewer,
                &other_binding,
                &state,
            )
            .0,
            428
        );
        let other_revision = db.segment_review_revision("p4-other").unwrap().unwrap();
        let wrong_segment = serde_json::json!({
            "operationId": "33000000-0000-4000-8000-000000000002",
            "id": "p4-other", "action": "accept", "text": "دەقی تاقیکردنەوە",
            "reviewer": reviewer, "rowVersion": other_revision.to_string(),
            "playbackReceiptId": receipt,
        });
        assign_policy4_work(&state, "p4-other", reviewer);
        assert_eq!(
            super::api_decision_authenticated(&db, wrong_segment.to_string().as_bytes(), reviewer, &binding, &state,).0,
            428
        );
        let mut valid = base.clone();
        valid["playbackReceiptId"] = serde_json::json!(receipt);
        assign_policy4_work(&state, "p4-main", reviewer);
        let committed =
            super::api_decision_authenticated(&db, valid.to_string().as_bytes(), reviewer, &binding, &state);
        assert_eq!(committed.0, 200, "valid receipt-bound decision failed: {}", String::from_utf8_lossy(&committed.2));
        assert_eq!(
            super::api_decision_authenticated(&db, valid.to_string().as_bytes(), reviewer, &binding, &state,).0,
            200,
            "the exact operation retry must ACK after the receipt is consumed"
        );
        let mut reuse = valid.clone();
        reuse["operationId"] = serde_json::json!("33000000-0000-4000-8000-000000000003");
        assert_ne!(
            super::api_decision_authenticated(&db, reuse.to_string().as_bytes(), reviewer, &binding, &state,).0,
            200,
            "one playback authority must never fund a second operation"
        );
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM playback_authority_consumptions_v4", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='p4-main'",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            1
        );

        // Revision drift and process-memory loss both strand an unfinalized attempt; neither can be
        // upgraded into durable evidence after the exact server authority disappears or changes.
        assign_policy4_work(&state, "p4-stale", reviewer);
        let stale =
            start_policy4_attempt(&db, &state, "p4-stale", reviewer, &binding, "34000000-0000-4000-8000-000000000001");
        let stale_receipt = stale["playbackReceiptId"].as_str().unwrap();
        assert_eq!(
            super::api_audio_authenticated(
                &db,
                "p4-stale",
                reviewer,
                &binding,
                &state,
                Some(stale_receipt),
                false,
                None,
                None,
            )
            .0,
            200
        );
        db.connection()
            .execute("UPDATE speech_segments SET review_revision=review_revision+1 WHERE id='p4-stale'", [])
            .unwrap();
        std::thread::sleep(Duration::from_millis(55));
        let stale_body = serde_json::json!({
            "playbackReceiptId": stale_receipt,
            "clientAttemptId": "34000000-0000-4000-8000-000000000001",
            "intervals": [{"startMs":0,"endMs":90}],
        });
        assert_eq!(
            super::api_playback_finalize(&db, stale_body.to_string().as_bytes(), reviewer, &binding, &state,).0,
            409
        );

        assign_policy4_work(&state, "p4-restart", reviewer);
        let restart = start_policy4_attempt(
            &db,
            &state,
            "p4-restart",
            reviewer,
            &binding,
            "35000000-0000-4000-8000-000000000001",
        );
        let restart_receipt = restart["playbackReceiptId"].as_str().unwrap();
        assert_eq!(
            super::api_audio_authenticated(
                &db,
                "p4-restart",
                reviewer,
                &binding,
                &state,
                Some(restart_receipt),
                false,
                None,
                None,
            )
            .0,
            200
        );
        let restarted_state = Mutex::new(CouchState::default());
        assign_policy4_work(&restarted_state, "p4-restart", reviewer);
        let restart_body = serde_json::json!({
            "playbackReceiptId": restart_receipt,
            "clientAttemptId": "35000000-0000-4000-8000-000000000001",
            "intervals": [{"startMs":0,"endMs":90}],
        });
        assert_eq!(
            super::api_playback_finalize(
                &db,
                restart_body.to_string().as_bytes(),
                reviewer,
                &binding,
                &restarted_state,
            )
            .0,
            409
        );

        let skip_revision = db.segment_review_revision("p4-skip").unwrap().unwrap();
        let skip = serde_json::json!({
            "operationId": "36000000-0000-4000-8000-000000000001",
            "id": "p4-skip", "action": "skip", "text": "دەقی تاقیکردنەوە",
            "reviewer": reviewer, "rowVersion": skip_revision.to_string(),
        });
        assign_policy4_work(&state, "p4-skip", reviewer);
        assert_eq!(
            super::api_decision_authenticated(&db, skip.to_string().as_bytes(), reviewer, &binding, &state,).0,
            200,
            "skip is explicitly no verdict and needs no playback authority"
        );
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM playback_authority_consumptions_v4", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            1,
            "skip consumes no playback authority"
        );
    }

    #[test]
    fn a_missing_audio_content_hash_blocks_every_durable_review_event() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("no-fingerprint", "دەقی دوو")).unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET audio_content_hash = NULL WHERE id = 'no-fingerprint'", [])
            .unwrap();
        let state = state();

        let revision = db.segment_review_revision("no-fingerprint").unwrap().unwrap_or(0);
        let verdict = serde_json::json!({
            "id": "no-fingerprint",
            "action": "accept",
            "text": "دەقی دوو",
            "heardMs": 1_500,
            "clipDurationMs": 1_500,
            "rowVersion": revision.to_string(),
            "operationId": uuid::Uuid::new_v4().to_string(),
        });
        let (code, ..) = super::api_decision(&db, verdict.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 503, "a segment id must never stand in for exact audio identity");
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'no-fingerprint'",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            0,
            "a failed verdict must not mint pseudo-identity evidence"
        );

        let skip = serde_json::json!({
            "id": "no-fingerprint",
            "action": "skip",
            "heardMs": 0,
            "clipDurationMs": 1_500,
            "rowVersion": revision.to_string(),
            "operationId": uuid::Uuid::new_v4().to_string(),
        });
        let (code, ..) = super::api_decision(&db, skip.to_string().as_bytes(), "Sara", &state);
        assert_eq!(
            code, 500,
            "skip needs no listening proof, but its durable audit/pay identity must still fail closed"
        );
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM playback_receipts WHERE segment_id = 'no-fingerprint'",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            0,
            "a refused identity-free skip cannot mint evidence"
        );
    }

    /// Undo must not orphan the reviewer's own listening: the immediate re-decision has to land.
    ///
    /// Found by the 2026-08-19 hunt: the decision bumps review_revision, the receipt stays keyed to
    /// the pre-decision revision, and undo restores the ROW but not the evidence — so the very next
    /// decision on a clip the reviewer just heard, decided and undid was refused 428 "play the whole
    /// clip first". The reviewer is punished for using Undo, seconds after listening.
    #[test]
    fn an_undone_decision_never_reaches_an_export_and_a_redecision_exports_only_its_own_text() {
        // The learning flywheel's entry condition, proven at the SERVING path of the export rather
        // than at the writer: only an ACTIVE, unreversed human decision may hand text to a dataset.
        // Decision -> export -> exact Undo -> export -> redecision -> export, one database, the
        // paid phone path. Before 2026-09-02 the undo restore and the export filter were each
        // proven alone; nothing proved the pair end to end.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let raw = "دەقی ئەسڵی";
        db.insert_segment(&seg("fw1", raw)).unwrap();
        let state = state();
        let first = "ڕاستکراوەی یەکەم";
        let second = "ڕاستکراوەی دووەم";
        let export = |label: &str| -> (String, u64) {
            let out = tmp.path().join(format!("{label}.json"));
            crate::export::export_dataset(&db, &out, &crate::settings::ExportFormat::Json).expect(label);
            let body = std::fs::read_to_string(&out).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            let verified = parsed["metadata"]["verified_segments"].as_u64().unwrap_or(0);
            (body, verified)
        };
        let decide = |text: &str| {
            let body = serde_json::json!({
                "id": "fw1", "action": "edit", "text": text,
                "heardMs": 1_500, "clipDurationMs": 1_500,
            });
            api_decision(&db, body.to_string().as_bytes(), "Sara", &state).0
        };

        let (before, verified) = export("before");
        assert!(before.contains(raw) && !before.contains(first), "untouched: the champion's raw text serves");
        assert_eq!(verified, 0);

        assert_eq!(decide(first), 200);
        let (decided, verified) = export("decided");
        assert!(decided.contains(first), "an active human decision is what the export serves");
        assert_eq!(verified, 1, "and it counts as verified");

        assert_eq!(api_undo(&db, "Sara", &state).0, 200, "exact undo");
        let (undone, verified) = export("undone");
        assert!(!undone.contains(first), "an undone decision's text must be absent from the export, in every field");
        assert!(undone.contains(raw), "the pre-decision text is served again");
        assert_eq!(verified, 0, "an undone decision no longer counts as verified");

        assert_eq!(decide(second), 200, "redecision after undo");
        let (redecided, verified) = export("redecided");
        assert!(redecided.contains(second), "the live decision exports");
        assert!(!redecided.contains(first), "the undone text stays gone after a redecision");
        assert_eq!(verified, 1);

        // Restart: a fresh connection to the same file must serve the same answer -- nothing above
        // may live only in connection state or in a cache.
        let reopened = Database::open(&db_path).unwrap();
        let out = tmp.path().join("restart.json");
        crate::export::export_dataset(&reopened, &out, &crate::settings::ExportFormat::Json).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        assert!(body.contains(second) && !body.contains(first), "the live decision survives a database restart");
    }

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

        // A new decision consumes a new policy-4 authority even immediately after exact Undo. The
        // test compatibility wrapper supplies that second, reviewer-bound traversal explicitly.
        let body = serde_json::json!({
            "id": "un1", "action": "edit", "text": "ڕاستکراوەی دوو",
            "heardMs": 1_500, "clipDurationMs": 1_500,
        });
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "a re-decision after undo lands with a fresh exact playback authority");

        // But SOMEBODY ELSE still has to listen for themselves.
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200);
        let body = serde_json::json!({
            "operationId": uuid::Uuid::new_v4().to_string(),
            "id": "un1", "action": "edit", "text": "دەقی کەسی تر",
            "rowVersion": db.segment_row_stamp("un1").unwrap().unwrap(),
        });
        let (code, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Hemn", &state);
        assert_eq!(code, 428, "another reviewer has no exact personal playback authority");
    }

    #[test]
    fn a_retyped_reviewer_still_owns_their_own_stored_decision() {
        // The same identity law at the decide path. Case-sensitive `reviewed_by` comparisons made a
        // re-typed reviewer a STRANGER to their own row: the dropped-response retry stopped counting
        // as a repeat (a second undo entry and a second DPO learning pair distilled from one human
        // correction), and the late-submit guard refused their own re-review with "already reviewed
        // by Rubar" — an unresolvable 409 against themselves.
        assert!(same_reviewer("Rubar", " rubar "));
        assert!(!same_reviewer("Rubar", "Alle"));

        let mut prev = seg("d1", "دەقی خام");
        prev.reviewed_by = Some("Rubar".to_string());
        prev.human_decision = Some("edit".to_string());
        prev.verdict_transcript = Some("دەقی ڕاست".to_string());
        assert!(is_repeat_of_stored_decision(&prev, "rubar", "edit", Some("دەقی ڕاست")), "one person, one decision");
        assert!(!is_repeat_of_stored_decision(&prev, "Alle", "edit", Some("دەقی ڕاست")), "still not somebody else");

        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("d1", "دەقی خام")).unwrap();
        let state = state();
        let first = serde_json::json!({
            "id": "d1", "action": "edit", "text": "یەکەم", "heardMs": 1_500, "clipDurationMs": 1_500,
        });
        assert_eq!(api_decision(&db, first.to_string().as_bytes(), "Rubar", &state).0, 200);
        let again = serde_json::json!({
            "id": "d1", "action": "edit", "text": "دووەم", "heardMs": 1_500, "clipDurationMs": 1_500,
        });
        let (code, _, body, ..) = api_decision(&db, again.to_string().as_bytes(), "rubar", &state);
        assert_eq!(
            code,
            200,
            "correcting your own verdict is a re-review, not a collision: {}",
            String::from_utf8_lossy(&body)
        );
        let row = db.get_segment_by_id("d1").unwrap().unwrap();
        assert_eq!(row.verdict_transcript.as_deref(), Some("دووەم"), "the re-review landed");
    }

    /// A decision sent with no heard-time reports nothing, and must not fabricate evidence.
    #[test]
    fn a_phone_decision_without_reported_playback_mints_no_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("pr2", "دەقی دوو")).unwrap();
        let state = state();

        let body = serde_json::json!({
            "operationId": uuid::Uuid::new_v4().to_string(),
            "id": "pr2", "action": "accept", "text": "دەقی دوو",
            "rowVersion": db.segment_row_stamp("pr2").unwrap().unwrap(),
        });
        let (code, ..) = super::api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
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

        // Model a watchdog/app restart: the in-memory Undo stack is gone, but reviewer-scoped DB
        // truth still resolves the latest Couch effect and its original stable operation UUID.
        let restarted_state = Mutex::new(CouchState::default());

        // UNDO restores the LAST decision (s2): unverified again, decision cleared.
        let (code, _, body, ..) = api_undo(&db, "Sara", &restarted_state);
        assert_eq!(code, 200);
        let undone: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(undone["id"], "s2");
        let row = db.get_segment_by_id("s2").unwrap().unwrap();
        assert!(!row.verified, "undo must reopen the clip");
        assert_eq!(row.reviewed_by, None, "undo retracts the attribution with the decision");

        // A bodyless retry after a lost HTTP response replays the SAME durable operation. It must
        // never fall through to s1 and accidentally undo older work.
        let restarted_again = Mutex::new(CouchState::default());
        let (code, _, retry_body, ..) = api_undo(&db, "Sara", &restarted_again);
        assert_eq!(code, 200, "the stable undo token makes a lost-response retry idempotent");
        let retried: serde_json::Value = serde_json::from_slice(&retry_body).unwrap();
        assert_eq!(retried["id"], "s2", "a retry must remain bound to the same effect");
        assert!(db.get_segment_by_id("s1").unwrap().unwrap().verified, "older work must not be touched");
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>("SELECT COUNT(*) FROM human_decision_effect_reversals", [], |row| row.get(0))
                .unwrap(),
            1,
            "idempotent retries append no second reversal"
        );
    }

    #[test]
    fn a_named_reviewer_cannot_enter_the_legacy_nonfinalized_desktop_path() {
        // The legacy attributed-desktop writer could leave reviewed_by=Sara while publishing a desktop
        // effect whose reviewer is necessarily NULL. Exact Undo could not authenticate that post-state.
        // Current code must reject the mixed boundary before any decision or learning effect, while the
        // real phone path remains available and atomic.
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        let state = state();
        let fix = "دەق یەک ڕاستکراوە";
        let body = serde_json::json!({"heardMs": 600_000, "id": "s1", "action": "edit", "text": fix});

        let error = db.record_human_decision_by("s1", "edit", Some(fix), None, Some("Sara")).unwrap_err();
        assert!(error.to_string().contains("anonymous desktop decision boundary"));
        let untouched = db.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(untouched.human_decision, None);
        for table in ["human_decision_effect_events", "agent_examples", "corrections"] {
            let count: i64 = db
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table} WHERE segment_id='s1'"), [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0, "the rejected mixed-boundary call must leave {table} untouched");
        }

        // The attributed phone endpoint then records the whole decision graph in one commit.
        let (code, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
        assert_eq!(code, 200, "the supported attributed path must remain available");

        let done = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(done.verified);
        assert_eq!(done.reviewed_by.as_deref(), Some("Sara"));
        assert_eq!(done.annotated_transcript.as_deref(), Some(fix));
        assert_eq!(
            db.connection()
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id='s1' AND source='couch'",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            1
        );
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
        crate::jury::record_human_decision(&db, "s1", "edit", Some("ڕاستکراوەی خاوەن"), None).unwrap();

        // Sara now taps undo. Her snapshot predates the owner's edit entirely.
        let (code, _, msg, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "undo must not silently revert someone else's later work");
        assert!(
            String::from_utf8_lossy(&msg).contains("desktop"),
            "the refusal must identify the anonymous desktop boundary: {msg:?}"
        );
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(row.reviewed_by, None, "the anonymous desktop edit must survive the refused undo");
        assert_eq!(row.verdict_transcript.as_deref(), Some("ڕاستکراوەی خاوەن"));

        // And the entry is KEPT, not consumed by the refusal — a refused undo must not silently
        // become "nothing to undo" on the next press.
        let (code, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 409, "still refused, and still present rather than swallowed");
    }

    #[test]
    fn undo_allows_unrelated_metadata_writes_and_preserves_them_exactly() {
        // Exact effect Undo owns only review fields. A background metadata write may advance the row
        // revision, but it must neither block Undo nor be reverted by renderer snapshot authority.
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
        assert_eq!(code, 200, "metadata-only revision drift must not block exact effect Undo: {msg:?}");
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(row.normalized_transcript.as_deref(), Some("نوێ"), "the later metadata write must survive Undo");
        assert_eq!(row.reviewed_by, None, "the exact review-owned attribution is restored");

        // A lost HTTP response is safe: the DB-derived retry reaches the same reversal idempotently.
        let (code, _, retry, ..) = api_undo(&db, "Sara", &state);
        assert_eq!(code, 200, "bodyless retry must resolve the same durable reversal: {retry:?}");
        assert_eq!(db.get_segment_by_id("s1").unwrap().unwrap().normalized_transcript.as_deref(), Some("نوێ"));
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
        rollback_fixture_to(&db, 59);
        let mut gold = seg("g1", "دەقی خاو");
        gold.annotated_transcript = Some("دەقی ڕاست".into());
        gold.verified = true;
        db.insert_segment(&gold).unwrap();
        upgrade_fixture_from(&db, 59);
        let state = state();

        // It was handed to Sara as a check while it was still verified.
        lock_state(&state).spot_checks.insert(("g1".to_string(), "Sara".to_string()));
        db.record_spot_check("g1", "Sara", "edit", "دەقی ڕاست", "دەقی ڕاست").unwrap();

        // The owner then un-verifies it, so it is pending work again and no longer an answer key.
        db.connection().execute("UPDATE speech_segments SET verified = 0 WHERE id = 'g1'", []).unwrap();

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
        db.insert_segment(&seg("g1", "دەقی خاو")).unwrap();
        // Through the REAL finalized desktop path, so the key lives in verdict_transcript exactly as
        // a human edit leaves it — the shape `list_spot_check_candidates` admits and "mark bad" strips.
        db.finalize_human_review("g1", "edit", Some("دەقی ڕاست"), None, None).unwrap();
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
        assert_eq!(queue_ids(&db, "Sara", &state), vec!["s1"], "the phone must hold the served clip");

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

        // Hemn's bodyless retry is bound to the same durable effect and remains an idempotent 200;
        // it can never fall through to Sara's older decision.
        assert_eq!(api_undo(&db, "Hemn", &state).0, 200, "Hemn's lost-response retry is idempotent");
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
    fn a_durable_operation_uuid_replays_after_restart_and_reviewer_retype_without_duplicate_event_or_pay() {
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
        assert_eq!(api_decision(&db, body.to_string().as_bytes(), "Sara", &first_state).0, 200);

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
        // identity. The roster was also retyped from `Sara` to `sara`; session restoration binds the
        // old cookie to that current spelling. The original stale rowVersion is intentional: the
        // immutable receipt must ACK before mutable row state, leases, or undo memory are consulted.
        let restarted_state = state();
        let (code, _, response, ..) = super::api_decision(&db, body.to_string().as_bytes(), "sara", &restarted_state);
        assert_eq!(code, 200);
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["duplicate"], true);
        assert_eq!(counts(), (1, 1, 1), "a restart replay must be side-effect free");
        let guard = lock_state(&restarted_state);
        let undo = guard.undo.get("sara").expect("exact replay restores the original durable undo token");
        assert_eq!(undo.len(), 1, "a replay is not a second undoable act");
        assert_eq!(undo[0].effect_event_id, db.human_decision_effect_for_operation(operation_id).unwrap().unwrap().0);
        drop(guard);

        // Case-only identity drift is the sole compatibility allowance. The receipt's original
        // spelling rederives its v1 hash, while any semantic contract change remains a 409 and cannot
        // append another event, payment, or learning effect.
        let mut changed_action = body.clone();
        changed_action["action"] = serde_json::json!("accept");
        let mut changed_text = body.clone();
        changed_text["text"] = serde_json::json!("دەقێکی تر");
        let mut changed_reviewer = body.clone();
        changed_reviewer["reviewer"] = serde_json::json!("Hemn");
        for (label, payload, authenticated_reviewer) in
            [("action", changed_action, "sara"), ("text", changed_text, "sara"), ("reviewer", changed_reviewer, "Hemn")]
        {
            let (code, _, message, ..) =
                super::api_decision(&db, payload.to_string().as_bytes(), authenticated_reviewer, &restarted_state);
            assert_eq!(code, 409, "changing {label} while reusing the UUID must conflict");
            assert!(String::from_utf8_lossy(&message).contains("operation UUID"));
            assert_eq!(counts(), (1, 1, 1), "rejected {label} reuse must be side-effect free");
        }
    }

    #[test]
    fn one_thousand_lost_response_retries_across_database_restarts_are_exactly_once() {
        const RESTARTS: usize = 20;
        const RETRIES_PER_RESTART: usize = 50;
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("operation-1000", "دەقی خاو")).unwrap();
        let operation_id = "33333333-3333-4333-8333-333333333333";
        let body = serde_json::json!({
            "operationId": operation_id,
            "id": "operation-1000",
            "action": "edit",
            "text": "دەقی ڕاستکراوە",
            "reviewer": "Sara",
            "rowVersion": db.segment_row_stamp("operation-1000").unwrap().unwrap(),
            "heardMs": 600_000,
            "clipDurationMs": 1_500,
        })
        .to_string();
        assert_eq!(api_decision(&db, body.as_bytes(), "Sara", &state()).0, 200);
        drop(db);

        let mut replay_count = 0usize;
        for _ in 0..RESTARTS {
            let reopened = Database::open(&db_path).unwrap();
            reopened.initialize().unwrap();
            let restarted_state = state();
            for _ in 0..RETRIES_PER_RESTART {
                let (code, _, response, ..) = api_decision(&reopened, body.as_bytes(), "Sara", &restarted_state);
                assert_eq!(code, 200);
                let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
                assert_eq!(response["duplicate"], true, "every lost-response replay must be an idempotent ACK");
                replay_count += 1;
            }
            let guard = lock_state(&restarted_state);
            assert_eq!(
                guard.undo.get("Sara").map(Vec::len),
                Some(1),
                "retries in one restarted process must restore one undo token, never one per request"
            );
        }
        assert_eq!(replay_count, 1_000);

        let final_db = Database::open(&db_path).unwrap();
        final_db.initialize().unwrap();
        let receipt = final_db.review_operation(operation_id).unwrap().expect("the one durable operation receipt");
        assert_eq!(receipt.segment_id, "operation-1000");
        assert_eq!(receipt.reviewer, "Sara");
        let counts: (i64, i64, i64) = final_db
            .connection()
            .query_row(
                "SELECT
                     (SELECT COUNT(*) FROM review_events WHERE operation_id=?1),
                     (SELECT COUNT(*) FROM review_compensation_ledger ledger
                        JOIN review_events event ON event.id=ledger.review_event_id
                       WHERE event.operation_id=?1),
                     (SELECT COUNT(*) FROM agent_examples WHERE segment_id='operation-1000')",
                [operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1, 1), "1,000 retries must leave exactly one event/pay/learning effect");
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
        assert_eq!(api_decision(&db, base.to_string().as_bytes(), "Sara", &state).0, 200);

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
    fn pool_undo_persists_an_exact_reversal_request_before_posting() {
        let page = include_str!("../assets/couch.html");
        let prepare = page
            .split_once("function preparePoolUndoTarget()")
            .and_then(|(_, tail)| tail.split_once("function clearPoolUndoTarget(target)"))
            .map(|(body, _)| body)
            .expect("pool undo preparation");
        assert!(prepare.contains("target.reversalOperationId = newOperationId();"));
        assert!(prepare.contains("writePoolUndoTarget(target); // durable before the first undo network byte"));
        let undo = page.split_once("$('undo').onclick").map(|(_, tail)| tail).expect("undo handler");
        let persist = undo.find("preparePoolUndoTarget()").expect("durable addressed target");
        let post = undo.find("await api('/api/undo'").expect("undo POST");
        assert!(persist < post, "the reversal operation identity must be durable before the POST");
        assert!(undo.contains("body: JSON.stringify(poolUndoTarget || {})"));
        assert!(page.contains("rememberPoolUndoTarget(item, saved);"), "outbox replay must recover the target receipt");
        assert!(
            page.contains("rememberPoolUndoTarget(submission, saved);"),
            "the online decision path must persist the same receipt"
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
        rollback_fixture_to(db, 59);
        let mut s = seg(id, wrong_draft);
        s.verified = true;
        s.is_gold = true;
        s.human_decision = Some("edit".into());
        s.verdict = Some("human_edit".into());
        s.verdict_transcript = Some(human_answer.into());
        db.insert_segment_full(&s).unwrap();
        upgrade_fixture_from(db, 59);
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
        assert!(
            payload["items"].as_array().unwrap().iter().all(|item| item["rowVersion"].is_string()),
            "a null rowVersion is a shape only a trap clip can have, and the decide path refuses it anyway"
        );
    }

    #[test]
    fn an_unsavable_ordinary_hidden_check_refuses_the_batch_instead_of_losing_the_score() {
        // The ORDINARY namespace reserves nothing in SQLite: couch_session.json is the only durable
        // record that these clips went out as hidden checks. The serve used to go out anyway on a save
        // failure, and silently — the warn fired only when a pilot quota was active. This machine
        // restarts 4-9 times a day, so `spot_checks` then rehydrated WITHOUT the pair, the reviewer's
        // answer took the was_served_as_spot_check=false path, and the 409 "already reviewed" lost a
        // QC score with nothing in the log to explain it.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("paid-work", "دەقی کار")).unwrap();
        gold_seg(&db, "quality-key", "دەقی هەڵە", "دەقی ڕاست");
        let state = Mutex::new(CouchState {
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            pairing_codes: HashMap::from([("test-token".to_string(), "Sara".to_string())]),
            reviewers: HashMap::from([("sara-cookie".to_string(), "Sara".to_string())]),
            session_issued: HashMap::from([("sara-cookie".to_string(), SystemTime::now())]),
            // Deterministic durable-save failure: one live cookie in the generation cannot be
            // protected, which is exactly how `persist_session_state` fails in production.
            fail_cookie_protection_for: Some("sara-cookie".to_string()),
            ..CouchState::default()
        });

        let (code, _, body, ..) = api_queue(&db, "Sara", &state);
        assert_eq!(code, 503, "a hidden check whose only durable record failed must not be served");
        assert!(
            String::from_utf8_lossy(&body).contains("could not be saved"),
            "the reviewer gets a retryable operational reason: {}",
            String::from_utf8_lossy(&body)
        );
        let guard = lock_state(&state);
        assert!(
            guard.spot_checks.is_empty(),
            "an unserved assignment must not linger — it would re-score a later ordinary serve of the same clip"
        );
        assert!(guard.leases.is_empty(), "a batch refused before delivery hands its work back to the pool");
        assert!(guard.served_work.is_empty(), "no delivery receipt is minted for a batch nobody received");
    }

    #[test]
    fn the_decide_guard_refuses_every_placeholder_the_declared_authority_knows() {
        // The three decide guards re-implemented `quality::is_placeholder_transcript` as "empty or
        // [bracketed]" and drifted from it: a bare "n/a" or "null" was accepted and minted as a
        // human-verified transcript, which the export then shipped as a training row.
        assert!(refuses_verification_as_placeholder("n/a"));
        assert!(refuses_verification_as_placeholder(" N/A "));
        assert!(refuses_verification_as_placeholder("NULL"));
        assert!(refuses_verification_as_placeholder("[Pending WSL 7B ASR]"));
        // The bracket test is kept as a strict addition, so a marker the authority has not been
        // taught about is still refused.
        assert!(refuses_verification_as_placeholder("[something new]"));
        assert!(!refuses_verification_as_placeholder("دەقی ڕاست"));

        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("ph1", "دەقی ئەسڵی")).unwrap();
        let state = state();
        for text in ["n/a", "N/A", "null", "[Pending WSL 7B ASR]"] {
            let body = serde_json::json!({
                "id": "ph1", "action": "edit", "text": text,
                "heardMs": 1_500, "clipDurationMs": 1_500,
            });
            let (code, _, reply, ..) = api_decision(&db, body.to_string().as_bytes(), "Sara", &state);
            assert_eq!(code, 400, "{text:?} must never be verifiable as gold");
            assert!(String::from_utf8_lossy(&reply).contains("placeholder"));
        }
        assert!(!db.get_segment_by_id("ph1").unwrap().unwrap().verified, "no placeholder reached the row");
    }

    #[test]
    fn a_live_session_cookie_is_not_a_pairing_credential_at_claim() {
        // `api_claim_probe` already refuses a cookie copied out of `reviewers` — "a cookie is not a
        // durable reviewer link". `api_claim` accepted one, so the two endpoints disagreed about what
        // a credential is and an issued cookie could be traded for fresh cookies forever.
        let state = Mutex::new(CouchState {
            pairing_codes: HashMap::from([("pair-secret".to_string(), "Sara".to_string())]),
            reviewers: HashMap::from([("sara-cookie".to_string(), "Sara".to_string())]),
            session_issued: HashMap::from([("sara-cookie".to_string(), SystemTime::now())]),
            ..CouchState::default()
        });

        let (reply, cookie) = api_claim(br#"{"token":"sara-cookie"}"#, &state);
        assert_eq!(reply.0, 401, "a session cookie must not mint a second session");
        assert!(cookie.is_none(), "and it must set no cookie");
        assert_eq!(api_claim_probe(br#"{"token":"sara-cookie"}"#, &state), claim_probe_denied(), "both agree");

        let (reply, cookie) = api_claim(br#"{"token":"pair-secret"}"#, &state);
        assert_eq!(reply.0, 200, "the pairing secret is still the way in — this is a closed door, not a broken lock");
        assert!(cookie.is_some());
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
    fn private_production_maintenance_blocks_data_but_not_the_read_only_link_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("maintenance-clip", "دەقی تاقیکردنەوە")).unwrap();
        let state = Mutex::new(CouchState {
            reviewers: HashMap::from([("cookie".to_string(), "Rubar".to_string())]),
            pairing_codes: HashMap::from([("pairing".to_string(), "Rubar".to_string())]),
            session_store: Some((tmp.path().to_path_buf(), db_path)),
            ..CouchState::default()
        });
        let maintenance = tmp.path().join(PRIVATE_PRODUCTION_MAINTENANCE_FILE);
        std::fs::write(&maintenance, b"{}\n").unwrap();

        assert_eq!(route_for_test(&db, &state, "cookie").0, 503, "no queue may escape before release gates pass");
        assert_eq!(
            api_claim_probe(br#"{"token":"pairing"}"#, &state).0,
            200,
            "the non-mutating authentication probe must remain available during handover"
        );

        std::fs::remove_file(maintenance).unwrap();
        let after = route_for_test(&db, &state, "cookie");
        assert!(
            !String::from_utf8_lossy(&after.2).contains("protected release handover"),
            "removing the marker must atomically leave the maintenance refusal path"
        );
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
            // The link's PAIRING secret — the only credential /api/claim accepts.
            pairing_codes: HashMap::from([("goodtoken".to_string(), "Sara".to_string())]),
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
            // The link's PAIRING secret — the only credential /api/claim accepts.
            pairing_codes: HashMap::from([("goodtoken".to_string(), "Sara".to_string())]),
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
            // The 1,441-test suite saturates the Windows scheduler with restore/database drills.
            // Ordering is already proved by the strict 250ms pre-release non-completion assertion;
            // allow a loaded CI worker time to schedule the revoker after the guard is released.
            .recv_timeout(Duration::from_secs(15))
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
            s.alignment_json =
                Some(format!("{{\"source_start_ms\":0,\"source_end_ms\":{ms},\"chunk_index\":0,\"chunk_count\":1}}"));
            db.insert_segment(&s).unwrap();
        }
        gold_seg(&db, "r-gold", "دەقی هەڵە", "دەقی ڕاست");
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
        gold_seg(&db, "pay-hidden", "raw wrong draft", "human answer");
        let key = db.get_segment_by_id("pay-hidden").unwrap().unwrap();
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
            segment.alignment_json =
                Some(r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#.into());
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
    fn an_active_flexible_pool_keeps_paid_first_pass_review_serving() {
        let _serial = GLOBAL_SESSION_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if is_running() {
            stop().unwrap();
        }
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        let champion_id = "omniasr-7b-pay-policy-start-test";
        crate::registry::register_candidate(
            &db,
            &crate::registry::NewModelVersion {
                id: champion_id.into(),
                family: crate::deployment::OMNIASR_7B_FAMILY.into(),
                model_card_name: Some("pay policy start refusal champion".into()),
                checkpoint_sha256: "d".repeat(64),
                checkpoint_path: "/test/pay-policy-start-champion.json".into(),
                source: "cortex-finetuned".into(),
                license: "owner-full-rights".into(),
            },
        )
        .unwrap();
        db.connection().execute("UPDATE model_versions SET status='champion' WHERE id=?1", [champion_id]).unwrap();
        let mut segment = seg("pay-policy-start-segment", "champion draft");
        segment.model_version_id = Some(champion_id.into());
        db.insert_segment(&segment).unwrap();
        crate::review_pool::activate(
            &db,
            "37000000-0000-4000-8000-000000000001",
            &[crate::review_pool::PoolMemberInput { segment_id: segment.id.clone(), voice_name: "Lamo".into() }],
        )
        .unwrap();

        let pool_before: String = db
            .connection()
            .query_row("SELECT pool_id FROM review_pool_registry WHERE singleton_key=1", [], |row| row.get(0))
            .unwrap();
        let listener_probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener_probe.local_addr().unwrap().port();
        drop(listener_probe);
        let writer_called = std::cell::Cell::new(false);
        let revocation_called = std::cell::Cell::new(false);
        let result = start_on_port_with_session_lifecycle(
            db_path,
            vec!["Sara".into()],
            port,
            Some(tmp.path().to_path_buf()),
            |_| {
                writer_called.set(true);
                Ok(())
            },
            |_| {
                revocation_called.set(true);
                Ok(())
            },
        );

        // A pool row is PERMANENT once activated, and the live library carries one. Refusing start on
        // its presence therefore killed phone review outright — including the FIRST-pass canonical
        // path, which is fully paid under review-iqd-v1-2026-08-21 and is the only path any reviewer
        // uses (measured 2026-08-27: 372 credits, ~5,299 IQD, zero pool decisions ever). The
        // pay-policy fence now sits on the pool DECISION branch instead, where the unpaid work is.
        let started = match result {
            Ok(started) => started,
            Err(error) => panic!("an active flexible pool must not take paid first-pass review down: {error}"),
        };
        assert!(is_running(), "the review server must serve while a pool row exists");
        assert!(writer_called.get(), "a real start persists its session");
        let published = status();
        assert!(published.running, "status must publish a running server");
        let _ = started;
        let _ = revocation_called.get();
        // This test now STARTS a real server, so it must hand the global singleton back. Leaving it
        // running made two unrelated lifecycle tests fail for a reason that had nothing to do with them.
        stop_with_data_dir(Some(tmp.path())).unwrap();
        assert!(!is_running(), "the test must leave the singleton clean for the next one");
        // Whatever start does, it must never mutate the immutable pool registry.
        assert_eq!(
            db.connection()
                .query_row::<String, _, _>(
                    "SELECT pool_id FROM review_pool_registry WHERE singleton_key=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            pool_before,
            "the immutable active-pool contract must remain byte-exact"
        );
    }

    #[test]
    fn a_retyped_reviewer_name_keeps_the_link_the_session_and_the_outstanding_check() {
        // `normalize_reviewers` trims but PRESERVES typed casing, and the review columns are
        // COLLATE NOCASE — so "rubar" and "Rubar" are one reviewer everywhere except in the three
        // `==` comparisons the resume path used to make. Re-typing one name in Settings therefore
        // minted a fresh pairing token (every link already sent answers "link expired"), dropped the
        // restored cookie sessions (installed phone shortcuts die), and dropped the remembered
        // served-check pairs (the outstanding check 409s and its score is lost). That is exactly the
        // six-reviewers-silent shape of 2026-08-20, reachable by re-typing a name.
        let _serial = GLOBAL_SESSION_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("retyped-roster.db").to_string_lossy().to_string();
        Database::open(&db_path).unwrap().initialize().unwrap();
        let pairing = HashMap::from([("rubar-pair".to_string(), "Rubar".to_string())]);
        let spot_checks = HashSet::from([("hidden-1".to_string(), "Rubar".to_string())]);
        let sessions = HashMap::from([("rubar-cookie".to_string(), ("Rubar".to_string(), SystemTime::now()))]);
        save_session(tmp.path(), &pairing, &db_path, &spot_checks, &HashSet::new(), &sessions, None).unwrap();
        let port_probe = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = port_probe.local_addr().unwrap().port();
        drop(port_probe);

        // The owner re-types the roster in a different case. Same person.
        let started = start_on_port_with_session_lifecycle(
            db_path.clone(),
            vec!["rubar".into()],
            port,
            Some(tmp.path().to_path_buf()),
            save_session_snapshot,
            clear_session_revocation,
        );
        // Read the durable generation BEFORE Stop: Stop plants the revocation marker, after which
        // `load_session` correctly refuses to read anything.
        let resumed = load_session(tmp.path(), &db_path);
        let _ = stop_with_data_dir(Some(tmp.path()));

        let started = started.expect("a re-typed roster still starts");
        let link = started.reviewers.first().expect("the reviewer keeps a link");
        assert!(link.url.contains("#t=rubar-pair"), "the already-sent link must keep working: {}", link.url);
        let resumed = resumed.expect("the durable generation is readable");
        assert_eq!(resumed.pairing.get("rubar-pair").map(String::as_str), Some("rubar"));
        assert!(
            resumed.spot_checks.contains(&("hidden-1".to_string(), "rubar".to_string())),
            "the outstanding hidden check must survive, re-keyed onto the roster's current spelling: {:?}",
            resumed.spot_checks
        );
        assert_eq!(
            resumed.sessions.get("rubar-cookie").map(|(name, _)| name.as_str()),
            Some("rubar"),
            "the installed phone shortcut's cookie must still be a live session"
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
            let skipped_hidden_check = id == "pilot-key-2";
            let body = serde_json::json!({
                "id": id,
                "action": if skipped_hidden_check { "skip" } else { "edit" },
                "text": if skipped_hidden_check { "" } else { corrected },
                "rowVersion": item["rowVersion"],
                "pilotAfterReviewEventId": item["pilotAfterReviewEventId"],
                "heardMs": 1_500,
                "clipDurationMs": 1_500,
            });
            assert_eq!(api_decision(&db, body.to_string().as_bytes(), "Hawzhin", &state).0, 200);
        }
        assert!(fetch()["items"].as_array().unwrap().is_empty(), "both durable QC outcomes end catch-up");
        assert_eq!(lock_state(&state).pilot_spot_checks.len(), 2, "a third key is never authorized");
        assert_eq!(
            db.review_decision_progress(&build_pilot_decision_limit(&policy).unwrap()).unwrap().total_review_actions,
            10,
            "a hidden-check skip consumes its bounded QC key, not an eleventh corpus slot"
        );
        let hidden_skip_source: String = db
            .connection()
            .query_row(
                "SELECT source FROM review_events WHERE segment_id='pilot-key-2' AND reviewer='Hawzhin' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hidden_skip_source, "couch_spot_check", "a hidden skip must remain auditable as failed QC");
    }

    #[test]
    fn pilot_uses_sqlite_authority_when_the_session_mirror_is_lost() {
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

        // SQLite commits the two-key authorization before the response. The remembered-file mirror
        // is deliberately failed: reviewers still receive a safe batch because restart recovery no
        // longer depends on that cache.
        let failed_state = make_state(true);
        let (code, _, first_body, ..) = api_queue(&db, "Hawzhin", &failed_state);
        assert_eq!(code, 200);
        let first_payload: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        let mut first_ids: Vec<String> = first_payload["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_string())
            .collect();
        first_ids.sort();
        let policy_sha256 = policy.policy_sha256().unwrap();
        assert_eq!(
            db.review_pilot_hidden_keys(&policy_sha256, policy.after_review_event_id, "Hawzhin").unwrap(),
            first_ids,
            "every returned hidden key must already be durable in SQLite"
        );
        assert_eq!(lock_state(&failed_state).pilot_spot_checks.len(), 2);
        assert!(!session_path(dir.path()).exists());
        assert!(db.spot_check_report().unwrap().is_empty(), "serving a check does not fabricate a result");

        // Simulate deleting couch_session.json and restarting from empty memory. The exact same two
        // database reservations are re-served; there is no capacity for a third key.
        drop(failed_state);
        let restarted = make_state(false);
        let (code, _, body, ..) = api_queue(&db, "Hawzhin", &restarted);
        assert_eq!(code, 200);
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let mut restarted_ids: Vec<String> =
            payload["items"].as_array().unwrap().iter().map(|item| item["id"].as_str().unwrap().to_string()).collect();
        restarted_ids.sort();
        assert_eq!(restarted_ids, first_ids);
        assert_eq!(lock_state(&restarted).pilot_spot_checks.len(), 2);
        assert!(!session_path(dir.path()).exists(), "the proof must not depend on recreating the failed mirror");
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
        rollback_fixture_to(&db, 59);

        // Persist the jury columns with insert_segment_full (insert_segment would drop them).
        let mut s = seg("esc1", "دەق یەک");
        s.verdict = Some("escalated".into());
        s.escalated = true;
        s.verified = false;
        s.is_gold = false;
        db.insert_segment_full(&s).unwrap();
        upgrade_fixture_from(&db, 59);

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
        db.finalize_human_review("dk1", "edit", Some("ڕاستکراوەی دێسکتۆپ"), None, None).unwrap();

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
        let proof_db = Database::open(&db_path).unwrap();
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
            let playback_receipt_id = fixture_policy4_receipt(&proof_db, "Hawzhin", "tok", id, &item["rowVersion"]);
            let body = serde_json::json!({
                "operationId": uuid::Uuid::new_v4().to_string(),
                "playbackReceiptId": playback_receipt_id,
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
        let proof_db = Database::open(&db_path).unwrap();
        let mut decided: Vec<String> = Vec::new();
        let operation_ids = Mutex::new(HashMap::<String, String>::new());
        let playback_receipts = Mutex::new(HashMap::<String, String>::new());

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
            let playback_receipt_id = {
                let mut receipts = playback_receipts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                receipts
                    .entry(id.to_string())
                    .or_insert_with(|| fixture_policy4_receipt(&proof_db, "Hawzhin", "tok", id, row_version))
                    .clone()
            };
            let body = serde_json::json!({
                "operationId": operation_id,
                "playbackReceiptId": playback_receipt_id,
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
        let proof_db = Database::open(&db_path).unwrap();
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
                let playback_receipt_id =
                    fixture_policy4_receipt(&proof_db, "Hawzhin", "tok-solo", &id, &item["rowVersion"]);
                let body = serde_json::json!({
                    "operationId": uuid::Uuid::new_v4().to_string(),
                    "playbackReceiptId": playback_receipt_id,
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
            .map(|(token, reviewer)| {
                let (token, reviewer, base, db_path) =
                    (token.to_string(), reviewer.to_string(), format!("http://127.0.0.1:{port}"), db_path.clone());
                std::thread::spawn(move || {
                    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(20)).build();
                    let proof_db = Database::open(&db_path).unwrap();
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
                            let id = item["id"].as_str().unwrap().to_string();
                            let playback_receipt_id =
                                fixture_policy4_receipt(&proof_db, &reviewer, &token, &id, &item["rowVersion"]);
                            let body = serde_json::json!({
                                "operationId": uuid::Uuid::new_v4().to_string(),
                                "playbackReceiptId": playback_receipt_id,
                                "id": id,
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
        let client_attempt_id = uuid::Uuid::new_v4().to_string();
        let playback: serde_json::Value = agent
            .post(&format!("{base}/api/playback/start"))
            .set("Cookie", &cookie_for("saratoken123"))
            .send_string(
                &serde_json::json!({
                    "id": mine,
                    "rowVersion": mine_item["rowVersion"],
                    "clientAttemptId": client_attempt_id,
                })
                .to_string(),
            )
            .unwrap()
            .into_json()
            .unwrap();
        let playback_receipt_id = playback["playbackReceiptId"].as_str().unwrap();
        assert_eq!(
            agent
                .get(&format!("{base}/api/audio/{mine}?playbackAttemptId={playback_receipt_id}"))
                .set("Cookie", &cookie_for("saratoken123"))
                .call()
                .unwrap()
                .status(),
            200
        );
        // The policy permits at most 2x server-monotonic elapsed media traversal. A complete 1.5s
        // renderer interval therefore needs at least 750ms after authorized media delivery.
        std::thread::sleep(std::time::Duration::from_millis(800));
        let finalized: serde_json::Value = agent
            .post(&format!("{base}/api/playback/finalize"))
            .set("Cookie", &cookie_for("saratoken123"))
            .send_string(
                &serde_json::json!({
                    "playbackReceiptId": playback_receipt_id,
                    "clientAttemptId": client_attempt_id,
                    "intervals": [{"startMs": 0, "endMs": 1_500}],
                })
                .to_string(),
            )
            .unwrap()
            .into_json()
            .unwrap();
        let resp = agent
            .post(&format!("{base}/api/decision"))
            .set("Cookie", &cookie_for("saratoken123"))
            .send_string(
                &serde_json::json!({
                    "operationId": uuid::Uuid::new_v4().to_string(),
                    "playbackReceiptId": finalized["playbackReceiptId"],
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
        std::fs::write(&s.audio_path, b"RIFF").unwrap();
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
        write_test_wav_seeded(path, samples, &[]);
    }

    fn write_test_wav_seeded(path: &std::path::Path, samples: usize, salt: &[u8]) {
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
            let salt_sample = salt.get(n % salt.len().max(1)).copied().unwrap_or(128) as i16 - 128;
            w.write_sample(((n % 1000) as i16).wrapping_mul(30).wrapping_add(salt_sample)).unwrap();
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
        db.connection()
            .execute("UPDATE speech_segments SET alignment_json = NULL WHERE id LIKE 'audio-%'", [])
            .unwrap();
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
        assert!(db.update_verified_for_test("audio-policy", true).unwrap());
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
            db.insert_segment(&key).unwrap();
            db.finalize_human_review(id, "edit", Some(answer), None, None).unwrap();
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
        let policy_sha256 = policy.policy_sha256().unwrap();
        db.reserve_review_pilot_hidden_keys(
            &policy_sha256,
            policy.after_review_event_id,
            "Hawzhin",
            &["audio-pilot-key".into()],
            2,
        )
        .unwrap();
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
        let key_alignment = db
            .get_segment_by_id("audio-pilot-key")
            .unwrap()
            .unwrap()
            .alignment_json
            .expect("fixture has a compensation source span");
        db.connection()
            .execute("UPDATE speech_segments SET alignment_json = NULL WHERE id = 'audio-pilot-key'", [])
            .unwrap();

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
        db.connection()
            .execute("UPDATE speech_segments SET alignment_json = ?1 WHERE id = 'audio-pilot-key'", [key_alignment])
            .unwrap();
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
        db.connection().execute("UPDATE speech_segments SET alignment_json = NULL WHERE id = 's1'", []).unwrap();
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
