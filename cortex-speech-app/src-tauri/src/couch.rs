//! couch.rs — "Couch Review": a LAN-only, token-gated phone review page served by the desktop app.
//!
//! The owner starts it from Settings, opens the shown URL on a phone on the SAME Wi-Fi, and reviews
//! clips (listen → correct → save) from the couch. Decisions land in the same database through the
//! same functions the desktop review uses (`record_human_decision` + whole-row upsert), so a phone
//! review is indistinguishable from a desktop one.
//!
//! Privacy stance (voice is biometric — GDPR Art. 9):
//!   * OFF by default; started explicitly per session and stopped on demand or app exit.
//!   * Serves on the LAN only in the sense that nothing is ever published or relayed — no cloud, no
//!     tunneling. (A router/firewall still defines the actual reachable network; Windows prompts for
//!     the inbound allow on first start.)
//!   * EVERY endpoint — including audio bytes — requires the per-session random token. No token, no
//!     data. The token is regenerated on every start and never persisted.
//!   * Read + review only: the API can list the queue, stream one clip's audio, record a decision,
//!     and undo it. No export, no settings, no keys, no file paths are reachable.
//!
//! Deliberately synchronous (tiny_http, one request at a time against its own WAL connection) — the
//! couch has exactly one reviewer; parallelism would buy nothing and cost locking complexity.

use crate::db::{Database, SpeechSegment};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The single global server handle (at most one couch session at a time).
static COUCH: Mutex<Option<CouchHandle>> = Mutex::new(None);

/// Fixed default port — memorable, and stable across restarts so the phone bookmark keeps working.
const COUCH_PORT: u16 = 8737;
/// Cap on request bodies (a decision payload is < 1 KB; 256 KB leaves room for very long transcripts).
const MAX_BODY_BYTES: usize = 256 * 1024;

struct CouchHandle {
    shutdown: Arc<AtomicBool>,
    /// Shared with the accept-loop thread so `stop()` can `unblock()` it. A bare TcpStream connect
    /// does NOT wake tiny_http (it only yields COMPLETE HTTP requests), so unblock() is the only
    /// non-hanging shutdown — the live end-to-end test caught stop() deadlocking on join() without it.
    server: Arc<tiny_http::Server>,
    port: u16,
    token: String,
    join: Option<std::thread::JoinHandle<()>>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CouchStatus {
    pub running: bool,
    /// Full URL (with token) to open on the phone; None when stopped.
    pub url: Option<String>,
    /// Same page over the owner's Tailscale tailnet (works from ANY network, end-to-end encrypted
    /// WireGuard between the owner's own devices — never the public internet). None when this machine
    /// has no Tailscale interface. The transport is Tailscale's; the couch token gate still applies.
    pub tailscale_url: Option<String>,
}

/// Start the couch server (idempotent: returns the existing session if already running).
pub fn start(db_path: String) -> Result<CouchStatus, String> {
    let mut guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(h) = guard.as_ref() {
        return Ok(status_of(h));
    }
    if db_path == ":memory:" {
        return Err("Couch Review needs a file-backed database".to_string());
    }

    // Per-session random token (2× UUIDv4 = 244 random bits). Never persisted, never logged.
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());

    let server = Arc::new(
        tiny_http::Server::http(("0.0.0.0", COUCH_PORT))
            .map_err(|e| format!("Couch server could not bind port {COUCH_PORT}: {e}"))?,
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    let join = spawn_server_loop(server.clone(), db_path, token.clone(), shutdown.clone())?;
    let handle = CouchHandle { shutdown, server, port: COUCH_PORT, token: token.clone(), join: Some(join) };
    let status = status_of(&handle);
    *guard = Some(handle);
    tracing::info!("Couch Review started on port {COUCH_PORT} (token gated; stop from Settings)");
    Ok(status)
}

/// Stop the couch server and drop the session token.
pub fn stop() -> Result<CouchStatus, String> {
    let mut guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut h) = guard.take() {
        h.shutdown.store(true, Ordering::SeqCst);
        // unblock() makes the accept loop's iterator return None so the thread exits. (A bare TCP
        // connect does NOT wake tiny_http — it yields only complete HTTP requests — so without this,
        // join() deadlocked; caught by the live end-to-end test.)
        h.server.unblock();
        if let Some(join) = h.join.take() {
            let _ = join.join();
        }
        tracing::info!("Couch Review stopped");
    }
    Ok(CouchStatus { running: false, url: None, tailscale_url: None })
}

pub fn status() -> CouchStatus {
    let guard = COUCH.lock().unwrap_or_else(|p| p.into_inner());
    guard.as_ref().map(status_of).unwrap_or(CouchStatus { running: false, url: None, tailscale_url: None })
}

