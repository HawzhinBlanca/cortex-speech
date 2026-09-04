//! Isolated serving-path proof for the flexible review pool, at schema 69.
//!
//! This test deliberately drives the real HTTPS listener, pairing-cookie exchange, queue, the
//! policy-4 playback contract (start -> authorized media -> finalize), the decision, restart/resume,
//! and undo endpoints.  Each server generation is a child copy of this test binary, so killing it
//! models an app/watchdog restart without calling Couch Review's explicit `stop()` revocation path.
//! The profile and database are temporary and the port is never 8737.
//!
//! Ported from `codex/review-production-v63` (schema 63/65).  Three v69 boundaries changed what the
//! serving path can be asked to prove, and each is asserted here rather than worked around:
//!
//!   1. A pool clip that is NOT yet canonical is decided through the ordinary, PAID canonical path.
//!      A clip that already carries a canonical human answer routes to the pool branch, which — since
//!      the owner's canon change of 2026-09-04 ("pool second opinions are paid at the same weights as
//!      first opinions") — records a paid pool observation instead of refusing with
//!      `PAY_POLICY_REQUIRED`. This test pins that a lost-response replay of the canonical decision is
//!      still acknowledged (not diverted into the pool path), that the one-opinion clip is served
//!      FIRST to a different reviewer, that the second opinion needs the same policy-4 listening proof,
//!      lands exactly once however often it is replayed, and earns exactly one credit at the same
//!      weight as a first opinion.
//!   2. A new verdict needs a server-issued, cookie-session-bound policy-4 playback receipt.  Legacy
//!      `heardMs`/`clipDurationMs` counters never authorize one, so every judgement below earns its
//!      receipt through the real endpoints against real WAV bytes.
//!   3. A canonical `skip` writes NOTHING to the corpus and pushes no undo entry, so the durable
//!      restart-safe Undo target here is the reviewer's canonical judgement, recovered from the
//!      database after the in-memory stack died with its process.

use cortex_speech_app_lib::couch;
use cortex_speech_app_lib::db::{Database, SpeechSegment};
use cortex_speech_app_lib::deployment::OMNIASR_7B_FAMILY;
use cortex_speech_app_lib::fingerprint::AudioFingerprint;
use cortex_speech_app_lib::registry::{self, NewModelVersion};
use cortex_speech_app_lib::review_pool::{self, PoolMemberInput};
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const LIVE_PORT: u16 = 8737;
const FIRST_ID: &str = "serving-path-first";
const SKIP_ID: &str = "serving-path-skip";
/// Fixture identities only. Real reviewers are people, and their names are data this repository does
/// not carry — the roster lives in the owner's profile, never in the tracked tree.
const REVIEWER_ONE: &str = "Fixture-Reviewer-One";
const REVIEWER_TWO: &str = "Fixture-Reviewer-Two";
const VOICE_NAME: &str = "fixture-voice";
const FIRST_OPERATION: &str = "00000000-0000-4000-8000-000000000101";
const FENCED_OPERATION: &str = "00000000-0000-4000-8000-000000000102";
const SKIP_OPERATION: &str = "00000000-0000-4000-8000-000000000103";
const LEGACY_COUNTER_OPERATION: &str = "00000000-0000-4000-8000-000000000104";
const NO_PROOF_OPERATION: &str = "00000000-0000-4000-8000-000000000105";
const CLIP_DURATION_MS: i64 = 1_500;
/// `MIN_PLAYBACK_COVERAGE` is 0.85 and `DESKTOP_PLAYBACK_MAX_RATE` is 2, so a full-clip interval union
/// is only plausible once at least `CLIP_DURATION_MS / 2` of real wall clock has passed since the
/// authorized media was served. Sleep past that bound rather than near it.
const PLAYBACK_DWELL: Duration = Duration::from_millis(900);

