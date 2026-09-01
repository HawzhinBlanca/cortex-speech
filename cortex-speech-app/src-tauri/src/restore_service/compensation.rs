//! Compensation-ledger semantic validation for staged restore generations.

fn exact_review_entitlement(duration_ms: i64, basis_points: i64) -> Result<i64, String> {
    if duration_ms <= 0 || !(0..=10_000).contains(&basis_points) {
        return Err("review compensation has invalid duration or basis points".to_string());
    }
    let numerator = i128::from(duration_ms)
        .checked_mul(i128::from(crate::db::REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR))
        .and_then(|value| value.checked_mul(i128::from(basis_points)))
        .ok_or_else(|| "review compensation arithmetic overflow".to_string())?;
    let denominator = 3_600_000_i128 * 10_000_i128;
    if numerator % denominator != 0 {
        return Err("review compensation duration/rate is not an exact micro-IQD amount".to_string());
    }
    i64::try_from(numerator / denominator)
        .map_err(|_| "review compensation entitlement exceeds the supported integer range".to_string())
}

fn review_action_basis_points(action: &str) -> Option<i64> {
    match action {
        "edit" => Some(crate::db::REVIEW_PAY_EDIT_BPS),
        "accept" => Some(crate::db::REVIEW_PAY_ACCEPT_BPS),
        "reject" => Some(crate::db::REVIEW_PAY_REJECT_BPS),
        "skip" => Some(crate::db::REVIEW_PAY_SKIP_BPS),
        _ => None,
    }
}

pub(super) fn is_canonical_lowercase_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).map(|parsed| parsed.hyphenated().to_string() == value).unwrap_or(false)
}

pub(super) fn is_canonical_lowercase_64_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_compensation_reviewer(reviewer: &str) -> bool {
    !reviewer.is_empty()
        && reviewer == reviewer.trim()
        && reviewer.chars().count() <= 40
        && !reviewer.chars().any(char::is_control)
}

fn canonical_work_id_has_writer_shape(work_id: &str, reviewer: &str, duration_ms: i64) -> bool {
    let reviewer_key = reviewer.trim().to_lowercase();
    let prefix = format!("reviewer-work-v1:{}:{reviewer_key}:audio-segment-v1:", reviewer_key.len());
    let Some(audio_identity) = work_id.strip_prefix(&prefix) else {
        return false;
    };
    let mut parts = audio_identity.split(':');
    let (Some(content_hash), Some(start), Some(end), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(start), Ok(end)) = (start.parse::<i64>(), end.parse::<i64>()) else {
        return false;
    };
    is_canonical_lowercase_64_hex(content_hash) && crate::db::source_span_matches_duration(start, end, duration_ms)
}

fn canonical_work_audio_identity<'a>(work_id: &'a str, reviewer: &str) -> Option<(&'a str, i64, i64)> {
    let reviewer_key = reviewer.trim().to_lowercase();
    let prefix = format!("reviewer-work-v1:{}:{reviewer_key}:audio-segment-v1:", reviewer_key.len());
    let audio_identity = work_id.strip_prefix(&prefix)?;
    let mut parts = audio_identity.split(':');
    let content_hash = parts.next()?;
    let start = parts.next()?.parse::<i64>().ok()?;
    let end = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || !is_canonical_lowercase_64_hex(content_hash) || start < 0 || end <= start {
        return None;
    }
    Some((content_hash, start, end))
}