fn status_of(h: &CouchHandle) -> CouchStatus {
    CouchStatus {
        running: true,
        url: Some(format!("http://{}:{}/?t={}", lan_ip(), h.port, h.token)),
        tailscale_url: tailscale_ip().map(|ip| format!("http://{ip}:{}/?t={}", h.port, h.token)),
    }
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
    server: Arc<tiny_http::Server>,
    db_path: String,
    token: String,
    shutdown: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("couch-review".into())
        .spawn(move || {
            // The couch session's own WAL connection + its own undo stack (mirrors ReviewMode's).
            let db = match Database::open(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    tracing::error!("Couch Review db open failed: {e}");
                    return;
                }
            };
            let mut undo_stack: Vec<(String, SpeechSegment)> = Vec::new();
            for mut request in server.incoming_requests() {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let response = handle_request(&mut request, &db, &token, &mut undo_stack);
                let _ = respond(request, response);
            }
        })
        .map_err(|e| format!("Couch Review server thread failed to spawn: {e}"))
}

/// (status, content_type, body)
type Reply = (u16, &'static str, Vec<u8>);

fn json_reply(status: u16, value: serde_json::Value) -> Reply {
    (status, "application/json", value.to_string().into_bytes())
}
fn err_reply(status: u16, msg: &str) -> Reply {
    (status, "text/plain; charset=utf-8", msg.as_bytes().to_vec())
}

fn respond(request: tiny_http::Request, (status, ctype, body): Reply) -> std::io::Result<()> {
    let mut response = tiny_http::Response::from_data(body).with_status_code(status);
    // The content types here are static ASCII literals, so this never fails in practice — but the
    // panic policy forbids expect(); a (hypothetical) bad header just means the response ships
    // without a Content-Type rather than killing the server thread.
    if let Ok(header) = tiny_http::Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()) {
        response = response.with_header(header);
    }
    request.respond(response)
}

/// Extract `t` from a raw request URL's query string.
fn token_from_url(url: &str) -> Option<&str> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("t="))
}

fn handle_request(
    request: &mut tiny_http::Request,
    db: &Database,
    token: &str,
    undo_stack: &mut Vec<(String, SpeechSegment)>,
) -> Reply {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = request.method().clone();

    // EVERY route is token-gated — including the page itself, so the URL is unusable without `?t=`.
    if token_from_url(&url) != Some(token) {
        return err_reply(401, "unauthorized");
    }

    match (method, path.as_str()) {
        (tiny_http::Method::Get, "/") => {
            (200, "text/html; charset=utf-8", include_str!("../assets/couch.html").as_bytes().to_vec())
        }
        (tiny_http::Method::Get, "/api/queue") => api_queue(db),
        (tiny_http::Method::Get, p) if p.starts_with("/api/audio/") => {
            api_audio(db, p.trim_start_matches("/api/audio/"))
        }
        (tiny_http::Method::Post, "/api/decision") => match read_body(request) {
            Ok(body) => api_decision(db, &body, undo_stack),
            Err(e) => err_reply(400, &e),
        },
        (tiny_http::Method::Post, "/api/undo") => api_undo(db, undo_stack),
        _ => err_reply(404, "not found"),
    }
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
/// with. Bounded to keep the payload phone-friendly; the page reloads as the queue drains.
fn api_queue(db: &Database) -> Reply {
    let segments = match db.get_segments(Some(false)) {
        Ok(s) => s,
        Err(e) => return err_reply(500, &e.to_string()),
    };
    let queue: Vec<serde_json::Value> = segments
        .iter()
        .take(200)
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "text": review_text(s),
                "durationMs": s.duration_ms,
                "speakerId": s.speaker_id,
            })
        })
        .collect();
    json_reply(200, serde_json::Value::Array(queue))
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
struct DecisionBody {
    id: String,
    /// "accept" | "edit" | "bad"
    action: String,
    #[serde(default)]
    text: String,
}