/// Child entrypoint.  It is ignored during an ordinary test run and launched explicitly by the parent
/// test below.  Abruptly killing this process preserves the durable session exactly like an
/// app/watchdog restart; writing `stop` asks the final generation to revoke and release cleanly.
#[test]
#[ignore = "helper process for reviewer_flow_survives_real_https_retries_and_restarts"]
fn reviewer_serving_path_child() {
    let Ok(mode) = std::env::var("CORTEX_SERVING_PROBE_CHILD") else {
        return;
    };
    let data_dir =
        PathBuf::from(std::env::var_os("CORTEX_SERVING_PROBE_DATA_DIR").expect("isolated profile is required"));
    let db_path = std::env::var("CORTEX_SERVING_PROBE_DB").expect("isolated database path is required");
    let ready_path = PathBuf::from(std::env::var_os("CORTEX_SERVING_PROBE_READY").expect("ready path is required"));
    let control_path =
        PathBuf::from(std::env::var_os("CORTEX_SERVING_PROBE_CONTROL").expect("control path is required"));

    let status = match mode.as_str() {
        "start" => couch::start(db_path, vec![REVIEWER_ONE.into(), REVIEWER_TWO.into()], Some(data_dir)),
        "resume" => couch::resume(db_path, &data_dir).ok_or_else(|| "durable Couch session did not resume".into()),
        other => Err(format!("unknown child mode {other}")),
    }
    .expect("isolated Couch server must start");
    fs::write(&ready_path, serde_json::to_vec(&status).expect("serialize status")).expect("publish ready status");

    loop {
        if control_path.is_file() {
            let command = fs::read_to_string(&control_path).unwrap_or_default();
            if command.trim() == "stop" {
                couch::stop().expect("final isolated Couch server must stop cleanly");
            }
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

struct ServerChild {
    child: Option<Child>,
    control_path: PathBuf,
}

impl ServerChild {
    fn crash(mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().expect("kill isolated server generation");
            let status = child.wait().expect("wait for killed isolated server generation");
            assert!(!status.success(), "the restart boundary must be abrupt, not an accidental graceful exit");
        }
    }

    fn stop(mut self) {
        fs::write(&self.control_path, b"stop\n").expect("signal clean isolated shutdown");
        if let Some(mut child) = self.child.take() {
            let status = child.wait().expect("wait for clean isolated shutdown");
            assert!(status.success(), "final isolated server generation failed: {status}");
        }
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn free_non_live_port() -> u16 {
    for _ in 0..32 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("reserve an isolated port");
        let port = listener.local_addr().expect("reserved port address").port();
        drop(listener);
        if port != LIVE_PORT {
            return port;
        }
    }
    panic!("could not allocate a non-live test port");
}

fn spawn_server_child(
    data_dir: &Path,
    db_path: &Path,
    port: u16,
    generation: usize,
    mode: &str,
) -> (ServerChild, Value) {
    assert_ne!(port, LIVE_PORT, "the proof must never bind the live reviewer port");
    let ready_path = data_dir.join(format!("serving-probe-ready-{generation}.json"));
    let control_path = data_dir.join(format!("serving-probe-control-{generation}.txt"));
    let mut command = Command::new(std::env::current_exe().expect("current integration-test executable"));
    command
        .args(["--ignored", "--exact", "reviewer_serving_path_child", "--nocapture"])
        .env("CORTEX_SERVING_PROBE_CHILD", mode)
        .env("CORTEX_SERVING_PROBE_DATA_DIR", data_dir)
        .env("CORTEX_SERVING_PROBE_DB", db_path)
        .env("CORTEX_SERVING_PROBE_READY", &ready_path)
        .env("CORTEX_SERVING_PROBE_CONTROL", &control_path)
        .env("CORTEX_COUCH_PORT", port.to_string())
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = command.spawn().expect("spawn isolated server generation");
    let mut server = ServerChild { child: Some(child), control_path };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(bytes) = fs::read(&ready_path) {
            let status = serde_json::from_slice(&bytes).expect("parse ready status");
            return (server, status);
        }
        if let Some(status) = server.child.as_mut().expect("child still owned").try_wait().expect("poll child") {
            panic!("isolated server exited before readiness: {status}");
        }
        assert!(Instant::now() < deadline, "isolated server did not become ready within 30 seconds");
        thread::sleep(Duration::from_millis(25));
    }
}

fn write_wav(path: &Path, frequency_hz: f32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("create fixture WAV");
    for index in 0..24_000 {
        let phase = index as f32 * frequency_hz * std::f32::consts::TAU / 16_000.0;
        writer.write_sample((phase.sin() * 8_000.0) as i16).expect("write fixture sample");
    }
    writer.finalize().expect("finalize fixture WAV");
}

fn seed_profile(data_dir: &Path, db_path: &Path) {
    fs::create_dir_all(data_dir).expect("create isolated profile");
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let live = fs::canonicalize(PathBuf::from(appdata).join("cortex-speech")).ok();
        let isolated = fs::canonicalize(data_dir).expect("canonical isolated profile");
        assert_ne!(Some(isolated), live, "the serving proof must never target the live profile");
    }

    let db = Database::open(db_path.to_string_lossy().as_ref()).expect("open isolated database");
    db.initialize().expect("initialize isolated database");
    let champion_id = "omniasr-7b-serving-path-probe";
    registry::register_candidate(
        &db,
        &NewModelVersion {
            id: champion_id.into(),
            family: OMNIASR_7B_FAMILY.into(),
            model_card_name: Some("isolated serving-path champion".into()),
            checkpoint_sha256: "c".repeat(64),
            checkpoint_path: "/isolated/no-model-loaded.json".into(),
            source: "cortex-finetuned".into(),
            license: "owner-full-rights".into(),
        },
    )
    .expect("register isolated champion identity");
    db.connection()
        .execute("UPDATE model_versions SET status='champion' WHERE id=?1", [champion_id])
        .expect("activate isolated champion identity");

    let clips = [(FIRST_ID, "دەقی یەکەم", 330.0_f32), (SKIP_ID, "دەقی دووەم", 550.0_f32)];
    let mut members = Vec::new();
    for (id, transcript, frequency) in clips {
        let audio_path = data_dir.join(format!("{id}.wav"));
        write_wav(&audio_path, frequency);
        let mut segment = SpeechSegment {
            id: id.into(),
            audio_path: audio_path.to_string_lossy().into_owned(),
            raw_transcript: transcript.into(),
            duration_ms: CLIP_DURATION_MS,
            alignment_json: Some(
                r#"{"source_start_ms":0,"source_end_ms":1500,"chunk_index":0,"chunk_count":1}"#.into(),
            ),
            model_version_id: Some(champion_id.into()),
            ..SpeechSegment::default()
        };
        // Keep the fixture truthful if SpeechSegment defaults change in the future.
        segment.verified = false;
        db.insert_segment(&segment).expect("insert isolated pool segment");
        // Decode the file through the SAME canonical protocol the playback lease re-verifies with
        // (`current_canonical_pcm_blake3`). Hashing the in-memory sample buffer instead would agree
        // today and diverge silently the day the decode window or resampler changes.
        let identity = AudioFingerprint::identify_canonical_file(&audio_path).expect("canonical PCM identity");
        assert_eq!(db.set_audio_identity(&segment.audio_path, &identity).expect("stamp isolated PCM identity"), 1);
        members.push(PoolMemberInput { segment_id: id.into(), voice_name: VOICE_NAME.into() });
    }
    review_pool::activate(&db, "00000000-0000-4000-8000-000000000201", &members)
        .expect("activate isolated review pool");
}

fn tls_agent(data_dir: &Path) -> ureq::Agent {
    let saved: Value = serde_json::from_slice(
        &fs::read(data_dir.join("couch_tls_identity.json")).expect("read isolated TLS identity"),
    )
    .expect("parse isolated TLS identity");
    let certificate_pem = saved["certificate_pem"].as_str().expect("certificate PEM in identity");
    let mut roots = rustls::RootCertStore::empty();
    let mut pem = std::io::Cursor::new(certificate_pem.as_bytes());
    for certificate in rustls_pemfile::certs(&mut pem) {
        roots.add(certificate.expect("parse isolated TLS certificate")).expect("trust isolated TLS certificate");
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    ureq::AgentBuilder::new().tls_config(std::sync::Arc::new(config)).timeout(Duration::from_secs(10)).build()
}

fn reviewer_pairing_token(status: &Value, reviewer: &str) -> String {
    status["reviewers"]
        .as_array()
        .expect("reviewer links")
        .iter()
        .find(|item| item["name"] == reviewer)
        .and_then(|item| item["url"].as_str())
        .and_then(|url| url.split("#t=").nth(1))
        .unwrap_or_else(|| panic!("missing pairing token for {reviewer}: {status}"))
        .to_string()
}

fn claim(agent: &ureq::Agent, base: &str, token: &str) -> String {
    let response = agent
        .post(&format!("{base}/api/claim"))
        .send_json(json!({ "token": token }))
        .expect("claim isolated reviewer cookie");
    assert_eq!(response.status(), 200);
    response
        .header("Set-Cookie")
        .and_then(|value| value.split(';').next())
        .expect("claim sets reviewer cookie")
        .to_string()
}

fn queue(agent: &ureq::Agent, base: &str, cookie: &str) -> Value {
    let response =
        agent.get(&format!("{base}/api/queue")).set("Cookie", cookie).call().expect("fetch isolated reviewer queue");
    assert_eq!(response.status(), 200);
    response.into_json().expect("parse isolated queue")
}

fn find_item<'a>(queue: &'a Value, id: &str) -> Option<&'a Value> {
    queue["items"].as_array().expect("queue items").iter().find(|candidate| candidate["id"] == id)
}

