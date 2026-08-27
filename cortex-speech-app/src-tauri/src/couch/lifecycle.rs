use super::*;

/// Where a started session is remembered, so a link keeps working after the app is closed and
/// reopened (`{data_dir}/couch_session.json`).
pub(super) const SESSION_FILE: &str = "couch_session.json";
/// Durable deny marker written by Stop before the server is torn down. If deleting a stale session
/// file fails (permissions/AV/disk fault), startup still refuses to resurrect its credentials.
pub(super) const SESSION_REVOCATION_FILE: &str = "couch_session.revoked";

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SavedTlsIdentity {
    pub(super) certificate_pem: String,
    pub(super) protected_private_key_pem: String,
    pub(super) subject_alt_names: Vec<String>,
}

pub(super) struct TlsIdentity {
    pub(super) certificate_pem: Vec<u8>,
    pub(super) private_key_pem: Vec<u8>,
    pub(super) fingerprint: String,
}

pub(super) fn tls_subject_alt_names() -> Vec<String> {
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

pub(super) fn certificate_der_from_pem(pem: &str) -> Result<Vec<u8>, String> {
    let encoded: String = pem.lines().filter(|line| !line.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| format!("decode Couch TLS certificate PEM: {e}"))
}

pub(super) fn certificate_fingerprint(pem: &str) -> Result<String, String> {
    let digest = Sha256::digest(certificate_der_from_pem(pem)?);
    Ok(digest.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(":"))
}

pub(super) fn generate_tls_identity(subject_alt_names: &[String]) -> Result<TlsIdentity, String> {
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

pub(super) fn save_tls_identity(
    data_dir: &Path,
    identity: &TlsIdentity,
    subject_alt_names: &[String],
) -> Result<(), String> {
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

pub(super) fn load_tls_identity(data_dir: &Path, required_names: &[String]) -> Result<Option<TlsIdentity>, String> {
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

pub(super) fn load_or_create_tls_identity(data_dir: Option<&Path>) -> Result<TlsIdentity, String> {
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
pub(super) struct SavedSession {
    /// token -> reviewer name, the same map `CouchState::reviewers` holds at runtime.
    pub(super) reviewers: HashMap<String, String>,
    /// The database the session was serving. A session remembered against a DIFFERENT library must
    /// not silently resume: the links would hand out clips from a corpus the owner did not intend.
    pub(super) db_path: String,
    /// (segment id, reviewer) pairs served as SPOT CHECKS and not yet answered (phase 4 of
    /// docs/REMOTE_PUBLIC_LINKS_PLAN.md). Durable sessions make restarts routine, and a check served
    /// before a restart used to become unanswerable after it: the in-memory served-set was gone, so
    /// the submit fell into the already-reviewed 409 — score silently lost, reviewer shown a jarring
    /// error for doing exactly what they were asked. `serde(default)` so a pre-phase-4 session file
    /// still loads (missing field = empty set).
    #[serde(default)]
    pub(super) spot_checks: Vec<(String, String)>,
    #[serde(default)]
    pub(super) pilot_spot_checks: Vec<(String, String)>,
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
    pub(super) sessions: Vec<SavedCookieSession>,
    /// Bound policy survives an app/watchdog restart, so changing a baseline cannot silently reset a
    /// live paid pilot. Older unrestricted sessions deserialize as `None`.
    #[serde(default)]
    pub(super) pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
}

/// One remembered cookie session. Same protection as a pairing token: it is a credential.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedCookieSession {
    /// DPAPI-protected session token — the cookie value the browser will present.
    pub(super) token: String,
    pub(super) reviewer: String,
    /// Seconds since the epoch. Wall clock because an `Instant` cannot cross a restart, which is the
    /// entire point of writing this down. Restoring compares it against `COUCH_SESSION_TTL`, so a
    /// session that expired while the app was closed stays expired instead of being resurrected.
    pub(super) issued_unix: u64,
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
pub(super) fn session_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SESSION_FILE)
}

pub(super) fn session_revocation_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SESSION_REVOCATION_FILE)
}

/// Establish the durable revocation fact before reporting Stop success or shutting down the live
/// server. A failure leaves the live handle registered, so the owner gets an honest error instead of
/// a false "stopped" state whose old links return on restart.
pub(super) fn write_session_revocation(data_dir: &Path) -> Result<(), String> {
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

pub(super) fn clear_session_revocation(data_dir: &Path) -> Result<(), String> {
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
pub(super) fn durable_pairing_codes(st: &CouchState) -> HashMap<String, String> {
    if st.pairing_codes.is_empty() {
        st.reviewers.clone()
    } else {
        st.pairing_codes.clone()
    }
}

pub(super) struct SessionSaveSnapshot {
    pub(super) dir: PathBuf,
    pub(super) db_path: String,
    pub(super) reviewers: HashMap<String, String>,
    pub(super) spot_checks: HashSet<(String, String)>,
    pub(super) pilot_spot_checks: HashSet<(String, String)>,
    pub(super) sessions: HashMap<String, (String, SystemTime)>,
    pub(super) pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
    #[cfg(test)]
    fail_cookie_protection_for: Option<String>,
}

pub(super) struct SessionSaveInput<'a> {
    pub(super) data_dir: &'a Path,
    pub(super) reviewers: &'a HashMap<String, String>,
    pub(super) db_path: &'a str,
    pub(super) spot_checks: &'a HashSet<(String, String)>,
    pub(super) pilot_spot_checks: &'a HashSet<(String, String)>,
    pub(super) sessions: &'a HashMap<String, (String, SystemTime)>,
    pub(super) pilot_policy: Option<&'a crate::review_pilot::ReviewPilotPolicy>,
}

pub(super) fn snapshot_session_save(st: &CouchState) -> Option<SessionSaveSnapshot> {
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

pub(super) fn save_session_snapshot(snapshot: &SessionSaveSnapshot) -> Result<(), String> {
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

pub(super) fn save_session_snapshot_with_cookie_protector<F>(
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
pub(super) fn lock_session_persist() -> std::sync::MutexGuard<'static, ()> {
    SESSION_PERSIST.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn persist_session_state(state: &Mutex<CouchState>) -> Result<(), String> {
    persist_session_state_with_hook(state, || {})
}

pub(super) fn persist_session_state_with_hook<F: FnOnce()>(
    state: &Mutex<CouchState>,
    after_snapshot: F,
) -> Result<(), String> {
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
pub(super) fn snapshot_live_sessions(st: &CouchState) -> HashMap<String, (String, SystemTime)> {
    snapshot_sessions_from_maps(&st.reviewers, &st.session_issued)
}

pub(super) fn snapshot_sessions_from_maps(
    reviewers: &HashMap<String, String>,
    session_issued: &HashMap<String, SystemTime>,
) -> HashMap<String, (String, SystemTime)> {
    reviewers
        .iter()
        .filter_map(|(token, name)| session_issued.get(token).map(|at| (token.clone(), (name.clone(), *at))))
        .collect()
}

pub(super) fn save_session(
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

pub(super) fn save_session_with_cookie_protector<F>(
    input: SessionSaveInput<'_>,
    protect_cookie: F,
) -> Result<(), String>
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

pub(super) fn clear_session(data_dir: &Path) -> Result<(), String> {
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
pub(super) struct RememberedSession {
    /// Pairing token -> reviewer name. The long-lived secret in the link the owner sends.
    pub(super) pairing: HashMap<String, String>,
    /// Served-but-unanswered spot checks.
    pub(super) spot_checks: HashSet<(String, String)>,
    /// Pilot checks already served, including answered ones; used to prevent reloads minting extras.
    pub(super) pilot_spot_checks: HashSet<(String, String)>,
    /// Cookie session token -> (reviewer, issued). Already filtered to sessions still within
    /// `COUCH_SESSION_TTL`: restoring is resuming, never extending.
    pub(super) sessions: HashMap<String, (String, SystemTime)>,
    /// The policy snapshot written when this durable session was started.
    pub(super) pilot_policy: Option<crate::review_pilot::ReviewPilotPolicy>,
}

/// Read back a remembered session, or None if there is none / it is unreadable / it belongs to a
/// different library.
pub(super) fn load_session(data_dir: &Path, db_path: &str) -> Option<RememberedSession> {
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
pub(super) fn configured_port() -> u16 {
    port_from(std::env::var("CORTEX_COUCH_PORT").ok().as_deref())
}

/// The parsing half of [`configured_port`], split out so the FALLBACK can be tested. Reading the
/// variable is the untestable part (process-global, and mutating the environment mid-test is unsound
/// with other threads running); deciding what a bad value means is the part that matters, because
/// getting it wrong takes the owner's phone link down rather than merely ignoring a typo.
pub(super) fn port_from(raw: Option<&str>) -> u16 {
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
pub(super) fn resume_on_port(db_path: String, data_dir: &Path, port: u16) -> Option<CouchStatus> {
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
pub(super) fn start_on_port(
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
pub(super) fn start_on_port_with_session_lifecycle<F, G>(
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
    if crate::database_runtime::restore_pending() {
        return Err(crate::database_runtime::RESTORE_IN_PROGRESS_MSG.to_string());
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
    let preflight_db = Database::open(&db_path).map_err(|e| format!("Couch Review cannot open the library: {e}"))?;
    // The pool is LOADED here (a corrupt registry must still refuse to start) but its presence no
    // longer refuses startup. Flexible-pool work has no owner-approved compensation contract, so it
    // must not be served for free — but that refusal belongs on the pool DECISION branch in
    // `decisions.rs`, not here: a pool row is permanent once activated, so refusing at `start` took
    // down the FIRST-pass canonical path with it. That path is fully paid under
    // review-iqd-v1-2026-08-21, and on the live library it is the only path any reviewer uses
    // (measured 2026-08-27: 372 credits, ~5,299 IQD, all first-pass, zero pool decisions ever).
    // Refusing it stops phone review dead with only a PAY_POLICY_REQUIRED string to explain why.
    let configured_pool = crate::review_pool::load(&preflight_db)
        .map_err(|error| format!("flexible review pool cannot start: {error}"))?;
    // The flexible pool explicitly supersedes the old Rubar→Alle serving policy. Its historical
    // setting and any evidence remain in SQLite (and continue blocking generic export), but they no
    // longer constrain the live roster or queue once an immutable pool is active.
    let configured_campaign = if configured_pool.is_some() {
        None
    } else {
        crate::review_campaign::load(&preflight_db)
            .map_err(|error| format!("sequential review campaign cannot start: {error}"))?
    };
    if configured_pilot.is_some() && (configured_campaign.is_some() || configured_pool.is_some()) {
        return Err("controlled pilot cannot be active with another review policy".to_string());
    }
    if let Some(policy) = configured_pilot.as_ref() {
        if !policy.matches_session(&names) {
            return Err(format!(
                "controlled review pilot requires exactly these two reviewers: {}",
                policy.reviewer_names().join(", ")
            ));
        }
    }
    if let Some(policy) = configured_campaign.as_ref() {
        if names.len() != 1 || !policy.matches_reviewer(&names[0]) {
            return Err(match policy.authorized_reviewer() {
                Some(reviewer) => format!(
                    "sequential campaign phase {} requires exactly reviewer {reviewer}",
                    policy.phase().as_str()
                ),
                None => format!(
                    "sequential campaign phase {} does not permit a live reviewer session",
                    policy.phase().as_str()
                ),
            });
        }
        let Some(dir) = data_dir.as_deref() else {
            return Err("sequential review requires the durable app data directory".to_string());
        };
        crate::review_campaign::validate_focus(dir, policy)
            .map_err(|error| format!("sequential review campaign cannot start: {error}"))?;
        let maximum = preflight_db.max_review_event_id().map_err(|error| error.to_string())?;
        if policy.activated_at_review_event_id > maximum {
            return Err("sequential review campaign activation boundary is ahead of durable history".to_string());
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
            let existing =
                remembered.pairing.iter().find(|(_, n)| same_reviewer(n.as_str(), name)).map(|(t, _)| t.clone());
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
    let mut durable_pilot_served_checks = HashSet::new();
    if let Some(policy) = configured_pilot.as_ref() {
        if policy.after_review_event_id > preflight_db.max_review_event_id().map_err(|error| error.to_string())? {
            return Err("controlled review pilot baseline is ahead of durable review history".to_string());
        }
        let limit = build_pilot_decision_limit(policy)?;
        preflight_db
            .review_decision_progress(&limit)
            .map_err(|error| format!("controlled review pilot history is invalid: {error}"))?;
        let policy_sha256 = policy.policy_sha256()?;
        for name in &names {
            let quota = pilot_spot_check_quota(policy, name)?;
            let remembered_ids: Vec<String> = pilot_served_checks
                .iter()
                .filter(|(_, reviewer)| reviewer.eq_ignore_ascii_case(name))
                .map(|(segment_id, _)| segment_id.clone())
                .collect();
            let durable = preflight_db
                .reserve_review_pilot_hidden_keys(
                    &policy_sha256,
                    policy.after_review_event_id,
                    name,
                    &remembered_ids,
                    quota,
                )
                .map_err(|error| format!("controlled review pilot hidden-key history is invalid: {error}"))?;
            durable_pilot_served_checks.extend(durable.into_iter().map(|segment_id| (segment_id, name.clone())));
        }
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
    //
    // RE-KEYED onto the roster's current spelling, not merely filtered by it. Every later lookup is an
    // exact tuple match (`spot_checks.contains(&(id, reviewer))`), so a pair rehydrated under
    // yesterday's casing would be invisible to the reviewer it belongs to — the check would be
    // outstanding on the phone and unknown to the server.
    let mut spot_checks: HashSet<(String, String)> = served_checks
        .into_iter()
        .filter_map(|(segment_id, name)| {
            let current = names.iter().find(|current| same_reviewer(current.as_str(), &name))?;
            Some((segment_id, current.clone()))
        })
        .collect();
    let pilot_spot_checks: HashSet<(String, String)> = durable_pilot_served_checks
        .into_iter()
        .filter_map(|(segment_id, name)| {
            let current = names.iter().find(|current| same_reviewer(current.as_str(), &name))?;
            Some((segment_id, current.clone()))
        })
        .collect();
    // The remembered file is now only a cache. A lost/repaired file is rebuilt from SQLite, and a
    // stale remembered key had to be transactionally imported above before it can authorize audio.
    spot_checks.extend(pilot_spot_checks.iter().cloned());
    let session_store = data_dir.as_ref().map(|dir| (dir.clone(), db_path.clone()));
    // Cookie sessions come back with the server, for names still on the roster. Without this the
    // browser holds a valid cookie the server has never heard of, answers 401, and the page reports
    // "link expired" for a link that is fine — the amnesia that silenced six of eight reviewers.
    // Filtered by name so a reviewer REMOVED from the roster does not keep a working session.
    // Also re-keyed onto the current spelling: the session's reviewer String is copied onto every row
    // this cookie decides, and attribution must read the same everywhere.
    let restored: HashMap<String, (String, SystemTime)> = remembered
        .sessions
        .into_iter()
        .filter_map(|(token, (name, issued))| {
            names
                .iter()
                .find(|current| same_reviewer(current.as_str(), &name))
                .map(|current| (token, (current.clone(), issued)))
        })
        .collect();
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
        campaign_policy: configured_campaign,
        pool_policy: configured_pool,
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
pub(super) fn shutdown_unpublished_handle(mut handle: CouchHandle) {
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
pub(super) fn release_listener(port: u16) {
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
pub(super) fn revoke_in(state: &Mutex<CouchState>, name: &str) -> Result<(), String> {
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
    let revoked_session_bindings: Vec<String> = st
        .reviewers
        .iter()
        .filter(|(_, reviewer)| reviewer.eq_ignore_ascii_case(name))
        .map(|(token, _)| couch_session_binding_sha256(token))
        .collect();
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
    for binding in revoked_session_bindings {
        st.remove_playback_attempts_for_session(&binding);
    }

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

pub(super) fn status_of(h: &CouchHandle) -> CouchStatus {
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
pub(super) fn lan_ip() -> String {
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
pub(super) fn funnel_host(port: u16) -> Option<String> {
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
pub(super) fn funnel_host_from_status(parsed: &serde_json::Value, port: u16) -> Option<String> {
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
pub(super) fn tailscale_ip() -> Option<String> {
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

pub(super) fn spawn_server_loop(
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
