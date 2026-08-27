use super::*;

pub(super) type Reply = (u16, &'static str, Vec<u8>, Vec<(&'static str, String)>);

/// A reply that also plants the session cookie. Used only for the page itself: the token arrives in
/// the URL exactly once, and from then on the cookie carries it.
///
/// `HttpOnly` is the point — the SERVER sets it, so page JavaScript can never read the token back
/// out. `Secure` is mandatory because Couch Review now serves TLS on every interface;
/// `SameSite=Strict` keeps it off cross-site requests and a 24-hour sliding lifetime narrows exposure
/// from a lost phone without interrupting an active reviewer.
pub(super) fn page_reply_with_cookie(token: &str, body: Vec<u8>) -> (Reply, Option<String>) {
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
pub(super) fn api_claim(body: &[u8], state: &Mutex<CouchState>) -> (Reply, Option<String>) {
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
        // PAIRING CREDENTIALS ONLY, the same boundary `api_claim_probe` enforces. A session cookie is
        // not a durable reviewer link: accepting one here let an already-issued cookie be traded for
        // fresh cookies indefinitely, outliving the eviction and revocation that the pairing secret
        // governs — and it made the two endpoints disagree about what a credential is.
        let reviewer = guard.pairing_codes.get(&parsed.token).cloned();
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
pub(super) enum CookieRenewal {
    Current,
    Renewed,
    Missing,
}

/// Advance a cookie's server-side/durable checkpoint only when the last checkpoint is old enough.
/// Frequent page loads do NOT move the in-memory time: otherwise every sub-15-minute visit resets the
/// clock before it becomes eligible to persist, and an active reviewer eventually dies at restart.
pub(super) fn renew_cookie_checkpoint(
    token: &str,
    state: &Mutex<CouchState>,
    now: SystemTime,
) -> Result<CookieRenewal, String> {
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
pub(super) struct ClaimProbeSnapshot {
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
pub(super) fn unexpired_live_session_seconds(
    state: &CouchState,
    now: SystemTime,
) -> Option<HashMap<String, (String, u64)>> {
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

pub(super) fn remembered_session_seconds(
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
pub(super) fn durable_claim_probe_state_matches(snapshot: &ClaimProbeSnapshot) -> bool {
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

pub(super) fn sha256_lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

pub(super) fn claim_probe_denied() -> Reply {
    // One generic response for malformed, unknown, cookie-only, and oversized credentials: the
    // endpoint never explains which part of a secret was close and never echoes untrusted input.
    err_reply(401, "unauthorized")
}

/// Read-only counterpart to [`api_claim`], used only by the production link gate. It authenticates
/// the durable pairing secret but never mints/prunes a cookie, touches a lease, consumes a hidden
/// key, writes the database, or persists state. The database path itself is intentionally absent;
/// callers compare a SHA-256 binding instead of receiving a private machine path.
pub(super) fn api_claim_probe(body: &[u8], state: &Mutex<CouchState>) -> Reply {
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

pub(super) fn json_reply(status: u16, value: serde_json::Value) -> Reply {
    (status, "application/json", value.to_string().into_bytes(), vec![])
}

/// Build the one reviewer-accounting payload used by every queue/decision/undo response.
///
/// `reviewedMs` is full judged-audio progress. `earnedMicroIqd` is the distinct, action-weighted
/// ledger total and is encoded as a decimal STRING so JavaScript cannot round an i64 amount. Keeping
/// this in one helper prevents the retry, spot-check and Undo paths from drifting onto different pay
/// semantics. Callers that have already committed irreplaceable work use
/// [`json_reply_with_accounting`], which reports accounting as unavailable instead of inventing zero.
pub(super) fn reviewer_accounting(db: &Database, reviewer: &str) -> Result<serde_json::Value, String> {
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

pub(super) fn merge_json_object(target: &mut serde_json::Value, source: serde_json::Value) {
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
pub(super) fn json_reply_with_accounting(
    status: u16,
    mut value: serde_json::Value,
    db: &Database,
    reviewer: &str,
) -> Reply {
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

pub(super) fn err_reply(status: u16, msg: &str) -> Reply {
    (status, "text/plain; charset=utf-8", msg.as_bytes().to_vec(), vec![])
}

pub(super) fn respond_with_cookie(
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
pub(super) const COUCH_COOKIE: &str = "cortex_couch";

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
pub(super) fn token_from_request(request: &tiny_http::Request) -> Option<String> {
    let cookies = request.headers().get("Cookie")?;
    cookies.as_str().split(';').find_map(|kv| {
        let (name, value) = kv.split_once('=')?;
        (name.trim() == COUCH_COOKIE).then(|| value.trim().to_string())
    })
}

/// One request header by name, case-insensitively (tiny_http's `equiv` does the casing, and requires
/// a `'static` name — every caller passes a literal).
pub(super) fn request_header(request: &tiny_http::Request, name: &'static str) -> Option<String> {
    request.headers().get(name).map(|value| value.as_str().to_string())
}

pub(super) fn handle_request(
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

    let maintenance = {
        let guard = lock_state(state);
        guard
            .session_store
            .as_ref()
            .is_some_and(|(data_dir, _)| data_dir.join(PRIVATE_PRODUCTION_MAINTENANCE_FILE).is_file())
    };
    if maintenance {
        return (err_reply(503, "Cortex Review is completing a protected release handover; retry shortly"), None);
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
                guard.remove_playback_attempts_for_session(&couch_session_binding_sha256(&token));
                return None;
            }
            guard.reviewers.get(&token).cloned().map(|name| (token, name))
        });

    if let (tiny_http::Method::Get, "/") = (&method, path.as_str()) {
        let shell = include_str!("../../assets/couch.html").as_bytes().to_vec();
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
        return ((200, "image/png", include_bytes!("../../assets/couch-icon.png").to_vec(), vec![]), None);
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
    let session_binding_sha256 = couch_session_binding_sha256(&token);
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
        (audio_method @ (tiny_http::Method::Get | tiny_http::Method::Head), p) if p.starts_with("/api/audio/") => {
            let is_head = audio_method == tiny_http::Method::Head;
            with_live_reviewer(&token, reviewer, state, || {
                let playback_attempt = match playback_attempt_query(&url) {
                    Ok(value) => value,
                    Err(reply) => return reply,
                };
                api_audio_authenticated(
                    db,
                    p.trim_start_matches("/api/audio/"),
                    reviewer,
                    &session_binding_sha256,
                    state,
                    playback_attempt.as_deref(),
                    is_head,
                    range.as_deref(),
                    if_none_match.as_deref(),
                )
            })
        }
        (tiny_http::Method::Post, "/api/playback/start") => match read_body(request) {
            Ok(body) => with_live_reviewer(&token, reviewer, state, || {
                api_playback_start(db, &body, reviewer, &session_binding_sha256, state)
            }),
            Err(e) => err_reply(400, &e),
        },
        (tiny_http::Method::Post, "/api/playback/finalize") => match read_body(request) {
            Ok(body) => with_live_reviewer(&token, reviewer, state, || {
                api_playback_finalize(db, &body, reviewer, &session_binding_sha256, state)
            }),
            Err(e) => err_reply(400, &e),
        },
        (tiny_http::Method::Post, "/api/decision") => match read_body(request) {
            Ok(body) => with_live_reviewer(&token, reviewer, state, || {
                api_decision_authenticated(db, &body, reviewer, &session_binding_sha256, state)
            }),
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
pub(super) fn lock_state(state: &Mutex<CouchState>) -> std::sync::MutexGuard<'_, CouchState> {
    state.lock().unwrap_or_else(|p| p.into_inner())
}

pub(super) fn remember_phone_undo(
    state: &Mutex<CouchState>,
    reviewer: &str,
    operation_id: &str,
    segment_id: &str,
    effect_event_id: i64,
) {
    let mut guard = lock_state(state);
    // The UI exposes one "Undo last" operation. Once a canonical decision lands, an older independent
    // pool observation is no longer the last action and must not win routing merely because it has its
    // own stack.
    guard.pool_undo.remove(reviewer);
    let stack = guard.undo.entry(reviewer.to_string()).or_default();
    if !stack.iter().any(|entry| entry.effect_event_id == effect_event_id) {
        stack.push(UndoEntry {
            operation_id: operation_id.to_string(),
            seg_id: segment_id.to_string(),
            effect_event_id,
        });
    }
    let overflow = stack.len().saturating_sub(UNDO_DEPTH);
    if overflow > 0 {
        stack.drain(0..overflow);
    }
}

pub(super) fn remember_pool_undo(
    state: &Mutex<CouchState>,
    reviewer: &str,
    operation_id: &str,
    segment_id: &str,
    decision_id: i64,
) {
    let mut guard = lock_state(state);
    guard.undo.remove(reviewer);
    let stack = guard.pool_undo.entry(reviewer.to_string()).or_default();
    if !stack.iter().any(|entry| entry.decision_id == decision_id) {
        stack.push(IndependentUndoEntry {
            operation_id: operation_id.to_string(),
            seg_id: segment_id.to_string(),
            decision_id,
        });
    }
    let overflow = stack.len().saturating_sub(UNDO_DEPTH);
    if overflow > 0 {
        stack.drain(0..overflow);
    }
}

pub(super) fn remember_independent_undo(
    state: &Mutex<CouchState>,
    reviewer: &str,
    operation_id: &str,
    segment_id: &str,
    decision_id: i64,
) {
    let mut guard = lock_state(state);
    let stack = guard.independent_undo.entry(reviewer.to_string()).or_default();
    if !stack.iter().any(|entry| entry.decision_id == decision_id) {
        stack.push(IndependentUndoEntry {
            operation_id: operation_id.to_string(),
            seg_id: segment_id.to_string(),
            decision_id,
        });
    }
    let overflow = stack.len().saturating_sub(UNDO_DEPTH);
    if overflow > 0 {
        stack.drain(0..overflow);
    }
}

pub(super) fn retain_phone_undo_token(
    state: &Mutex<CouchState>,
    reviewer: &str,
    insertion_index: usize,
    entry: UndoEntry,
) {
    let mut guard = lock_state(state);
    let stack = guard.undo.entry(reviewer.to_string()).or_default();
    if stack.iter().any(|existing| existing.effect_event_id == entry.effect_event_id) {
        return;
    }
    stack.insert(insertion_index.min(stack.len()), entry);
}

/// Execute one protected route only while the cookie still resolves to the same reviewer. The caller
/// reads the bounded POST body first, so a slow client cannot hold revocation hostage merely by never
/// finishing its upload. The read guard stays through the whole closure; `revoke_in` takes the write
/// side, which closes the copied-identity TOCTOU without serializing independent reviewer requests.
pub(super) fn with_live_reviewer<F>(token: &str, reviewer: &str, state: &Mutex<CouchState>, operation: F) -> Reply
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
pub(super) fn live_session_guard(
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
            guard.remove_playback_attempts_for_session(&couch_session_binding_sha256(token));
        }
        !expired
            && guard
                .reviewers
                .get(token)
                .is_some_and(|name| expected_reviewer.map_or(true, |expected| name == expected))
    };
    live.then_some(authorization)
}

pub(super) fn read_claim_probe_body(request: &mut tiny_http::Request) -> Result<Vec<u8>, ()> {
    let mut body = Vec::new();
    request.as_reader().take(MAX_CLAIM_PROBE_BODY_BYTES as u64 + 1).read_to_end(&mut body).map_err(|_| ())?;
    (body.len() <= MAX_CLAIM_PROBE_BODY_BYTES).then_some(body).ok_or(())
}

pub(super) fn read_body(request: &mut tiny_http::Request) -> Result<Vec<u8>, String> {
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