/// Reproduce `Database::compensation_audio_identity_tx` and the reviewer namespace byte for byte.
/// Restore validation cannot trust a ledger's self-declared work id: a forged target could otherwise
/// split one clip into several invented work ids and earn the full rate on every split.
fn canonical_compensation_work(
    db: &crate::db::Database,
    segment_id: &str,
    reviewer: &str,
    decision_revision: i64,
) -> Result<Option<(String, i64)>, String> {
    use rusqlite::OptionalExtension;

    if !valid_compensation_reviewer(reviewer) {
        return Err("database restore refused: compensation row has an invalid reviewer identity".to_string());
    }
    let row: Option<(Option<String>, Option<String>, i64, i64)> = db
        .connection()
        .query_row(
            "SELECT audio_content_hash, alignment_json, duration_ms, COALESCE(review_revision, 0)
               FROM speech_segments WHERE id = ?1",
            [segment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| format!("restore target compensation segment identity is unreadable: {error}"))?;
    let Some((content_hash, alignment_json, duration_ms, current_revision)) = row else {
        return Ok(None);
    };
    if current_revision < decision_revision || current_revision < 0 {
        return Err(format!(
            "database restore refused: compensation segment {segment_id} regresses its decision revision"
        ));
    }
    if current_revision != decision_revision {
        return Ok(None);
    }
    if duration_ms <= 0 {
        return Err(format!(
            "database restore refused: current compensation segment {segment_id} has invalid duration"
        ));
    }
    let content_hash = content_hash.as_deref().map(str::trim).filter(|value| !value.is_empty()).ok_or_else(|| {
        format!("database restore refused: compensation segment {segment_id} has fallback audio identity")
    })?;
    let alignment_json = alignment_json.as_deref().ok_or_else(|| {
        format!("database restore refused: compensation segment {segment_id} has no source-span identity")
    })?;
    let alignment: serde_json::Value = serde_json::from_str(alignment_json).map_err(|_| {
        format!("database restore refused: compensation segment {segment_id} has invalid source-span identity")
    })?;
    let start = alignment.get("source_start_ms").and_then(serde_json::Value::as_i64);
    let end = alignment.get("source_end_ms").and_then(serde_json::Value::as_i64);
    let (Some(start), Some(end)) = (start, end) else {
        return Err(format!(
            "database restore refused: compensation segment {segment_id} has incomplete source-span identity"
        ));
    };
    if !crate::db::source_span_matches_duration(start, end, duration_ms) {
        return Err(format!(
            "database restore refused: compensation segment {segment_id} source span disagrees with decoded duration"
        ));
    }
    let reviewer_key = reviewer.trim().to_lowercase();
    let audio_work_id = format!("audio-segment-v1:{content_hash}:{start}:{end}");
    Ok(Some((format!("reviewer-work-v1:{}:{reviewer_key}:{audio_work_id}", reviewer_key.len()), duration_ms)))
}

/// Re-derive the current compensation ledger and settlements from their immutable inputs. Schema
/// triggers protect future writes, but a restored database may contain pre-existing forged extras;
/// this read-only pass proves their complete arithmetic/identity semantics before page publication.
pub(crate) fn validate_review_compensation_semantics(db: &crate::db::Database) -> Result<(), String> {
    use rusqlite::OptionalExtension;

    #[derive(Clone)]
    struct Event {
        segment_id: String,
        reviewer: String,
        action: String,
        compensation_action: Option<String>,
        source: String,
        duration_ms: Option<i64>,
        operation_id: Option<String>,
        operation_payload_hash: Option<String>,
        requested_action: Option<String>,
        requested_transcript: Option<String>,
        served_transcript: Option<String>,
        served_revision: Option<i64>,
    }

    #[derive(Clone)]
    struct Ledger {
        id: i64,
        entry_id: String,
        entry_key: String,
        review_event_id: Option<i64>,
        canonical_work_id: String,
        canonical_identity_kind: String,
        reviewer: String,
        segment_id: String,
        source: String,
        compensation_action: String,
        effective_decision: String,
        decision_revision: Option<i64>,
        duration_ms: i64,
        rate_basis_points: i64,
        entitlement_micro_iqd: i64,
        delta_micro_iqd: i64,
        corrected_entitlement_ms: i64,
        delta_corrected_ms: i64,
        reverses_entry_id: Option<String>,
    }

    let mut policy_statement = db
        .connection()
        .prepare(
            "SELECT policy_version, effective_after_event_id, base_rate_micro_iqd_per_hour,
                    edit_basis_points, accept_basis_points, reject_basis_points, skip_basis_points
               FROM review_compensation_policies ORDER BY policy_version",
        )
        .map_err(|error| format!("restore target compensation policy is unreadable: {error}"))?;
    let policies = policy_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|error| format!("restore target compensation policy is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target compensation policy is unreadable: {error}"))?;
    drop(policy_statement);
    if policies.len() != 1 || policies[0].0 != crate::db::REVIEW_PAY_POLICY_VERSION {
        return Err(format!(
            "database restore refused: target must contain only the exact {} compensation policy row",
            crate::db::REVIEW_PAY_POLICY_VERSION
        ));
    }
    let policy = &policies[0];
    if (policy.2, policy.3, policy.4, policy.5, policy.6)
        != (
            crate::db::REVIEW_PAY_BASE_RATE_MICRO_IQD_PER_HOUR,
            crate::db::REVIEW_PAY_EDIT_BPS,
            crate::db::REVIEW_PAY_ACCEPT_BPS,
            crate::db::REVIEW_PAY_REJECT_BPS,
            crate::db::REVIEW_PAY_SKIP_BPS,
        )
    {
        return Err(
            "database restore refused: target compensation policy constants differ from this binary".to_string()
        );
    }
    let cutoff = policy.1;
    let effect_event_frontier: i64 = db
        .connection()
        .query_row(
            "SELECT effective_after_review_event_id FROM review_effect_state WHERE singleton_key = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("restore target review-effect frontier is unreadable: {error}"))?;
    let maximum_event_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("restore target compensation cutoff cannot be verified: {error}"))?;
    if cutoff < 0 || cutoff > maximum_event_id {
        return Err(format!(
            "database restore refused: target compensation cutoff {cutoff} is outside review history 0..={maximum_event_id}"
        ));
    }

    let mut event_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, reviewer, action, compensation_action, source, duration_ms,
                    operation_id, operation_payload_hash, requested_action,
                    requested_transcript, served_transcript, served_revision
               FROM review_events WHERE id > ?1 ORDER BY id",
        )
        .map_err(|error| format!("restore target prospective compensation events are unreadable: {error}"))?;
    let event_rows = event_statement
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Event {
                    segment_id: row.get(1)?,
                    reviewer: row.get(2)?,
                    action: row.get(3)?,
                    compensation_action: row.get(4)?,
                    source: row.get(5)?,
                    duration_ms: row.get(6)?,
                    operation_id: row.get(7)?,
                    operation_payload_hash: row.get(8)?,
                    requested_action: row.get(9)?,
                    requested_transcript: row.get(10)?,
                    served_transcript: row.get(11)?,
                    served_revision: row.get(12)?,
                },
            ))
        })
        .map_err(|error| format!("restore target prospective compensation events are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target prospective compensation events are unreadable: {error}"))?;
    drop(event_statement);
    let events = event_rows.into_iter().collect::<std::collections::HashMap<_, _>>();

    let mut operation_ids = std::collections::HashSet::<String>::new();
    for (event_id, event) in &events {
        if !valid_compensation_reviewer(&event.reviewer)
            || !matches!(event.source.as_str(), "couch" | "couch_spot_check")
            || !matches!(event.action.as_str(), "accept" | "edit" | "reject" | "skip")
        {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} is not a valid production Couch action"
            ));
        }
        let compensation_action = event.compensation_action.as_deref().ok_or_else(|| {
            format!("database restore refused: post-cutoff review event {event_id} has no compensation action")
        })?;
        let requested_action = event.requested_action.as_deref().unwrap_or_default();
        let requested_transcript = event.requested_transcript.as_deref().unwrap_or_default();
        let served_transcript = event.served_transcript.as_deref().unwrap_or_default();
        let served_revision = event.served_revision.unwrap_or(-1);
        let expected_compensation = match requested_action {
            "skip" => Some("skip"),
            "bad" | "reject" => Some("reject"),
            "accept" | "edit" => Some(
                if crate::normalizer::learning_text_key(requested_transcript)
                    == crate::normalizer::learning_text_key(served_transcript)
                {
                    "accept"
                } else {
                    "edit"
                },
            ),
            _ => None,
        };
        let valid_action_pair = if event.source == "couch_spot_check" {
            compensation_action == event.action && expected_compensation == Some(compensation_action)
        } else {
            match event.action.as_str() {
                "skip" => compensation_action == "skip" && expected_compensation == Some("skip"),
                "reject" => compensation_action == "reject" && expected_compensation == Some("reject"),
                // Corpus provenance may reclassify an unchanged earlier human correction as edit
                // while pay remains an accept, or an alternate ASR hypothesis as accept while pay
                // remains an edit. Both are deliberate writer outcomes; no other cross-pair is.
                "accept" | "edit" => {
                    matches!(compensation_action, "accept" | "edit")
                        && expected_compensation == Some(compensation_action)
                }
                _ => false,
            }
        };
        if review_action_basis_points(compensation_action).is_none() || !valid_action_pair {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} has invalid action/pay semantics"
            ));
        }
        if requested_transcript != crate::db::to_nfc(requested_transcript.trim())
            || served_transcript.is_empty()
            || served_transcript != crate::db::to_nfc(served_transcript.trim())
            || served_revision < 0
        {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} has invalid served/request evidence"
            ));
        }
        let operation_id = event.operation_id.as_deref().unwrap_or_default();
        if !is_canonical_lowercase_uuid(operation_id) || !operation_ids.insert(operation_id.to_string()) {
            return Err(format!(
                "database restore refused: post-cutoff Couch event {event_id} lacks a unique canonical lowercase UUID"
            ));
        }
        let operation_hash = event.operation_payload_hash.as_deref().unwrap_or_default();
        if !is_canonical_lowercase_64_hex(operation_hash) {
            return Err(format!(
                "database restore refused: post-cutoff Couch event {event_id} lacks a canonical payload hash"
            ));
        }
        if !event.duration_ms.is_some_and(|duration| duration > 0) {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} has no valid durable duration"
            ));
        }
        if event.source == "couch_spot_check" {
            let exact_results: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM spot_checks
                      WHERE segment_id = ?1 AND reviewer = ?2 COLLATE NOCASE AND action = ?3",
                    rusqlite::params![event.segment_id, event.reviewer, event.action],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target spot-check compensation evidence is unreadable: {error}"))?;
            if exact_results != 1 {
                return Err(format!(
                    "database restore refused: hidden review event {event_id} lacks its exact immutable spot-check result"
                ));
            }
        }
    }

    let mut ledger_statement = db
        .connection()
        .prepare(
            "SELECT id, entry_id, entry_key, review_event_id, canonical_work_id, canonical_identity_kind,
                    reviewer, segment_id, source, compensation_action, effective_decision,
                    decision_revision, duration_ms, rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                    corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id
               FROM review_compensation_ledger
              WHERE policy_version = ?1 ORDER BY id",
        )
        .map_err(|error| format!("restore target compensation ledger is unreadable: {error}"))?;
    let ledger_rows = ledger_statement
        .query_map([crate::db::REVIEW_PAY_POLICY_VERSION], |row| {
            Ok(Ledger {
                id: row.get(0)?,
                entry_id: row.get(1)?,
                entry_key: row.get(2)?,
                review_event_id: row.get(3)?,
                canonical_work_id: row.get(4)?,
                canonical_identity_kind: row.get(5)?,
                reviewer: row.get(6)?,
                segment_id: row.get(7)?,
                source: row.get(8)?,
                compensation_action: row.get(9)?,
                effective_decision: row.get(10)?,
                decision_revision: row.get(11)?,
                duration_ms: row.get(12)?,
                rate_basis_points: row.get(13)?,
                entitlement_micro_iqd: row.get(14)?,
                delta_micro_iqd: row.get(15)?,
                corrected_entitlement_ms: row.get(16)?,
                delta_corrected_ms: row.get(17)?,
                reverses_entry_id: row.get(18)?,
            })
        })
        .map_err(|error| format!("restore target compensation ledger is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target compensation ledger is unreadable: {error}"))?;
    drop(ledger_statement);

    let mut event_entry_counts = std::collections::HashMap::<i64, usize>::new();
    let mut entries = std::collections::HashMap::<String, Ledger>::new();
    let mut entry_keys = std::collections::HashSet::<String>::new();
    let mut reversed_entries = std::collections::HashSet::<String>::new();
    let mut balances = std::collections::HashMap::<String, i64>::new();
    let mut corrected_balances = std::collections::HashMap::<String, i64>::new();
    for ledger in &ledger_rows {
        if ledger.id <= 0
            || !is_canonical_lowercase_uuid(&ledger.entry_id)
            || !valid_compensation_reviewer(&ledger.reviewer)
            || !entry_keys.insert(ledger.entry_key.clone())
        {
            return Err(format!(
                "database restore refused: compensation ledger entry {} has invalid or duplicate durable identity",
                ledger.entry_id
            ));
        }
        let decision_revision = ledger.decision_revision.ok_or_else(|| {
            format!("database restore refused: compensation ledger entry {} has no decision revision", ledger.entry_id)
        })?;
        if ledger.canonical_identity_kind != "audio_content_hash+source_span"
            || !canonical_work_id_has_writer_shape(&ledger.canonical_work_id, &ledger.reviewer, ledger.duration_ms)
            || ledger.duration_ms <= 0
            || decision_revision < 0
        {
            return Err(format!(
                "database restore refused: compensation ledger entry {} disagrees with canonical segment/work identity",
                ledger.entry_id
            ));
        }
        if let Some((expected_work_id, segment_duration)) =
            canonical_compensation_work(db, &ledger.segment_id, &ledger.reviewer, decision_revision)?
        {
            if ledger.canonical_work_id != expected_work_id || ledger.duration_ms != segment_duration {
                return Err(format!(
                    "database restore refused: current compensation ledger entry {} disagrees with its segment identity",
                    ledger.entry_id
                ));
            }
        }
        let prior = *balances.get(&ledger.canonical_work_id).unwrap_or(&0);
        let prior_corrected = *corrected_balances.get(&ledger.canonical_work_id).unwrap_or(&0);

        if let Some(event_id) = ledger.review_event_id {
            *event_entry_counts.entry(event_id).or_default() += 1;
            let event = events.get(&event_id).ok_or_else(|| {
                format!(
                    "database restore refused: compensation ledger entry {} points outside the post-cutoff event range",
                    ledger.entry_id
                )
            })?;
            let expected_action = event
                .compensation_action
                .as_deref()
                .ok_or_else(|| format!("database restore refused: event {event_id} has no compensation action"))?;
            let event_duration = event
                .duration_ms
                .ok_or_else(|| format!("database restore refused: event {event_id} has no durable duration"))?;
            if ledger.compensation_action != expected_action
                || ledger.effective_decision != event.action
                || ledger.segment_id != event.segment_id
                || ledger.reviewer.trim().to_lowercase() != event.reviewer.trim().to_lowercase()
                || ledger.source != event.source
                || ledger.duration_ms != event_duration
                || ledger.entry_key != format!("review-event:{event_id}")
                || ledger.reverses_entry_id.is_some()
                || (event.source == "couch" && event.action != "skip" && decision_revision == 0)
                || ((event.source == "couch_spot_check" || event.action == "skip")
                    && event.served_revision != Some(decision_revision))
            {
                return Err(format!(
                    "database restore refused: compensation ledger entry {} disagrees with review event {event_id}",
                    ledger.entry_id
                ));
            }
            if event.action != "skip" && event_id > effect_event_frontier {
                let receipt_revision = event
                    .served_revision
                    .ok_or_else(|| format!("database restore refused: paid event {event_id} has no served revision"))?;
                if event.source == "couch" {
                    let (effect_count, prior_revision): (i64, Option<i64>) = db
                        .connection()
                        .query_row(
                            "SELECT COUNT(*), MIN(prior_revision)
                               FROM human_decision_effect_events
                              WHERE review_event_id = ?1 AND decision_revision = ?2",
                            rusqlite::params![event_id, decision_revision],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .map_err(|error| format!("restore target paid playback revision is unreadable: {error}"))?;
                    if effect_count != 1 {
                        return Err(format!(
                            "database restore refused: paid corpus event {event_id} has no unique decision effect for playback binding"
                        ));
                    }
                    let effect_prior_revision = prior_revision.ok_or_else(|| {
                        format!("database restore refused: paid corpus event {event_id} has no receipt revision")
                    })?;
                    if effect_prior_revision != receipt_revision {
                        return Err(format!(
                            "database restore refused: paid corpus event {event_id} served revision disagrees with its decision effect"
                        ));
                    }
                }
                let (content_hash, source_start_ms, source_end_ms) = canonical_work_audio_identity(
                    &ledger.canonical_work_id,
                    &ledger.reviewer,
                )
                .ok_or_else(|| {
                    format!(
                        "database restore refused: paid event {event_id} has no canonical content-hash/source-span work identity"
                    )
                })?;
                let retained_identity: Option<(Option<String>, i64, i64, Option<String>)> = db
                    .connection()
                    .query_row(
                        "SELECT audio_content_hash, duration_ms, COALESCE(review_revision, 0), alignment_json
                           FROM speech_segments WHERE id = ?1",
                        [&ledger.segment_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()
                    .map_err(|error| format!("restore target paid segment identity is unreadable: {error}"))?;
                let Some((retained_hash, retained_duration, retained_revision, retained_alignment)) = retained_identity
                else {
                    return Err(format!(
                        "database restore refused: post-v60 paid segment {} is missing; policy-3 evidence forbids reviewed-segment deletion",
                        ledger.segment_id
                    ));
                };
                let retained_span = crate::db::canonical_source_span(retained_alignment.as_deref());
                if retained_hash.as_deref() != Some(content_hash)
                    || retained_span != Some((source_start_ms, source_end_ms))
                    || retained_duration != ledger.duration_ms
                    || retained_revision < decision_revision
                {
                    return Err(format!(
                        "database restore refused: paid review event {event_id} disagrees with its retained BLAKE3/source-span/duration identity"
                    ));
                }
                let sufficient_receipts: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*)
                           FROM playback_receipts receipt
                          WHERE receipt.segment_id = ?1
                            AND receipt.reviewer = ?2 COLLATE NOCASE
                            AND receipt.segment_revision = ?3
                            AND receipt.audio_fingerprint = ?4
                            AND receipt.source_start_ms = ?5
                            AND receipt.source_end_ms = ?6
                            AND receipt.clip_duration_ms = ?7
                            AND (
                                 receipt.policy_version = ?8
                                 OR (
                                      receipt.policy_version = ?9
                                      AND receipt.authority_session_id IS NOT NULL
                                      AND EXISTS (
                                           SELECT 1 FROM playback_authority_consumptions_v4 consumption
                                            WHERE consumption.playback_receipt_id = receipt.authority_session_id
                                              AND consumption.namespace = ?10
                                              AND consumption.operation_id = ?11
                                              AND consumption.reviewer = ?2 COLLATE NOCASE
                                              AND consumption.segment_id = ?1
                                      )
                                      AND (
                                           ?10 <> 'canonical'
                                           OR EXISTS (
                                                SELECT 1 FROM human_decision_effect_events effect
                                                 WHERE effect.review_event_id = ?12
                                                   AND effect.playback_authority_session_id = receipt.authority_session_id
                                           )
                                      )
                                 )
                            )
                            AND receipt.started_at_ms >= 0
                            AND receipt.played_ms >= 0
                            AND receipt.coverage_ratio >= ?13",
                        rusqlite::params![
                            ledger.segment_id,
                            ledger.reviewer,
                            receipt_revision,
                            content_hash,
                            source_start_ms,
                            source_end_ms,
                            ledger.duration_ms,
                            crate::db::PLAYBACK_POLICY_VERSION,
                            crate::db::DESKTOP_PLAYBACK_POLICY_VERSION,
                            if event.source == "couch" { "canonical" } else { "spot_check" },
                            event.operation_id.as_deref().unwrap_or_default(),
                            event_id,
                            crate::db::MIN_PLAYBACK_COVERAGE,
                        ],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("restore target paid playback evidence is unreadable: {error}"))?;
                if sufficient_receipts == 0 {
                    return Err(format!(
                        "database restore refused: paid review event {event_id} has no exact consumed policy-3/4 playback authority"
                    ));
                }
            }
            let expected_bps = review_action_basis_points(expected_action).ok_or_else(|| {
                format!("database restore refused: ledger entry {} has an unsupported action", ledger.entry_id)
            })?;
            let entitlement = if expected_bps == 0 {
                if ledger.duration_ms <= 0 {
                    return Err(format!(
                        "database restore refused: ledger entry {} has non-positive duration",
                        ledger.entry_id
                    ));
                }
                0
            } else {
                exact_review_entitlement(ledger.duration_ms, expected_bps)?
            };
            let expected_delta = if expected_action == "skip" {
                0
            } else {
                entitlement.checked_sub(prior).ok_or_else(|| "review compensation delta overflow".to_string())?
            };
            let corrected_target = match expected_action {
                "edit" => ledger.duration_ms,
                "skip" => prior_corrected,
                "accept" | "reject" => 0,
                _ => return Err(format!("database restore refused: unsupported ledger action {expected_action}")),
            };
            let expected_corrected_delta = corrected_target
                .checked_sub(prior_corrected)
                .ok_or_else(|| "review corrected-entitlement delta overflow".to_string())?;
            if ledger.rate_basis_points != expected_bps
                || ledger.entitlement_micro_iqd != entitlement
                || ledger.delta_micro_iqd != expected_delta
                || ledger.corrected_entitlement_ms != corrected_target
                || ledger.delta_corrected_ms != expected_corrected_delta
            {
                return Err(format!(
                    "database restore refused: compensation rate/delta/corrected math is invalid at {}",
                    ledger.entry_id
                ));
            }
        } else if ledger.compensation_action == "undo" {
            if ledger.effective_decision != "undo"
                || ledger.source != "couch_undo"
                || ledger.rate_basis_points != 0
                || ledger.entitlement_micro_iqd != 0
            {
                return Err(format!(
                    "database restore refused: undo ledger entry {} has invalid fixed semantics",
                    ledger.entry_id
                ));
            }
            let reversed_id = ledger.reverses_entry_id.as_deref().ok_or_else(|| {
                format!("database restore refused: undo {} does not name an earlier entry", ledger.entry_id)
            })?;
            let reversed = entries.get(reversed_id).ok_or_else(|| {
                format!("database restore refused: undo {} references a missing or later entry", ledger.entry_id)
            })?;
            let latest_eligible = entries
                .values()
                .filter(|entry| {
                    entry.review_event_id.is_some()
                        && entry.compensation_action != "undo"
                        && entry.canonical_work_id == ledger.canonical_work_id
                        && entry.reviewer.trim().eq_ignore_ascii_case(ledger.reviewer.trim())
                        && !reversed_entries.contains(&entry.entry_id)
                })
                .max_by_key(|entry| entry.id)
                .map(|entry| entry.entry_id.as_str());
            if reversed.compensation_action == "undo"
                || reversed.canonical_work_id != ledger.canonical_work_id
                || reversed.segment_id != ledger.segment_id
                || reversed.reviewer.trim().to_lowercase() != ledger.reviewer.trim().to_lowercase()
                || reversed.duration_ms != ledger.duration_ms
                || reversed.decision_revision != ledger.decision_revision
                || latest_eligible != Some(reversed_id)
                || !reversed_entries.insert(reversed_id.to_string())
            {
                return Err(format!(
                    "database restore refused: undo {} does not exactly bind its earlier decision entry",
                    ledger.entry_id
                ));
            }
            let reversed_event_id = reversed.review_event_id.ok_or_else(|| {
                format!("database restore refused: undo {} does not reverse a production decision", ledger.entry_id)
            })?;
            let reversed_event = events.get(&reversed_event_id).ok_or_else(|| {
                format!("database restore refused: undo {} reverses an unknown event", ledger.entry_id)
            })?;
            let undo_operation = ledger.entry_key.strip_prefix("undo:").unwrap_or_default();
            if reversed.source != "couch"
                || reversed.effective_decision == "skip"
                || !is_canonical_lowercase_uuid(undo_operation)
                || reversed_event.operation_id.as_deref() != Some(undo_operation)
            {
                return Err(format!(
                    "database restore refused: undo {} has invalid operation/event linkage",
                    ledger.entry_id
                ));
            }
            let expected_delta = reversed
                .delta_micro_iqd
                .checked_neg()
                .ok_or_else(|| "review compensation undo overflow".to_string())?;
            let expected_corrected_delta = reversed
                .delta_corrected_ms
                .checked_neg()
                .ok_or_else(|| "review corrected-entitlement undo overflow".to_string())?;
            let expected_corrected_entitlement = prior_corrected
                .checked_add(expected_corrected_delta)
                .ok_or_else(|| "review corrected-entitlement undo balance overflow".to_string())?;
            if ledger.delta_micro_iqd != expected_delta
                || ledger.delta_corrected_ms != expected_corrected_delta
                || ledger.corrected_entitlement_ms != expected_corrected_entitlement
            {
                return Err(format!(
                    "database restore refused: undo compensation math is invalid at {}",
                    ledger.entry_id
                ));
            }
        } else {
            return Err(format!(
                "database restore refused: ledger entry {} has neither event nor undo semantics",
                ledger.entry_id
            ));
        }

        let balance = prior
            .checked_add(ledger.delta_micro_iqd)
            .ok_or_else(|| "review compensation running balance overflow".to_string())?;
        let corrected_balance = prior_corrected
            .checked_add(ledger.delta_corrected_ms)
            .ok_or_else(|| "review corrected-entitlement running balance overflow".to_string())?;
        if balance < 0 || corrected_balance < 0 {
            return Err(format!(
                "database restore refused: compensation entry {} creates a negative running balance",
                ledger.entry_id
            ));
        }
        balances.insert(ledger.canonical_work_id.clone(), balance);
        corrected_balances.insert(ledger.canonical_work_id.clone(), corrected_balance);
        if entries.insert(ledger.entry_id.clone(), ledger.clone()).is_some() {
            return Err("database restore refused: compensation ledger has duplicate entry identity".to_string());
        }
    }
    for event_id in events.keys() {
        if event_entry_counts.get(event_id).copied().unwrap_or(0) != 1 {
            return Err(format!(
                "database restore refused: post-cutoff review event {event_id} does not have exactly one current-policy ledger entry"
            ));
        }
    }

    let mut settlement_statement = db
        .connection()
        .prepare(
            "SELECT settlement_id, reviewer, from_ledger_id_exclusive,
                    through_ledger_id_inclusive, allocated_micro_iqd, payout_reference
               FROM review_compensation_settlements
              WHERE policy_version = ?1
              ORDER BY reviewer COLLATE NOCASE, through_ledger_id_inclusive",
        )
        .map_err(|error| format!("restore target compensation settlements are unreadable: {error}"))?;
    let settlements = settlement_statement
        .query_map([crate::db::REVIEW_PAY_POLICY_VERSION], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|error| format!("restore target compensation settlements are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("restore target compensation settlements are unreadable: {error}"))?;
    drop(settlement_statement);
    let maximum_ledger_id = ledger_rows.last().map(|row| row.id).unwrap_or(0);
    let mut boundaries = std::collections::HashMap::<String, i64>::new();
    let mut payout_references = std::collections::HashSet::<String>::new();
    for (settlement_id, reviewer, from, through, amount, payout_reference) in settlements {
        let reviewer_key = reviewer.trim().to_lowercase();
        let expected_from = boundaries.get(&reviewer_key).copied().unwrap_or(0);
        if !is_canonical_lowercase_uuid(&settlement_id)
            || !valid_compensation_reviewer(&reviewer)
            || from != expected_from
            || through <= from
            || through > maximum_ledger_id
        {
            return Err(format!(
                "database restore refused: settlement {settlement_id} has a non-contiguous or invalid ledger range"
            ));
        }
        let mut exact_amount = 0i64;
        let mut matching_rows = 0usize;
        for ledger in &ledger_rows {
            if ledger.reviewer.trim().to_lowercase() == reviewer_key && ledger.id > from && ledger.id <= through {
                exact_amount = exact_amount
                    .checked_add(ledger.delta_micro_iqd)
                    .ok_or_else(|| "review settlement amount overflow".to_string())?;
                matching_rows += 1;
            }
        }
        if matching_rows == 0 || exact_amount != amount {
            return Err(format!(
                "database restore refused: settlement {settlement_id} amount differs from its immutable ledger range"
            ));
        }
        let reference = payout_reference.trim().to_string();
        if reference.is_empty() || reference != payout_reference || !payout_references.insert(reference) {
            return Err(format!(
                "database restore refused: settlement {settlement_id} has an empty or duplicate payout reference"
            ));
        }
        boundaries.insert(reviewer_key, through);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_review_compensation_semantics;
    use crate::db::Database;

    fn canonical_operation(index: u64) -> String {
        format!("00000000-0000-4000-8000-{index:012x}")
    }

    /// A segment carrying the canonical pay evidence: content hash, fingerprint, source span.
    fn paid_segment(db: &Database, id: &str) {
        db.insert_segment(&crate::db::SpeechSegment {
            id: id.to_string(),
            audio_path: format!("{id}.wav"),
            raw_transcript: "machine draft".to_string(),
            duration_ms: 1_000,
            confidence: Some(0.99),
            ..crate::db::SpeechSegment::default()
        })
        .unwrap();
        db.connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?2,
                        audio_fingerprint = ?3,
                        alignment_json = '{\"source_start_ms\":0,\"source_end_ms\":1000}',
                        duration_ms = 1000
                  WHERE id = ?1",
                rusqlite::params![id, "a".repeat(64), 424_242_i64],
            )
            .unwrap();
    }

    fn seeded_db(id: &str) -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        paid_segment(&db, id);
        db
    }

    /// Mint the policy-3 listening evidence the paid decision below must be able to prove.
    fn listened(db: &Database, id: &str) {
        db.record_playback_receipt(&crate::db::PlaybackReceipt {
            segment_id: id.to_string(),
            segment_revision: 0,
            audio_content_hash: "a".repeat(64),
            reviewer: Some("Reviewer".to_string()),
            session_id: None,
            started_at_ms: 1,
            played_ms: 1_000,
            clip_duration_ms: 1_000,
            source_start_ms: None,
            source_end_ms: None,
        })
        .unwrap();
    }

    /// A real paid phone edit through the production API.
    fn decided(db: &Database, id: &str, index: u64) {
        let revision = db.segment_review_revision(id).unwrap().unwrap();
        db.record_phone_human_decision_by_at_revision_with_operation(
            id,
            "edit",
            Some("corrected text"),
            "Reviewer",
            revision,
            &canonical_operation(index),
            &crate::db::review_operation_payload_hash(id, "edit", "corrected text", "Reviewer"),
        )
        .unwrap()
        .unwrap();
    }

    fn paid_fixture(id: &str, index: u64) -> Database {
        let db = seeded_db(id);
        listened(&db, id);
        decided(&db, id, index);
        validate_review_compensation_semantics(&db)
            .expect("a genuine paid decision with listening evidence must validate first");
        db
    }

    /// Restored files can carry rows written with the guards disabled — drop them first.
    fn unlock(db: &Database, table: &str) {
        let names = db
            .connection()
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name=?1")
            .unwrap()
            .query_map([table], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for name in names {
            db.connection().execute(&format!("DROP TRIGGER \"{name}\""), []).unwrap();
        }
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON; PRAGMA foreign_keys = OFF;").unwrap();
    }

    #[test]
    fn a_fresh_database_and_a_genuine_paid_decision_both_validate() {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        validate_review_compensation_semantics(&db).expect("a pristine database is a valid pay target");
        paid_fixture("paid-clip", 100);
    }

    #[test]
    fn the_compensation_policy_row_must_be_the_exact_binary_constants() {
        let deleted = paid_fixture("policy-clip", 110);
        unlock(&deleted, "review_compensation_policies");
        deleted.connection().execute("DELETE FROM review_compensation_policies", []).unwrap();
        let error = validate_review_compensation_semantics(&deleted).unwrap_err();
        assert!(error.contains("must contain only the exact"), "{error}");

        let drifted = paid_fixture("policy-clip", 111);
        unlock(&drifted, "review_compensation_policies");
        drifted.connection().execute("UPDATE review_compensation_policies SET edit_basis_points = 9999", []).unwrap();
        let error = validate_review_compensation_semantics(&drifted).unwrap_err();
        assert!(error.contains("constants differ from this binary"), "{error}");

        let out_of_range = paid_fixture("policy-clip", 112);
        unlock(&out_of_range, "review_compensation_policies");
        out_of_range
            .connection()
            .execute("UPDATE review_compensation_policies SET effective_after_event_id = 99", [])
            .unwrap();
        let error = validate_review_compensation_semantics(&out_of_range).unwrap_err();
        assert!(error.contains("outside review history"), "{error}");
    }

    #[test]
    fn post_cutoff_event_evidence_refusals() {
        let cases: [(&str, &str, &str); 4] = [
            (
                "pay action contradicts the request classification",
                "UPDATE review_events SET compensation_action='accept'",
                "invalid action/pay semantics",
            ),
            ("no durable duration", "UPDATE review_events SET duration_ms=NULL", "no valid durable duration"),
            (
                "non-canonical payload hash",
                "UPDATE review_events SET operation_payload_hash='xyz'",
                "lacks a canonical payload hash",
            ),
            (
                "unauthorized event source",
                "UPDATE review_events SET source='desktop'",
                "not a valid production Couch action",
            ),
        ];
        for (label, sabotage, expected) in cases {
            let db = paid_fixture("event-clip", 120);
            unlock(&db, "review_events");
            assert_eq!(db.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = validate_review_compensation_semantics(&db).unwrap_err();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }

        // Every paid event must carry a canonical lowercase operation UUID. (A same-value duplicate
        // cannot be forged in place: the schema's UNIQUE index on review_events.operation_id is
        // undroppable table authority, so the non-canonical arm is the reachable half.)
        let db = paid_fixture("event-clip", 121);
        unlock(&db, "review_events");
        db.connection().execute("UPDATE review_events SET operation_id='not-a-uuid'", []).unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("unique canonical lowercase UUID"), "{error}");
    }

    #[test]
    fn ledger_identity_and_arithmetic_refusals() {
        let cases: [(&str, &str, &str); 5] = [
            (
                "delta drifts from the re-derived entitlement",
                "UPDATE review_compensation_ledger SET delta_micro_iqd=delta_micro_iqd+1",
                "math is invalid",
            ),
            (
                "rate drifts from the action's basis points",
                "UPDATE review_compensation_ledger SET rate_basis_points=5",
                "math is invalid",
            ),
            (
                "work identity kind is not the canonical audio identity",
                "UPDATE review_compensation_ledger SET canonical_identity_kind='legacy'",
                "disagrees with canonical segment/work identity",
            ),
            (
                "blank reviewer identity",
                "UPDATE review_compensation_ledger SET reviewer=''",
                "invalid or duplicate durable identity",
            ),
            (
                "entry key repoints to another event",
                "UPDATE review_compensation_ledger SET entry_key='review-event:999'",
                "disagrees with review event",
            ),
        ];
        for (label, sabotage, expected) in cases {
            let db = paid_fixture("ledger-clip", 130);
            unlock(&db, "review_compensation_ledger");
            assert_eq!(db.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = validate_review_compensation_semantics(&db).unwrap_err();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }

        // Deleting the listening evidence behind a paid event is refused: pay without proof of
        // playback is exactly the forgery the policy-3/4 binding exists to stop.
        let db = paid_fixture("ledger-clip", 131);
        unlock(&db, "playback_receipts");
        assert!(db.connection().execute("DELETE FROM playback_receipts", []).unwrap() >= 1);
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("no exact consumed policy-3/4 playback authority"), "{error}");
    }

    #[test]
    fn settlements_must_cover_exact_contiguous_ledger_ranges() {
        let db = paid_fixture("settle-clip", 140);
        let (maximum_id, amount): (i64, i64) = db
            .connection()
            .query_row(
                "SELECT COALESCE(MAX(id),0), COALESCE(SUM(delta_micro_iqd),0) FROM review_compensation_ledger",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(maximum_id >= 1, "the paid decision must have minted a ledger entry");
        unlock(&db, "review_compensation_settlements");
        let insert = |settlement_id: &str, from: i64, through: i64, amount: i64, reference: &str| {
            db.connection()
                .execute(
                    "INSERT INTO review_compensation_settlements
                        (settlement_id, policy_version, reviewer, from_ledger_id_exclusive,
                         through_ledger_id_inclusive, allocated_micro_iqd, payout_reference)
                     VALUES (?1, ?2, 'Reviewer', ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        settlement_id,
                        crate::db::REVIEW_PAY_POLICY_VERSION,
                        from,
                        through,
                        amount,
                        reference
                    ],
                )
                .unwrap();
        };

        // The exact contiguous range with the exact immutable amount validates.
        insert("10000000-0000-4000-8000-000000000001", 0, maximum_id, amount, "payout-1");
        validate_review_compensation_semantics(&db).unwrap();

        // A range reaching beyond retained ledger history is refused.
        insert("10000000-0000-4000-8000-000000000002", maximum_id, maximum_id + 5, 0, "payout-2");
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("non-contiguous or invalid ledger range"), "{error}");
        db.connection()
            .execute("DELETE FROM review_compensation_settlements WHERE payout_reference='payout-2'", [])
            .unwrap();

        // An amount differing from the immutable range is refused.
        let mismatched = paid_fixture("settle-clip-2", 141);
        unlock(&mismatched, "review_compensation_settlements");
        mismatched
            .connection()
            .execute(
                "INSERT INTO review_compensation_settlements
                    (settlement_id, policy_version, reviewer, from_ledger_id_exclusive,
                     through_ledger_id_inclusive, allocated_micro_iqd, payout_reference)
                 VALUES ('10000000-0000-4000-8000-000000000003', ?1, 'Reviewer', 0, 1, 42, 'payout-3')",
                [crate::db::REVIEW_PAY_POLICY_VERSION],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&mismatched).unwrap_err();
        assert!(error.contains("amount differs from its immutable ledger range"), "{error}");

        // A blank payout reference can never anchor real money movement.
        let blank = paid_fixture("settle-clip-3", 142);
        unlock(&blank, "review_compensation_settlements");
        let (blank_max, blank_amount): (i64, i64) = blank
            .connection()
            .query_row(
                "SELECT COALESCE(MAX(id),0), COALESCE(SUM(delta_micro_iqd),0) FROM review_compensation_ledger",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        blank
            .connection()
            .execute(
                "INSERT INTO review_compensation_settlements
                    (settlement_id, policy_version, reviewer, from_ledger_id_exclusive,
                     through_ledger_id_inclusive, allocated_micro_iqd, payout_reference)
                 VALUES ('10000000-0000-4000-8000-000000000004', ?1, 'Reviewer', 0, ?2, ?3, '   ')",
                rusqlite::params![crate::db::REVIEW_PAY_POLICY_VERSION, blank_max, blank_amount],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&blank).unwrap_err();
        assert!(error.contains("empty or duplicate payout reference"), "{error}");
    }

    // ── Wave-4 branch coverage. File-backed databases (tempfile, never :memory:) and direct arms
    // on the pure identity/arithmetic helpers this module owns.

    fn file_paid_db(dir: &tempfile::TempDir, id: &str) -> Database {
        let path = dir.path().join(format!("{id}.db"));
        let db = Database::open(path.to_string_lossy().as_ref()).unwrap();
        db.initialize().unwrap();
        paid_segment(&db, id);
        db
    }

    #[test]
    fn entitlement_arithmetic_and_action_rates_are_exact() {
        // 1000 ms at the full edit rate is exactly 5,000,000 micro-IQD under the canon constants
        // (18,000,000,000 micro-IQD/h × 10,000 bps): duration × bps / 2.
        assert_eq!(super::exact_review_entitlement(1_000, crate::db::REVIEW_PAY_EDIT_BPS).unwrap(), 5_000_000);
        assert_eq!(super::exact_review_entitlement(1_000, crate::db::REVIEW_PAY_ACCEPT_BPS).unwrap(), 500_000);

        let invalid_duration = super::exact_review_entitlement(0, 100).unwrap_err();
        assert!(invalid_duration.contains("invalid duration or basis points"), "{invalid_duration}");
        assert!(super::exact_review_entitlement(-5, 100).is_err());
        assert!(super::exact_review_entitlement(1_000, 10_001).is_err());
        assert!(super::exact_review_entitlement(1_000, -1).is_err());
        // 1 ms at 1 bps is 18e9/36e10 of a micro-IQD — not an integer amount, never roundable.
        let inexact = super::exact_review_entitlement(1, 1).unwrap_err();
        assert!(inexact.contains("not an exact micro-IQD amount"), "{inexact}");

        assert_eq!(super::review_action_basis_points("edit"), Some(crate::db::REVIEW_PAY_EDIT_BPS));
        assert_eq!(super::review_action_basis_points("accept"), Some(crate::db::REVIEW_PAY_ACCEPT_BPS));
        assert_eq!(super::review_action_basis_points("reject"), Some(crate::db::REVIEW_PAY_REJECT_BPS));
        assert_eq!(super::review_action_basis_points("skip"), Some(crate::db::REVIEW_PAY_SKIP_BPS));
        assert_eq!(super::review_action_basis_points("undo"), None);
    }

    #[test]
    fn identity_helpers_accept_only_canonical_shapes() {
        assert!(super::is_canonical_lowercase_uuid("00000000-0000-4000-8000-000000000001"));
        assert!(!super::is_canonical_lowercase_uuid("00000000-0000-4000-8000-0000000000ZZ"));
        // Parseable but not canonical lowercase-hyphenated.
        assert!(!super::is_canonical_lowercase_uuid("00000000-0000-4000-8000-0000000000AB"));
        assert!(super::is_canonical_lowercase_64_hex(&"a".repeat(64)));
        assert!(!super::is_canonical_lowercase_64_hex(&"A".repeat(64)));
        assert!(!super::is_canonical_lowercase_64_hex("abc"));

        assert!(super::valid_compensation_reviewer("Reviewer"));
        assert!(!super::valid_compensation_reviewer(""));
        assert!(!super::valid_compensation_reviewer(" Reviewer "));
        assert!(!super::valid_compensation_reviewer(&"x".repeat(41)));
        assert!(!super::valid_compensation_reviewer("Re\u{7}viewer"));

        let work = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0:1000", "a".repeat(64));
        assert!(super::canonical_work_id_has_writer_shape(&work, "Reviewer", 1_000));
        assert!(!super::canonical_work_id_has_writer_shape(&work, "Somebody", 1_000), "wrong reviewer namespace");
        assert!(!super::canonical_work_id_has_writer_shape(&work, "Reviewer", 900_000), "span must match duration");
        assert!(!super::canonical_work_id_has_writer_shape("reviewer-work-v1:8:reviewer:legacy:x", "Reviewer", 1_000));
        let extra_part = format!("{work}:9");
        assert!(!super::canonical_work_id_has_writer_shape(&extra_part, "Reviewer", 1_000));
        let bad_hash = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0:1000", "Z".repeat(64));
        assert!(!super::canonical_work_id_has_writer_shape(&bad_hash, "Reviewer", 1_000));
        let bad_number = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:zero:1000", "a".repeat(64));
        assert!(!super::canonical_work_id_has_writer_shape(&bad_number, "Reviewer", 1_000));

        let expected_hash = "a".repeat(64);
        assert_eq!(super::canonical_work_audio_identity(&work, "Reviewer"), Some((expected_hash.as_str(), 0, 1_000)));
        assert!(super::canonical_work_audio_identity(&work, "Somebody").is_none());
        assert!(super::canonical_work_audio_identity(&extra_part, "Reviewer").is_none());
        let negative_span = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:-1:1000", "a".repeat(64));
        assert!(super::canonical_work_audio_identity(&negative_span, "Reviewer").is_none());
        let empty_span = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:1000:1000", "a".repeat(64));
        assert!(super::canonical_work_audio_identity(&empty_span, "Reviewer").is_none());
    }

    #[test]
    fn canonical_compensation_work_rederives_only_exact_segment_identity() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "work-clip");
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON;").unwrap();
        // Every UPDATE that leaves review_revision untouched is auto-bumped by the
        // speech_segments_review_revision trigger, so each mutation below PINS the revision to a
        // fresh explicit value (which the trigger then leaves alone) and asks about exactly it.
        db.connection().execute("UPDATE speech_segments SET review_revision=7 WHERE id='work-clip'", []).unwrap();

        let (work_id, duration) =
            super::canonical_compensation_work(&db, "work-clip", "Reviewer", 7).unwrap().expect("exact current work");
        assert_eq!(work_id, format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0:1000", "a".repeat(64)));
        assert_eq!(duration, 1_000);

        assert_eq!(super::canonical_compensation_work(&db, "no-such-clip", "Reviewer", 0).unwrap(), None);
        let invalid_reviewer = super::canonical_compensation_work(&db, "work-clip", " ", 7).unwrap_err();
        assert!(invalid_reviewer.contains("invalid reviewer identity"), "{invalid_reviewer}");
        let regressed = super::canonical_compensation_work(&db, "work-clip", "Reviewer", 12).unwrap_err();
        assert!(regressed.contains("regresses its decision revision"), "{regressed}");

        // A decision at an older revision than the segment is simply not current — no identity claim.
        assert_eq!(super::canonical_compensation_work(&db, "work-clip", "Reviewer", 5).unwrap(), None);

        let mutate = |sql: &str| {
            db.connection().execute(sql, []).unwrap();
        };
        mutate("UPDATE speech_segments SET review_revision=8, duration_ms=0 WHERE id='work-clip'");
        let bad_duration = super::canonical_compensation_work(&db, "work-clip", "Reviewer", 8).unwrap_err();
        assert!(bad_duration.contains("invalid duration"), "{bad_duration}");

        mutate("UPDATE speech_segments SET review_revision=9, duration_ms=1000, audio_content_hash='  ' WHERE id='work-clip'");
        let fallback = super::canonical_compensation_work(&db, "work-clip", "Reviewer", 9).unwrap_err();
        assert!(fallback.contains("fallback audio identity"), "{fallback}");

        let restore_hash = format!(
            "UPDATE speech_segments SET review_revision=10, audio_content_hash='{}', alignment_json=NULL WHERE id='work-clip'",
            "a".repeat(64)
        );
        mutate(&restore_hash);
        let missing_span = super::canonical_compensation_work(&db, "work-clip", "Reviewer", 10).unwrap_err();
        assert!(missing_span.contains("no source-span identity"), "{missing_span}");

        mutate("UPDATE speech_segments SET review_revision=11, alignment_json='not json' WHERE id='work-clip'");
        let invalid_span = super::canonical_compensation_work(&db, "work-clip", "Reviewer", 11).unwrap_err();
        assert!(invalid_span.contains("invalid source-span identity"), "{invalid_span}");

        mutate("UPDATE speech_segments SET review_revision=12, alignment_json='{}' WHERE id='work-clip'");
        let incomplete = super::canonical_compensation_work(&db, "work-clip", "Reviewer", 12).unwrap_err();
        assert!(incomplete.contains("incomplete source-span identity"), "{incomplete}");

        mutate(
            "UPDATE speech_segments
                SET review_revision=13, alignment_json='{\"source_start_ms\":0,\"source_end_ms\":400}'
              WHERE id='work-clip'",
        );
        let mismatched = super::canonical_compensation_work(&db, "work-clip", "Reviewer", 13).unwrap_err();
        assert!(mismatched.contains("disagrees with decoded duration"), "{mismatched}");
    }

    /// A paid phone edit undone through the production API, with the couch undo shape: the undo is
    /// addressed by the DECISION's own operation id (couch::api_undo replays `entry.operation_id`),
    /// which is what binds `entry_key = 'undo:<op>'` to the reversed event.
    fn undone_paid_fixture(dir: &tempfile::TempDir, id: &str, index: u64) -> Database {
        let db = file_paid_db(dir, id);
        listened(&db, id);
        decided(&db, id, index);
        let effect_id: i64 = db
            .connection()
            .query_row("SELECT MAX(id) FROM human_decision_effect_events", [], |row| row.get(0))
            .unwrap();
        assert!(matches!(
            db.undo_human_decision(effect_id, Some("Reviewer"), &canonical_operation(index)).unwrap(),
            crate::db::HumanDecisionUndoOutcome::Applied { .. }
        ));
        validate_review_compensation_semantics(&db)
            .expect("a phone decision undone through the production API must validate first");
        db
    }

    #[test]
    fn undo_ledger_rows_must_be_exact_operation_bound_inverses() {
        let dir = tempfile::TempDir::new().unwrap();

        let db = undone_paid_fixture(&dir, "undo-fixed", 200);
        unlock(&db, "review_compensation_ledger");
        db.connection()
            .execute("UPDATE review_compensation_ledger SET rate_basis_points=1 WHERE compensation_action='undo'", [])
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("invalid fixed semantics"), "{error}");

        let db = undone_paid_fixture(&dir, "undo-unnamed", 201);
        unlock(&db, "review_compensation_ledger");
        db.connection()
            .execute(
                "UPDATE review_compensation_ledger SET reverses_entry_id=NULL WHERE compensation_action='undo'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("does not name an earlier entry"), "{error}");

        let db = undone_paid_fixture(&dir, "undo-missing", 202);
        unlock(&db, "review_compensation_ledger");
        db.connection()
            .execute(
                "UPDATE review_compensation_ledger
                    SET reverses_entry_id='10101010-1010-4010-8010-101010101010'
                  WHERE compensation_action='undo'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("references a missing or later entry"), "{error}");

        // A revision drift between the undo and the entry it reverses breaks the exact binding.
        let db = undone_paid_fixture(&dir, "undo-binding", 203);
        unlock(&db, "review_compensation_ledger");
        db.connection()
            .execute(
                "UPDATE review_compensation_ledger SET decision_revision=decision_revision+1
                  WHERE compensation_action='undo'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("does not exactly bind its earlier decision entry"), "{error}");

        // A second inverse for the same reversed entry can only double the clawback. The schema's
        // partial unique index forbids writing this state here, so the restored-file threat model
        // includes losing that index too.
        let db = undone_paid_fixture(&dir, "undo-double", 204);
        unlock(&db, "review_compensation_ledger");
        db.connection().execute("DROP INDEX idx_review_compensation_one_reversal_per_entry", []).unwrap();
        db.connection()
            .execute(
                "INSERT INTO review_compensation_ledger
                    (entry_id, entry_key, policy_version, canonical_work_id, canonical_identity_kind,
                     reviewer, segment_id, source, compensation_action, effective_decision,
                     decision_revision, duration_ms, rate_basis_points, entitlement_micro_iqd,
                     delta_micro_iqd, corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id)
                 SELECT '20202020-2020-4020-8020-202020202020',
                        'undo:20202020-2020-4020-8020-202020202020', policy_version,
                        canonical_work_id, canonical_identity_kind, reviewer, segment_id, source,
                        compensation_action, effective_decision, decision_revision, duration_ms,
                        rate_basis_points, entitlement_micro_iqd, delta_micro_iqd,
                        corrected_entitlement_ms, delta_corrected_ms, reverses_entry_id
                   FROM review_compensation_ledger WHERE compensation_action='undo'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("does not exactly bind its earlier decision entry"), "{error}");

        let db = undone_paid_fixture(&dir, "undo-math", 205);
        unlock(&db, "review_compensation_ledger");
        db.connection()
            .execute(
                "UPDATE review_compensation_ledger SET delta_micro_iqd=delta_micro_iqd+1
                  WHERE compensation_action='undo'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("undo compensation math is invalid"), "{error}");

        // An unlinked row that is not an undo has no semantics at all.
        let db = undone_paid_fixture(&dir, "undo-neither", 206);
        unlock(&db, "review_compensation_ledger");
        db.connection()
            .execute(
                "UPDATE review_compensation_ledger SET compensation_action='edit'
                  WHERE compensation_action='undo'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("has neither event nor undo semantics"), "{error}");
    }

    #[test]
    fn a_spot_check_event_without_its_immutable_result_is_refused() {
        // Hidden-QC pay evidence: a couch_spot_check event must carry exactly one spot_checks row.
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "spot-clip");
        listened(&db, "spot-clip");
        decided(&db, "spot-clip", 210);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "review_events");
        db.connection().execute("UPDATE review_events SET source='couch_spot_check'", []).unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("lacks its exact immutable spot-check result"), "{error}");
    }

    #[test]
    fn event_reviewer_request_and_served_evidence_are_strictly_validated() {
        let cases: [(&str, &str, &str); 3] = [
            (
                "untrimmed reviewer identity",
                "UPDATE review_events SET reviewer=' Reviewer '",
                "not a valid production Couch action",
            ),
            (
                "unknown requested action",
                "UPDATE review_events SET requested_action='mystery'",
                "invalid action/pay semantics",
            ),
            (
                "blank served transcript",
                "UPDATE review_events SET served_transcript=''",
                "invalid served/request evidence",
            ),
        ];
        for (label, sabotage, expected) in cases {
            let dir = tempfile::TempDir::new().unwrap();
            let db = file_paid_db(&dir, "evidence-clip");
            listened(&db, "evidence-clip");
            decided(&db, "evidence-clip", 220);
            validate_review_compensation_semantics(&db).unwrap();
            unlock(&db, "review_events");
            assert_eq!(db.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = validate_review_compensation_semantics(&db).unwrap_err();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }
    }

    #[test]
    fn ledger_rows_must_stay_bound_to_retained_post_cutoff_events() {
        // Repointed outside the retained post-cutoff event range.
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "orphan-ledger");
        listened(&db, "orphan-ledger");
        decided(&db, "orphan-ledger", 230);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "review_compensation_ledger");
        db.connection().execute("UPDATE review_compensation_ledger SET review_event_id=999", []).unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("points outside the post-cutoff event range"), "{error}");

        // A decision entry without its revision has lost its identity.
        let db = file_paid_db(&dir, "revisionless-ledger");
        listened(&db, "revisionless-ledger");
        decided(&db, "revisionless-ledger", 231);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "review_compensation_ledger");
        db.connection().execute("UPDATE review_compensation_ledger SET decision_revision=NULL", []).unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("has no decision revision"), "{error}");

        // A paid event whose ledger entry vanished is unpaid work — the exact loss restore must refuse.
        let db = file_paid_db(&dir, "unpaid-event");
        listened(&db, "unpaid-event");
        decided(&db, "unpaid-event", 232);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "review_compensation_ledger");
        assert_eq!(db.connection().execute("DELETE FROM review_compensation_ledger", []).unwrap(), 1);
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("does not have exactly one current-policy ledger entry"), "{error}");
    }

    #[test]
    fn settlement_chains_must_be_contiguous_with_unique_references() {
        // Two paid decisions on distinct audio identities, settled in two contiguous ranges through
        // the production settlement writer; then one field is broken per case.
        let build = || {
            let dir = tempfile::TempDir::new().unwrap();
            let db = file_paid_db(&dir, "settle-a");
            paid_segment(&db, "settle-b");
            db.connection()
                .execute("UPDATE speech_segments SET audio_content_hash=?1 WHERE id='settle-b'", [&"b".repeat(64)])
                .unwrap();
            listened(&db, "settle-a");
            db.record_playback_receipt(&crate::db::PlaybackReceipt {
                segment_id: "settle-b".to_string(),
                segment_revision: 0,
                audio_content_hash: "b".repeat(64),
                reviewer: Some("Reviewer".to_string()),
                session_id: None,
                started_at_ms: 1,
                played_ms: 1_000,
                clip_duration_ms: 1_000,
                source_start_ms: None,
                source_end_ms: None,
            })
            .unwrap();
            decided(&db, "settle-a", 240);
            decided(&db, "settle-b", 241);
            let (first, second): (i64, i64) = db
                .connection()
                .query_row("SELECT MIN(id), MAX(id) FROM review_compensation_ledger", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap();
            assert!(first < second, "two paid decisions must mint two ledger entries");
            db.record_review_compensation_settlement("Reviewer", first, "payout-first").unwrap();
            db.record_review_compensation_settlement("Reviewer", second, "payout-second").unwrap();
            validate_review_compensation_semantics(&db)
                .expect("two contiguous production settlements must validate first");
            unlock(&db, "review_compensation_settlements");
            (dir, db)
        };

        let (_dir, db) = build();
        db.connection()
            .execute(
                "UPDATE review_compensation_settlements SET from_ledger_id_exclusive=0
                  WHERE payout_reference='payout-second'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("non-contiguous or invalid ledger range"), "{error}");

        // The table-level UNIQUE on payout_reference is undroppable authority, so the reachable
        // forgery is an untrimmed reference — refused by the same durable-reference guard.
        let (_dir, db) = build();
        db.connection()
            .execute(
                "UPDATE review_compensation_settlements SET payout_reference=' payout-second '
                  WHERE payout_reference='payout-second'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("empty or duplicate payout reference"), "{error}");

        let (_dir, db) = build();
        db.connection()
            .execute(
                "UPDATE review_compensation_settlements SET settlement_id='NOT-A-UUID'
                  WHERE payout_reference='payout-first'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("non-contiguous or invalid ledger range"), "{error}");

        // A settlement claiming a reviewer with no ledger rows in range settles nothing.
        let (_dir, db) = build();
        db.connection()
            .execute(
                "UPDATE review_compensation_settlements SET reviewer='Nobody'
                  WHERE payout_reference='payout-first'",
                [],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("amount differs from its immutable ledger range"), "{error}");
    }
}