fn item<'a>(queue: &'a Value, id: &str) -> &'a Value {
    find_item(queue, id).unwrap_or_else(|| panic!("queue did not contain {id}: {queue}"))
}

/// GET the clip audio, optionally under a live playback attempt. Returns the HTTP status so a
/// deliberately unauthorized request can be asserted rather than panicking the harness.
fn get_audio(agent: &ureq::Agent, base: &str, cookie: &str, id: &str, attempt: Option<&str>) -> (u16, Vec<u8>) {
    let url = match attempt {
        Some(attempt) => format!("{base}/api/audio/{id}?playbackAttemptId={attempt}"),
        None => format!("{base}/api/audio/{id}"),
    };
    match agent.get(&url).set("Cookie", cookie).call() {
        Ok(response) => {
            let status = response.status();
            assert_eq!(response.header("Content-Type"), Some("audio/wav"));
            let mut bytes = Vec::new();
            response.into_reader().read_to_end(&mut bytes).expect("read isolated audio response");
            (status, bytes)
        }
        Err(ureq::Error::Status(status, _)) => (status, Vec::new()),
        Err(error) => panic!("isolated audio transport for {id} failed: {error}"),
    }
}

/// Walk the whole policy-4 contract for one clip and return the finalized receipt id: reserve an
/// attempt, receive the authorized media under it, dwell long enough for a full traversal to be
/// physically plausible, then finalize the interval union.
fn earn_playback_receipt(
    agent: &ureq::Agent,
    base: &str,
    cookie: &str,
    id: &str,
    row_version: &str,
    client_attempt_id: &str,
) -> String {
    let started: Value = agent
        .post(&format!("{base}/api/playback/start"))
        .set("Cookie", cookie)
        .send_json(json!({ "id": id, "rowVersion": row_version, "clientAttemptId": client_attempt_id }))
        .unwrap_or_else(|error| panic!("playback start for {id} failed: {error}"))
        .into_json()
        .expect("parse playback start");
    let receipt = started["playbackReceiptId"].as_str().expect("playback start issues a receipt id").to_string();
    assert_eq!(started["segmentId"], id);
    assert_eq!(started["clipDurationMs"], CLIP_DURATION_MS);

    let (status, bytes) = get_audio(agent, base, cookie, id, Some(&receipt));
    assert_eq!(status, 200, "the authorized media request must be served under its own attempt");
    assert!(bytes.len() > 44 && bytes.starts_with(b"RIFF"), "audio route must return a real WAV");

    thread::sleep(PLAYBACK_DWELL);
    let finalized: Value = agent
        .post(&format!("{base}/api/playback/finalize"))
        .set("Cookie", cookie)
        .send_json(json!({
            "playbackReceiptId": receipt,
            "clientAttemptId": client_attempt_id,
            "intervals": [{ "startMs": 0, "endMs": CLIP_DURATION_MS }],
        }))
        .unwrap_or_else(|error| panic!("playback finalize for {id} failed: {error}"))
        .into_json()
        .expect("parse playback finalize");
    assert_eq!(finalized["playbackReceiptId"], receipt.as_str());
    assert_eq!(finalized["uniqueTraversedMs"], CLIP_DURATION_MS);
    receipt
}