/// Record one phone decision through the exact desktop path: `record_human_decision` FIRST (its
/// validation aborts before anything is committed), then the whole-row upsert built from the FRESH
/// db row — never from the payload — so only `annotated_transcript`/`verified` can change.
fn api_decision(db: &Database, body: &[u8], undo_stack: &mut Vec<(String, SpeechSegment)>) -> Reply {
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

    undo_stack.push((prev.id.clone(), prev.clone()));
    if let Err(e) = crate::jury::record_human_decision(db, &parsed.id, decision, text.as_deref(), None) {
        undo_stack.pop();
        return err_reply(500, &e.to_string());
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
fn api_undo(db: &Database, undo_stack: &mut Vec<(String, SpeechSegment)>) -> Reply {
    let Some((id, prev)) = undo_stack.pop() else {
        return err_reply(409, "nothing to undo");
    };
    if let Err(e) = db.clear_human_decision(&id) {
        undo_stack.push((id, prev));
        return err_reply(500, &e.to_string());
    }
    if let Err(e) = db.insert_segment_full(&prev) {
        return err_reply(500, &e.to_string());
    }
    json_reply(200, serde_json::json!({ "id": id }))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn decision_accept_edit_bad_and_undo_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, _) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەق یەک")).unwrap();
        db.insert_segment(&seg("s2", "دەق دوو")).unwrap();
        let mut undo = Vec::new();

        // EDIT: corrected text becomes the annotated gold; row is verified; decision recorded.
        let body = serde_json::json!({"id": "s1", "action": "edit", "text": "دەق یەک ڕاستکراوە"});
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), &mut undo);
        assert_eq!(code, 200);
        let row = db.get_segment_by_id("s1").unwrap().unwrap();
        assert!(row.verified);
        assert_eq!(row.annotated_transcript.as_deref(), Some("دەق یەک ڕاستکراوە"));

        // BAD: reject decision, still marked reviewed.
        let body = serde_json::json!({"id": "s2", "action": "bad"});
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), &mut undo);
        assert_eq!(code, 200);
        assert!(db.get_segment_by_id("s2").unwrap().unwrap().verified);

        // Queue now empty (both reviewed).
        let (code, _, body) = api_queue(&db);
        assert_eq!(code, 200);
        let queue: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(queue.is_empty(), "both clips reviewed -> empty queue, got {queue:?}");

        // UNDO restores the LAST decision (s2): unverified again, decision cleared.
        let (code, _, body) = api_undo(&db, &mut undo);
        assert_eq!(code, 200);
        let undone: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(undone["id"], "s2");
        let row = db.get_segment_by_id("s2").unwrap().unwrap();
        assert!(!row.verified, "undo must reopen the clip");

        // Second undo pops s1; third has nothing left.
        let (code, _, _) = api_undo(&db, &mut undo);
        assert_eq!(code, 200);
        let (code, _, _) = api_undo(&db, &mut undo);
        assert_eq!(code, 409, "empty undo stack is a 409, not a phantom undo");
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

        let mut undo = Vec::new();
        let body = serde_json::json!({ "id": "esc1", "action": "accept", "text": "دەق یەک" });
        let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), &mut undo);
        assert_eq!(code, 200, "the accept decision must succeed");
        assert!(db.get_segment_by_id("esc1").unwrap().unwrap().verified, "accept verifies the clip");

        let (code, _, _) = api_undo(&db, &mut undo);
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
        let mut undo = Vec::new();

        for (body, expect) in [
            (serde_json::json!({"id": "s1", "action": "edit", "text": ""}), 400u16),
            (serde_json::json!({"id": "s1", "action": "accept", "text": "[Pending WSL 7B ASR]"}), 400),
            (serde_json::json!({"id": "s1", "action": "explode", "text": "x"}), 400),
            (serde_json::json!({"id": "../etc", "action": "accept", "text": "x"}), 400),
            (serde_json::json!({"id": "missing", "action": "accept", "text": "x"}), 404),
        ] {
            let (code, _, _) = api_decision(&db, body.to_string().as_bytes(), &mut undo);
            assert_eq!(code, expect, "body: {body}");
        }
        assert!(undo.is_empty(), "no failed decision may leave an undo entry");
        assert!(!db.get_segment_by_id("s1").unwrap().unwrap().verified, "row untouched by rejected requests");
    }

    #[test]
    fn live_server_gates_every_route_on_the_token() {
        // REAL end-to-end: boot the server on an OS-assigned port, hit it over loopback HTTP.
        // Every HTTP call carries explicit timeouts so a wedged server/socket FAILS the test with the
        // offending call named, instead of hanging the suite indefinitely.
        let tmp = tempfile::tempdir().unwrap();
        let (db, db_path) = test_db(tmp.path());
        db.insert_segment(&seg("s1", "دەقی تاقیکردنەوە")).unwrap();
        drop(db); // the server opens its own connection

        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", 0)).unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let token = "testtoken123".to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = spawn_server_loop(server.clone(), db_path, token.clone(), shutdown.clone())
            .expect("test server thread spawns");

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build();

        let base = format!("http://127.0.0.1:{port}");
        // No token -> 401 on the page, the queue, and audio alike.
        for path in ["/", "/api/queue", "/api/audio/s1"] {
            let code = agent.get(&format!("{base}{path}")).call().map(|r| r.status()).unwrap_or_else(|e| match e {
                ureq::Error::Status(c, _) => c,
                other => panic!("{path} transport error (timeout/hang?): {other}"),
            });
            assert_eq!(code, 401, "{path} must be token-gated");
        }
        // With the token: the page serves and the queue lists the pending clip.
        let page = agent.get(&format!("{base}/?t={token}")).call().unwrap();
        assert_eq!(page.status(), 200);
        let queue: Vec<serde_json::Value> =
            agent.get(&format!("{base}/api/queue?t={token}")).call().unwrap().into_json().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0]["id"], "s1");

        // A phone decision over real HTTP verifies the row in the real db file.
        let resp = agent
            .post(&format!("{base}/api/decision?t={token}"))
            .send_string(&serde_json::json!({"id": "s1", "action": "accept", "text": "دەقی تاقیکردنەوە"}).to_string())
            .unwrap();
        assert_eq!(resp.status(), 200);
        let db = Database::open(tmp.path().join("couch-test.db").to_string_lossy().as_ref()).unwrap();
        assert!(db.get_segment_by_id("s1").unwrap().unwrap().verified);

        shutdown.store(true, Ordering::SeqCst);
        server.unblock(); // the fix under test: unblock() (not a bare connect) ends the accept loop
        join.join().expect("server thread joins cleanly after unblock()");
    }
}