fn post_decision(agent: &ureq::Agent, base: &str, cookie: &str, body: &Value) -> (u16, String) {
    match agent.post(&format!("{base}/api/decision")).set("Cookie", cookie).send_json(body.clone()) {
        Ok(response) => {
            let status = response.status();
            (status, response.into_string().unwrap_or_default())
        }
        Err(ureq::Error::Status(status, response)) => (status, response.into_string().unwrap_or_default()),
        Err(error) => panic!("isolated decision transport failed: {error}"),
    }
}

fn post_undo(agent: &ureq::Agent, base: &str, cookie: &str) -> (u16, Value) {
    let response =
        agent.post(&format!("{base}/api/undo")).set("Cookie", cookie).send_bytes(&[]).expect("isolated undo request");
    let status = response.status();
    let body = response.into_json().expect("parse isolated undo response");
    (status, body)
}

fn count(db: &Database, sql: &str, params: &[&dyn rusqlite::ToSql]) -> i64 {
    db.connection().query_row(sql, params, |row| row.get(0)).unwrap_or_else(|error| panic!("{sql}: {error}"))
}

#[test]
fn reviewer_flow_survives_real_https_retries_and_restarts() {
    let temp = tempfile::tempdir().expect("isolated serving profile");
    let data_dir = temp.path().join("profile");
    let db_path = data_dir.join("cortex-speech.db");
    seed_profile(&data_dir, &db_path);
    let port = free_non_live_port();
    let base = format!("https://127.0.0.1:{port}");

    let (server1, status1) = spawn_server_child(&data_dir, &db_path, port, 1, "start");
    assert_eq!(status1["running"], true);
    let agent = tls_agent(&data_dir);
    let one_cookie = claim(&agent, &base, &reviewer_pairing_token(&status1, REVIEWER_ONE));
    let two_cookie = claim(&agent, &base, &reviewer_pairing_token(&status1, REVIEWER_TWO));

    // ── One canonical judgement: queue -> playback contract -> two simultaneous copies of it ──────
    let first_queue = queue(&agent, &base, &one_cookie);
    let first = item(&first_queue, FIRST_ID).clone();
    let skip = item(&first_queue, SKIP_ID).clone();
    let first_row_version = first["rowVersion"].as_str().expect("served clip carries a revision").to_string();

    // Policy 4 replaced the raw traversal counters this test used to send. Both halves of that
    // replacement are asserted before any real verdict, because a page that can still buy a judgement
    // with a self-reported number is a page that can mint gold without playing the audio.
    let (legacy_status, legacy_body) = post_decision(
        &agent,
        &base,
        &one_cookie,
        &json!({
            "operationId": LEGACY_COUNTER_OPERATION,
            "id": FIRST_ID,
            "action": "accept",
            "text": first["text"],
            "rowVersion": first_row_version,
            "heardMs": CLIP_DURATION_MS,
            "clipDurationMs": CLIP_DURATION_MS,
        }),
    );
    assert_eq!(legacy_status, 428, "legacy playback counters must never authorize a verdict: {legacy_body}");
    let (unproven_status, unproven_body) = post_decision(
        &agent,
        &base,
        &one_cookie,
        &json!({
            "operationId": NO_PROOF_OPERATION,
            "id": FIRST_ID,
            "action": "accept",
            "text": first["text"],
            "rowVersion": first_row_version,
        }),
    );
    assert_eq!(unproven_status, 428, "a verdict with no finalized playback receipt must be refused: {unproven_body}");

    let receipt = earn_playback_receipt(
        &agent,
        &base,
        &one_cookie,
        FIRST_ID,
        &first_row_version,
        "00000000-0000-4000-8000-0000000002a1",
    );
    let first_body = json!({
        "operationId": FIRST_OPERATION,
        "id": FIRST_ID,
        "action": "accept",
        "text": first["text"],
        "rowVersion": first_row_version,
        "playbackReceiptId": receipt,
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut attempts = Vec::new();
    for _ in 0..2 {
        let barrier = barrier.clone();
        let base = base.clone();
        let cookie = one_cookie.clone();
        let body = first_body.clone();
        let data_dir = data_dir.clone();
        attempts.push(thread::spawn(move || {
            let agent = tls_agent(&data_dir);
            barrier.wait();
            post_decision(&agent, &base, &cookie, &body)
        }));
    }
    barrier.wait();
    let concurrent: Vec<(u16, String)> =
        attempts.into_iter().map(|join| join.join().expect("decision thread")).collect();
    assert!(concurrent.iter().any(|(status, _)| *status == 200), "one concurrent submit must commit: {concurrent:?}");
    // A racing duplicate may commit, replay, be told to retry (503), or be refused as a spent
    // operation/receipt (409/428). It may NEVER 500 — an unhandled write conflict on the paid path is
    // the shape that loses a reviewer's work with nothing in the log to reconstruct it — and the
    // durable-count assertion at the end proves no second judgement was minted either way.
    assert!(
        concurrent.iter().all(|(status, _)| matches!(*status, 200 | 409 | 428 | 503)),
        "a racing duplicate may only commit, replay, or ask for a safe retry: {concurrent:?}",
    );
    // Committing the judgement makes the clip `already_canonical`. A lost-response replay of that
    // same operation must still be acknowledged as the canonical duplicate it is, never diverted
    // into the pool path and answered 409 (owner canon 2026-09-04 retired the 503 fence here).
    let (replay_status, replay_body) = post_decision(&agent, &base, &one_cookie, &first_body);
    assert_eq!(
        replay_status, 200,
        "a lost-response replay of a canonical decision must be acknowledged: {replay_body}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&replay_body).expect("parse replay")["duplicate"],
        true,
        "the replay must be the canonical duplicate acknowledgement, never a pool 409: {replay_body}",
    );

    // ── Skip: the explicit NO-VERDICT. It writes nothing to the corpus and needs no playback proof ─
    // It also releases the lease, which is what lets the second reviewer below be offered this clip —
    // a skip hands work to somebody who can judge it rather than retiring it.
    assert!(skip["rowVersion"].is_string(), "the served clip itself still carries a revision");
    let (skip_status, skip_reply) = post_decision(
        &agent,
        &base,
        &one_cookie,
        &json!({ "operationId": SKIP_OPERATION, "id": SKIP_ID, "action": "skip", "text": "" }),
    );
    assert_eq!(skip_status, 200, "durable skip failed: {skip_reply}");
    assert_eq!(serde_json::from_str::<Value>(&skip_reply).expect("parse skip")["skipped"], true);
    let after_skip = queue(&agent, &base, &one_cookie);
    assert!(find_item(&after_skip, SKIP_ID).is_none(), "a skipped clip stops being this reviewer's work: {after_skip}",);

    // ── PAY FENCE and its MIRROR, asserted together and in both directions ───────────────────────
    // FIRST_ID now carries a canonical human answer, so a further pool observation on it is exactly
    // the unpaid work `decisions.rs` refuses. The mirror in `review_pool::pending_segment_ids` must
    // therefore keep it out of the second reviewer's queue: a queue that served it would walk that
    // reviewer into a refusal, which is how three fenced clips at the front of every queue walled all
    // ten reviewers out of a 19,905-clip savable backlog on 2026-08-31.
    //
    // The second half matters just as much. The mirror must be exactly as WIDE as the fence and no
    // wider: the skipped clip is still savable, still unfenced, and must still be served — refusing
    // it here would kill the paid canonical path, which is the only path anyone is actually using.
    // Owner canon 2026-09-04: a second opinion is paid like a first one, so the clip that already
    // holds one canonical opinion is served FIRST to a different reviewer (it is the work nearest a
    // decision), its audio streams, and the judgement lands as a pool decision with its own credit.
    let second_queue = queue(&agent, &base, &two_cookie);
    let second_first = item(&second_queue, FIRST_ID);
    assert_eq!(
        second_queue["items"][0]["id"], FIRST_ID,
        "the one-opinion clip is nearest a decision and must lead the second reviewer's queue: {second_queue}",
    );
    assert!(find_item(&second_queue, SKIP_ID).is_some(), "untouched pool work is still served: {second_queue}");
    assert_eq!(second_queue["pendingTotal"], 2, "nothing is fenced out of the pending count any more: {second_queue}");
    let (second_audio_status, second_bytes) = get_audio(&agent, &base, &two_cookie, FIRST_ID, None);
    assert_eq!(second_audio_status, 200, "a served one-opinion clip must stream its audio to the second reviewer");
    assert!(second_bytes.starts_with(b"RIFF"), "the second reviewer receives the real WAV bytes");
    let second_row_version = second_first["rowVersion"].as_str().expect("served revision").to_string();
    let (unproven_pool_status, unproven_pool_body) = post_decision(
        &agent,
        &base,
        &two_cookie,
        &json!({
            "operationId": FENCED_OPERATION,
            "id": FIRST_ID,
            "action": "accept",
            "text": first["text"],
            "rowVersion": second_row_version,
        }),
    );
    assert_eq!(
        unproven_pool_status, 428,
        "a pool second opinion needs the same policy-4 listening proof as a first opinion: {unproven_pool_body}",
    );
    let second_receipt = earn_playback_receipt(
        &agent,
        &base,
        &two_cookie,
        FIRST_ID,
        &second_row_version,
        "00000000-0000-4000-8000-0000000002b2",
    );
    let (pool_status, pool_body) = post_decision(
        &agent,
        &base,
        &two_cookie,
        &json!({
            "operationId": FENCED_OPERATION,
            "id": FIRST_ID,
            "action": "accept",
            "text": first["text"],
            "rowVersion": second_row_version,
            "playbackReceiptId": second_receipt,
        }),
    );
    assert_eq!(pool_status, 200, "a proven pool second opinion must land: {pool_body}");
    let pool_reply = serde_json::from_str::<Value>(&pool_body).expect("parse pool decision");
    assert!(pool_reply["poolDecisionId"].is_i64(), "the pool judgement must name its durable id: {pool_body}");
    let (pool_replay_status, pool_replay_body) = post_decision(
        &agent,
        &base,
        &two_cookie,
        &json!({
            "operationId": FENCED_OPERATION,
            "id": FIRST_ID,
            "action": "accept",
            "text": first["text"],
            "rowVersion": second_row_version,
            "playbackReceiptId": second_receipt,
        }),
    );
    assert_eq!(pool_replay_status, 200, "a lost-response pool replay is acknowledged: {pool_replay_body}");
    assert_eq!(
        serde_json::from_str::<Value>(&pool_replay_body).expect("parse pool replay")["duplicate"],
        true,
        "the pool replay must be a duplicate acknowledgement, not a second judgement",
    );

    // ── Restart boundary: the session, its cookies and the undo authority must survive a kill ─────
    server1.crash();
    let (server2, status2) = spawn_server_child(&data_dir, &db_path, port, 2, "resume");
    assert_eq!(status2["running"], true);
    let agent = tls_agent(&data_dir);
    // The in-memory undo stack died with generation 1, so this is served purely from the database.
    // A canonical skip pushes no undo entry (it wrote nothing), which makes the reviewer's judgement
    // on FIRST_ID the actual latest action to reverse.
    let (undo_status, undo) = post_undo(&agent, &base, &one_cookie);
    assert_eq!(undo_status, 200, "restart-safe undo failed: {undo}");
    assert_eq!(undo["id"], FIRST_ID, "restart recovery must undo the actual latest action");

    server2.crash();
    let (server3, status3) = spawn_server_child(&data_dir, &db_path, port, 3, "resume");
    assert_eq!(status3["running"], true);
    let agent = tls_agent(&data_dir);
    let (undo_replay_status, undo_replay) = post_undo(&agent, &base, &one_cookie);
    assert_eq!(undo_replay_status, 200, "lost Undo response must replay after another restart");
    assert_eq!(undo_replay["id"], FIRST_ID, "a replayed Undo must not reverse an older decision instead");

    // The undo made FIRST_ID savable again, so the mirror must hand it back — the fence and the queue
    // agree in BOTH directions, not just the refusing one.
    let requeued = queue(&agent, &base, &one_cookie);
    item(&requeued, FIRST_ID);
    let requeued_row_version = requeued["items"][0]["rowVersion"].as_str().unwrap_or_default().to_string();
    assert!(!requeued_row_version.is_empty(), "a requeued clip still carries a revision: {requeued}");
    let (requeued_audio_status, requeued_bytes) = get_audio(&agent, &base, &one_cookie, FIRST_ID, None);
    assert_eq!(requeued_audio_status, 200, "a requeued clip must be playable again");
    assert!(requeued_bytes.starts_with(b"RIFF"), "the requeued audio route must still return a real WAV");

    server3.stop();

    // ── Durable truth, read back through a fresh connection ──────────────────────────────────────
    let db = Database::open(db_path.to_string_lossy().as_ref()).expect("reopen proof database");
    db.initialize().expect("validate proof database schema");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM human_decision_effect_events WHERE segment_id=?1 AND reviewer=?2 COLLATE NOCASE",
            &[&FIRST_ID, &REVIEWER_ONE],
        ),
        1,
        "concurrency and exact replay must mint exactly one decision effect",
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM human_decision_effect_reversals", &[]),
        1,
        "the replayed Undo after a second restart must not reverse anything twice",
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM review_pool_decisions", &[]),
        1,
        "the second reviewer's proven judgement is exactly one pool decision, however often it was replayed",
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM effective_review_pool_decisions_v62", &[]),
        1,
        "the pool judgement is effective (never reversed)",
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM review_events
              WHERE segment_id=?1 AND reviewer=?2 COLLATE NOCASE AND source='couch' AND action='skip'",
            &[&SKIP_ID, &REVIEWER_ONE],
        ),
        1,
        "a skip leaves exactly one durable audit record, however often it is retried",
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM speech_segments WHERE id=?1 AND (verified=1 OR human_decision IS NOT NULL)",
            &[&SKIP_ID],
        ),
        0,
        "skip must never become a judgement",
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM review_compensation_ledger WHERE reviewer=?1 COLLATE NOCASE",
            &[&REVIEWER_TWO],
        ),
        1,
        "the second opinion earns exactly one credit (owner canon 2026-09-04), replay or not",
    );
    assert_eq!(
        count(
            &db,
            "SELECT COALESCE(SUM(delta_micro_iqd),0) FROM review_compensation_ledger
              WHERE reviewer=?1 COLLATE NOCASE AND source='couch_pool' AND compensation_action='accept'",
            &[&REVIEWER_TWO],
        ),
        750_000,
        "an unchanged pool accept earns the same 10% weight as a first-opinion accept",
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM playback_authority_consumptions_v4 WHERE namespace='independent' AND reviewer=?1 COLLATE NOCASE",
            &[&REVIEWER_TWO],
        ),
        1,
        "the second reviewer's listening authority was consumed exactly once by the pool judgement",
    );
    // Review compensation canon revision 2 (`review-iqd-v1-2026-08-21`): 18,000 IQD per full-
    // equivalent audio hour, and an unchanged `accept` earns 10% of it. A 1,500 ms clip is therefore
    // 18_000 * 1_500 / 3_600_000 * 0.10 = 0.75 IQD = 750,000 micro-IQD. Pinned as a literal so a
    // silent repricing of the phone path cannot pass this suite; a real owner change to the rate must
    // change this number in the same commit, and only under an explicit `change canon:` instruction.
    const ACCEPT_MICRO_IQD: i64 = 750_000;
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM review_compensation_ledger WHERE reviewer=?1 COLLATE NOCASE",
            &[&REVIEWER_ONE],
        ),
        3,
        "the accept credit, the zero-weight skip, and the undo reversal are each their own immutable row",
    );
    assert_eq!(
        count(
            &db,
            "SELECT delta_micro_iqd FROM review_compensation_ledger
              WHERE reviewer=?1 COLLATE NOCASE AND compensation_action='accept'",
            &[&REVIEWER_ONE],
        ),
        ACCEPT_MICRO_IQD,
        "a playback-evidenced unchanged accept earns the canon rate, not an approximation of it",
    );
    assert_eq!(
        count(
            &db,
            "SELECT delta_micro_iqd FROM review_compensation_ledger
              WHERE reviewer=?1 COLLATE NOCASE AND compensation_action='undo' AND reverses_entry_id IS NOT NULL",
            &[&REVIEWER_ONE],
        ),
        -ACCEPT_MICRO_IQD,
        "the Undo appends a signed reversal that names the credit it reverses; paid history is never rewritten",
    );
    assert_eq!(
        count(
            &db,
            "SELECT COALESCE(SUM(delta_micro_iqd), 0) FROM review_compensation_ledger WHERE reviewer=?1 COLLATE NOCASE",
            &[&REVIEWER_ONE],
        ),
        0,
        "an undone judgement must leave a net-zero balance, credit and reversal both still visible",
    );

    // OWNER CANON 2026-08-29: a sentence is decided by any two DIFFERENT reviewers. The second opinion
    // is exactly what the pay fence refuses, so while the fence stands NO pool clip can be resolved
    // through the phone. This is the fence's real cost, and it is asserted rather than left implicit —
    // the day an owner pay contract prices pool work, this expectation must change WITH the fence.
    let resolution = review_pool::resolution_summary(&db).expect("read pool resolution summary");
    assert_eq!(resolution.total_clips, 2);
    assert_eq!(resolution.resolved_clips, 0, "no clip can be decided while the second opinion is fenced");
    assert_eq!(resolution.owner_conflicts, 0);
    assert_eq!(resolution.needs_third_review, 0);
    assert_eq!(
        resolution.needs_first_or_second_review, 2,
        "the undone judgement and the skipped clip both return to pending work",
    );
}
