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

    /// The paid segment must still be the exact audio that was paid for, at or after the paid revision.
    fn require_retained_paid_audio(
        db: &crate::db::Database,
        ledger: &Ledger,
        subject: &str,
        content_hash: &str,
        source_start_ms: i64,
        source_end_ms: i64,
        decision_revision: i64,
    ) -> Result<(), String> {
        use rusqlite::OptionalExtension;
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
        let Some((retained_hash, retained_duration, retained_revision, retained_alignment)) = retained_identity else {
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
                "database restore refused: {subject} disagrees with its retained BLAKE3/source-span/duration identity"
            ));
        }
        Ok(())
    }

    /// Rate, entitlement, delta and corrected-audio math of one credit against the running balances.
    fn validate_credit_arithmetic(
        ledger: &Ledger,
        expected_action: &str,
        prior: i64,
        prior_corrected: i64,
    ) -> Result<(), String> {
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
        Ok(())
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
    let mut pool_entry_counts = std::collections::HashMap::<i64, usize>::new();
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
                require_retained_paid_audio(
                    db,
                    ledger,
                    &format!("paid review event {event_id}"),
                    content_hash,
                    source_start_ms,
                    source_end_ms,
                    decision_revision,
                )?;
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
            validate_credit_arithmetic(ledger, expected_action, prior, prior_corrected)?;
        } else if ledger.source == "couch_pool" {
            // Owner canon 2026-09-04: a pool second opinion is paid like a first opinion. It has no
            // review_events row; its provenance is the immutable review_pool_decisions row named by
            // the entry key, and its listening proof is the `independent` policy-4 consumption bound
            // to that row's operation id, session and receipt.
            let pool_decision_id = ledger
                .entry_key
                .strip_prefix("pool-decision:")
                .and_then(|id| id.parse::<i64>().ok())
                .filter(|id| *id > 0)
                .ok_or_else(|| {
                    format!("database restore refused: pool credit {} does not name a pool decision", ledger.entry_id)
                })?;
            *pool_entry_counts.entry(pool_decision_id).or_default() += 1;
            type PoolDecision = (String, String, String, i64, i64, Option<String>, Option<i64>, Option<i64>, String);
            let decision: Option<PoolDecision> = db
                .connection()
                .query_row(
                    "SELECT segment_id, reviewer, action, served_revision, duration_ms,
                            audio_content_hash, source_start_ms, source_end_ms, operation_id
                       FROM review_pool_decisions WHERE id = ?1",
                    [pool_decision_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| format!("restore target pool decision is unreadable: {error}"))?;
            let Some((
                segment_id,
                reviewer,
                action,
                served_revision,
                duration_ms,
                decision_hash,
                decision_start_ms,
                decision_end_ms,
                operation_id,
            )) = decision
            else {
                return Err(format!(
                    "database restore refused: pool credit {} names a missing pool decision {pool_decision_id}",
                    ledger.entry_id
                ));
            };
            if action == "skip"
                || ledger.compensation_action != action
                || ledger.effective_decision != action
                || ledger.segment_id != segment_id
                || !ledger.reviewer.trim().eq_ignore_ascii_case(reviewer.trim())
                || ledger.duration_ms != duration_ms
                || decision_revision != served_revision
                || ledger.reverses_entry_id.is_some()
            {
                return Err(format!(
                    "database restore refused: pool credit {} disagrees with pool decision {pool_decision_id}",
                    ledger.entry_id
                ));
            }
            let audio_identity = canonical_work_audio_identity(&ledger.canonical_work_id, &ledger.reviewer);
            let Some((content_hash, source_start_ms, source_end_ms)) = audio_identity else {
                return Err(format!(
                    "database restore refused: pool credit {} has no canonical content-hash/source-span work identity",
                    ledger.entry_id
                ));
            };
            if decision_hash.as_deref() != Some(content_hash)
                || (decision_start_ms, decision_end_ms) != (Some(source_start_ms), Some(source_end_ms))
            {
                return Err(format!(
                    "database restore refused: pool credit {} was paid for audio other than pool decision {pool_decision_id} judged",
                    ledger.entry_id
                ));
            }
            require_retained_paid_audio(
                db,
                ledger,
                &format!("pool credit {}", ledger.entry_id),
                content_hash,
                source_start_ms,
                source_end_ms,
                decision_revision,
            )?;
            let sufficient_receipts: i64 = db
                .connection()
                .query_row(
                    "SELECT COUNT(*)
                       FROM playback_receipts receipt
                       JOIN desktop_playback_sessions_v4 session
                         ON session.playback_receipt_id = receipt.authority_session_id
                        AND session.surface = 'couch'
                       JOIN playback_authority_consumptions_v4 consumption
                         ON consumption.playback_receipt_id = receipt.authority_session_id
                      WHERE receipt.policy_version = ?1
                        AND receipt.segment_id = ?2
                        AND receipt.reviewer = ?3 COLLATE NOCASE
                        AND receipt.segment_revision = ?4
                        AND receipt.audio_fingerprint = ?5
                        AND receipt.source_start_ms = ?6
                        AND receipt.source_end_ms = ?7
                        AND receipt.clip_duration_ms = ?8
                        AND receipt.started_at_ms >= 0
                        AND receipt.played_ms >= 0
                        AND receipt.coverage_ratio >= ?9
                        AND session.reviewer = ?3 COLLATE NOCASE
                        AND session.segment_id = ?2
                        AND session.segment_revision = ?4
                        AND session.audio_content_hash = ?5
                        AND consumption.namespace = 'independent'
                        AND consumption.operation_id = ?10
                        AND consumption.reviewer = ?3 COLLATE NOCASE
                        AND consumption.segment_id = ?2",
                    rusqlite::params![
                        crate::db::DESKTOP_PLAYBACK_POLICY_VERSION,
                        ledger.segment_id,
                        ledger.reviewer,
                        served_revision,
                        content_hash,
                        source_start_ms,
                        source_end_ms,
                        ledger.duration_ms,
                        crate::db::MIN_PLAYBACK_COVERAGE,
                        operation_id,
                    ],
                    |row| row.get(0),
                )
                .map_err(|error| format!("restore target pool playback evidence is unreadable: {error}"))?;
            if sufficient_receipts == 0 {
                return Err(format!(
                    "database restore refused: pool credit {} has no exact consumed policy-4 playback authority",
                    ledger.entry_id
                ));
            }
            validate_credit_arithmetic(ledger, &action, prior, prior_corrected)?;
        } else if ledger.compensation_action == "undo" {
            if ledger.effective_decision != "undo"
                || !matches!(ledger.source.as_str(), "couch_undo" | "couch_pool_undo")
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
                    (entry.review_event_id.is_some() || entry.source == "couch_pool")
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
            let undo_operation = ledger.entry_key.strip_prefix("undo:").unwrap_or_default();
            if reversed.source == "couch_pool" {
                // A pool undo settles the durable review_pool_reversals row (decision id + its own
                // reversal operation), the way a canonical undo settles its effect reversal.
                let pool_decision_id = reversed
                    .entry_key
                    .strip_prefix("pool-decision:")
                    .and_then(|id| id.parse::<i64>().ok())
                    .unwrap_or_default();
                let reversal_rows: i64 = db
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM review_pool_reversals
                          WHERE decision_id = ?1 AND operation_id = ?2 AND reviewer = ?3 COLLATE NOCASE",
                        rusqlite::params![pool_decision_id, undo_operation, ledger.reviewer],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("restore target pool reversals are unreadable: {error}"))?;
                if ledger.source != "couch_pool_undo"
                    || reversed.effective_decision == "skip"
                    || !is_canonical_lowercase_uuid(undo_operation)
                    || reversal_rows != 1
                {
                    return Err(format!(
                        "database restore refused: undo {} has invalid pool reversal linkage",
                        ledger.entry_id
                    ));
                }
            } else {
                let reversed_event_id = reversed.review_event_id.ok_or_else(|| {
                    format!("database restore refused: undo {} does not reverse a production decision", ledger.entry_id)
                })?;
                let reversed_event = events.get(&reversed_event_id).ok_or_else(|| {
                    format!("database restore refused: undo {} reverses an unknown event", ledger.entry_id)
                })?;
                if ledger.source != "couch_undo"
                    || reversed.source != "couch"
                    || reversed.effective_decision == "skip"
                    || !is_canonical_lowercase_uuid(undo_operation)
                    || reversed_event.operation_id.as_deref() != Some(undo_operation)
                {
                    return Err(format!(
                        "database restore refused: undo {} has invalid operation/event linkage",
                        ledger.entry_id
                    ));
                }
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
    // Every non-skip pool judgement is paid exactly once (owner canon 2026-09-04); the tables exist
    // from schema 62, and older targets simply have no pool work to account for.
    let pool_tables_present: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'review_pool_decisions'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("restore target schema catalog is unreadable: {error}"))?;
    if pool_tables_present == 1 {
        let mut pool_statement = db
            .connection()
            .prepare("SELECT id FROM review_pool_decisions WHERE action <> 'skip' ORDER BY id")
            .map_err(|error| format!("restore target pool decisions are unreadable: {error}"))?;
        let paid_pool_decisions = pool_statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|error| format!("restore target pool decisions are unreadable: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("restore target pool decisions are unreadable: {error}"))?;
        drop(pool_statement);
        for pool_decision_id in paid_pool_decisions {
            if pool_entry_counts.get(&pool_decision_id).copied().unwrap_or(0) != 1 {
                return Err(format!(
                    "database restore refused: pool decision {pool_decision_id} does not have exactly one current-policy credit"
                ));
            }
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

    // ── Wave-5 branch coverage. The unpaid/zero-rate actions, the retained-audio arm, and the
    // remaining identity edges on the pure helpers.

    #[test]
    fn entitlement_and_work_identity_helper_edges() {
        // The last arithmetic arm: an amount that is exact but larger than the money type can hold.
        // (duration × bps / 2 with both at their extremes — a real overflow, not a rounding one.)
        let too_large = super::exact_review_entitlement(i64::MAX, 10_000).unwrap_err();
        assert!(too_large.contains("exceeds the supported integer range"), "{too_large}");

        // A work id whose audio identity is missing its end offset is not a span at all.
        let truncated = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0", "a".repeat(64));
        assert!(!super::canonical_work_id_has_writer_shape(&truncated, "Reviewer", 1_000));
        assert!(super::canonical_work_audio_identity(&truncated, "Reviewer").is_none());

        let unparsable_end = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0:end", "a".repeat(64));
        assert!(super::canonical_work_audio_identity(&unparsable_end, "Reviewer").is_none());
        let uppercase_hash = format!("reviewer-work-v1:8:reviewer:audio-segment-v1:{}:0:1000", "A".repeat(64));
        assert!(super::canonical_work_audio_identity(&uppercase_hash, "Reviewer").is_none());

        // A negative current revision is corrupt even when the decision claims the same value, so
        // the "not current" early return must never swallow it.
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "negative-revision");
        db.connection().execute_batch("PRAGMA ignore_check_constraints = ON;").unwrap();
        db.connection()
            .execute("UPDATE speech_segments SET review_revision=-1 WHERE id='negative-revision'", [])
            .unwrap();
        let negative = super::canonical_compensation_work(&db, "negative-revision", "Reviewer", -1).unwrap_err();
        assert!(negative.contains("regresses its decision revision"), "{negative}");
    }

    #[test]
    fn a_skip_is_recorded_as_unpaid_work_that_still_needs_its_exact_ledger_entry() {
        // Skips are the zero-rate action: they mint a ledger entry that must exist, must be worth
        // nothing, and must not be asked for playback evidence (nobody is paid to skip).
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "skip-clip");
        db.record_review_event_with_operation(
            "skip-clip",
            "Reviewer",
            "skip",
            "couch",
            700,
            &canonical_operation(700),
            &crate::db::review_operation_payload_hash("skip-clip", "skip", "", "Reviewer"),
        )
        .unwrap();
        validate_review_compensation_semantics(&db).expect("a genuine skip is a valid pay target with no receipt");

        let (action, rate, entitlement, delta, corrected): (String, i64, i64, i64, i64) = db
            .connection()
            .query_row(
                "SELECT compensation_action, rate_basis_points, entitlement_micro_iqd,
                        delta_micro_iqd, corrected_entitlement_ms
                   FROM review_compensation_ledger ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            (action.as_str(), rate, entitlement, delta, corrected),
            ("skip", crate::db::REVIEW_PAY_SKIP_BPS, 0, 0, 0),
            "a skip is recorded at the zero rate and moves no money"
        );

        // Paying anything for a skip is refused by the re-derived arithmetic.
        unlock(&db, "review_compensation_ledger");
        assert!(
            db.connection()
                .execute(
                    "UPDATE review_compensation_ledger SET delta_micro_iqd=1 WHERE compensation_action='skip'",
                    [],
                )
                .unwrap()
                >= 1
        );
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("math is invalid"), "{error}");
    }

    #[test]
    fn a_reject_is_paid_at_the_reject_rate_with_no_corrected_time() {
        // The reject action pairs a non-zero rate with zero corrected-entitlement time — a distinct
        // arithmetic arm from the edit fixture every other test here uses.
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "reject-pay");
        listened(&db, "reject-pay");
        let revision = db.segment_review_revision("reject-pay").unwrap().unwrap();
        db.record_phone_human_decision_by_at_revision_with_operation(
            "reject-pay",
            "reject",
            None,
            "Reviewer",
            revision,
            &canonical_operation(710),
            &crate::db::review_operation_payload_hash("reject-pay", "reject", "", "Reviewer"),
        )
        .unwrap()
        .unwrap();
        validate_review_compensation_semantics(&db).expect("a genuine paid reject must validate");

        let (action, rate, corrected, corrected_delta): (String, i64, i64, i64) = db
            .connection()
            .query_row(
                "SELECT compensation_action, rate_basis_points, corrected_entitlement_ms, delta_corrected_ms
                   FROM review_compensation_ledger ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (action.as_str(), rate, corrected, corrected_delta),
            ("reject", crate::db::REVIEW_PAY_REJECT_BPS, 0, 0),
            "a reject pays the reject rate and earns no corrected-transcript time"
        );
    }

    #[test]
    fn paid_work_must_keep_the_retained_audio_it_was_paid_for() {
        // Policy-3 evidence: pay is bound to a specific clip's BLAKE3 + source span + duration. If
        // that clip is gone, or its identity has drifted, the ledger row can no longer name what was
        // reviewed — and an export built from it would attribute paid human work to other audio.
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "vanished-clip");
        listened(&db, "vanished-clip");
        decided(&db, "vanished-clip", 720);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "speech_segments");
        assert_eq!(db.connection().execute("DELETE FROM speech_segments WHERE id='vanished-clip'", []).unwrap(), 1);
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("policy-3 evidence forbids reviewed-segment deletion"), "{error}");

        // A drifted audio hash on a segment that has since moved past the paid revision: the
        // per-revision work-identity check no longer claims it, so the RETAINED identity arm is the
        // one that must still refuse.
        let db = file_paid_db(&dir, "drifted-clip");
        listened(&db, "drifted-clip");
        decided(&db, "drifted-clip", 721);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "speech_segments");
        assert_eq!(
            db.connection()
                .execute(
                    "UPDATE speech_segments SET review_revision = review_revision + 1, audio_content_hash = ?1
                      WHERE id='drifted-clip'",
                    [&"d".repeat(64)],
                )
                .unwrap(),
            1
        );
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("disagrees with its retained BLAKE3/source-span/duration identity"), "{error}");
    }

    #[test]
    fn every_remaining_event_and_ledger_identity_clause_is_load_bearing() {
        // Each case breaks exactly one clause of a disjunction whose other arms are already pinned
        // above, so the message alone would not prove the arm under test ever ran — the corruption
        // is asserted to apply first, and each case gets a fresh fixture.
        let event_cases: [(&str, &str, &str); 3] = [
            (
                "no compensation action at all",
                "UPDATE review_events SET compensation_action=NULL",
                "has no compensation action",
            ),
            ("no served revision", "UPDATE review_events SET served_revision=NULL", "invalid served/request evidence"),
            (
                "untrimmed request text is not canonical",
                "UPDATE review_events SET requested_transcript='  corrected text  '",
                "invalid served/request evidence",
            ),
        ];
        for (label, sabotage, expected) in event_cases {
            // A fresh directory per case: `file_paid_db` names the file after the segment id, so
            // reusing one directory would re-open the previous case's already-corrupted database.
            let dir = tempfile::TempDir::new().unwrap();
            let db = file_paid_db(&dir, "event-clause");
            listened(&db, "event-clause");
            decided(&db, "event-clause", 740);
            validate_review_compensation_semantics(&db).unwrap();
            unlock(&db, "review_events");
            assert_eq!(db.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = validate_review_compensation_semantics(&db).unwrap_err();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }

        let ledger_cases: [(&str, &str, &str); 3] = [
            (
                "durable entry identity is not a canonical UUID",
                "UPDATE review_compensation_ledger SET entry_id='not-a-uuid'",
                "invalid or duplicate durable identity",
            ),
            (
                "work id does not have the writer's shape",
                "UPDATE review_compensation_ledger SET canonical_work_id='forged-work'",
                "disagrees with canonical segment/work identity",
            ),
            (
                "non-positive paid duration",
                "UPDATE review_compensation_ledger SET duration_ms=0",
                "disagrees with canonical segment/work identity",
            ),
        ];
        for (label, sabotage, expected) in ledger_cases {
            let dir = tempfile::TempDir::new().unwrap();
            let db = file_paid_db(&dir, "ledger-clause");
            listened(&db, "ledger-clause");
            decided(&db, "ledger-clause", 741);
            validate_review_compensation_semantics(&db).unwrap();
            unlock(&db, "review_compensation_ledger");
            assert_eq!(db.connection().execute(sabotage, []).unwrap(), 1, "{label}");
            let error = validate_review_compensation_semantics(&db).unwrap_err();
            assert!(error.contains(expected), "{label}: expected '{expected}', got: {error}");
        }

        // A settlement whose range covers no ledger id at all can only be a payout for nothing.
        let dir = tempfile::TempDir::new().unwrap();
        let db = file_paid_db(&dir, "empty-range");
        listened(&db, "empty-range");
        decided(&db, "empty-range", 742);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "review_compensation_settlements");
        db.connection()
            .execute(
                "INSERT INTO review_compensation_settlements
                    (settlement_id, policy_version, reviewer, from_ledger_id_exclusive,
                     through_ledger_id_inclusive, allocated_micro_iqd, payout_reference)
                 VALUES ('30303030-3030-4030-8030-303030303030', ?1, 'Reviewer', 0, 0, 0, 'payout-empty')",
                [crate::db::REVIEW_PAY_POLICY_VERSION],
            )
            .unwrap();
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("non-contiguous or invalid ledger range"), "{error}");
    }

    #[test]
    fn undo_and_decision_rows_must_name_the_operation_and_revision_they_settle() {
        // An undo is addressed by the operation of the event it reverses; re-keying it to some other
        // operation leaves a clawback that answers for nothing.
        let dir = tempfile::TempDir::new().unwrap();
        let db = undone_paid_fixture(&dir, "undo-linkage", 730);
        unlock(&db, "review_compensation_ledger");
        assert!(
            db.connection()
                .execute(
                    "UPDATE review_compensation_ledger SET entry_key = 'undo:' || ?1
                      WHERE compensation_action='undo'",
                    [canonical_operation(731)],
                )
                .unwrap()
                >= 1
        );
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("invalid operation/event linkage"), "{error}");

        // A paid Couch decision at revision 0 never advanced the segment it claims to have decided.
        let db = file_paid_db(&dir, "zero-revision");
        listened(&db, "zero-revision");
        decided(&db, "zero-revision", 732);
        validate_review_compensation_semantics(&db).unwrap();
        unlock(&db, "review_compensation_ledger");
        assert_eq!(
            db.connection().execute("UPDATE review_compensation_ledger SET decision_revision=0", []).unwrap(),
            1
        );
        let error = validate_review_compensation_semantics(&db).unwrap_err();
        assert!(error.contains("disagrees with review event"), "{error}");
    }

    use super::{
        canonical_work_audio_identity, canonical_work_id_has_writer_shape, exact_review_entitlement,
        is_canonical_lowercase_64_hex, is_canonical_lowercase_uuid, review_action_basis_points,
        valid_compensation_reviewer,
    };

    const HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn work_id(reviewer_key: &str, hash: &str, start: i64, end: i64) -> String {
        format!("reviewer-work-v1:{}:{reviewer_key}:audio-segment-v1:{hash}:{start}:{end}", reviewer_key.len())
    }

    #[test]
    fn entitlement_is_the_published_rate_computed_exactly_or_refused() {
        // The policy is 18,000 IQD per audio-hour at full rate, so one second of `edit` work is
        // exactly 5 IQD = 5_000_000 micro-IQD. Asserting the literal keeps a rate change from
        // silently passing as "still exact".
        assert_eq!(exact_review_entitlement(1_000, crate::db::REVIEW_PAY_EDIT_BPS).unwrap(), 5_000_000);
        assert_eq!(exact_review_entitlement(3_600_000, crate::db::REVIEW_PAY_EDIT_BPS).unwrap(), 18_000_000_000);
        // accept/reject are a tenth of the edit rate; skip is unpaid and must compute to zero
        // rather than being refused -- a skip is legitimate work product, worth nothing.
        assert_eq!(exact_review_entitlement(1_000, crate::db::REVIEW_PAY_ACCEPT_BPS).unwrap(), 500_000);
        assert_eq!(exact_review_entitlement(1_000, crate::db::REVIEW_PAY_REJECT_BPS).unwrap(), 500_000);
        assert_eq!(exact_review_entitlement(1_000, crate::db::REVIEW_PAY_SKIP_BPS).unwrap(), 0);

        for (label, duration, bps) in [
            ("zero duration", 0_i64, 10_000_i64),
            ("negative duration", -1, 10_000),
            ("negative basis points", 1_000, -1),
            ("basis points above 100%", 1_000, 10_001),
        ] {
            let error = exact_review_entitlement(duration, bps).unwrap_err();
            assert!(error.contains("invalid duration or basis points"), "{label}: {error}");
        }

        // Money must divide exactly into micro-IQD. A fraction of a micro-IQD is refused rather
        // than rounded -- rounding is how a ledger and its reviewers quietly stop agreeing.
        let error = exact_review_entitlement(1, 1).unwrap_err();
        assert!(error.contains("not an exact micro-IQD amount"), "{error}");

        // An entitlement past i64 is refused rather than wrapped. 2e15 ms of audio is absurd, which
        // is the point: absurd input must fail loudly at the boundary, not become a negative debt.
        let error = exact_review_entitlement(2_000_000_000_000_000, crate::db::REVIEW_PAY_EDIT_BPS).unwrap_err();
        assert!(error.contains("exceeds the supported integer range"), "{error}");

        // NOTE: the i128 overflow arm above it is deliberately unreachable from here -- with
        // basis points capped at 10_000 and duration an i64, the product tops out around 1.7e33,
        // far below i128::MAX. It is defence in depth against a future caller, not dead code, and
        // is left untested rather than exercised through a fake path that would prove nothing.
    }

    #[test]
    fn every_paid_action_has_a_rate_and_nothing_else_does() {
        for (action, expected) in [
            ("edit", crate::db::REVIEW_PAY_EDIT_BPS),
            ("accept", crate::db::REVIEW_PAY_ACCEPT_BPS),
            ("reject", crate::db::REVIEW_PAY_REJECT_BPS),
            ("skip", crate::db::REVIEW_PAY_SKIP_BPS),
        ] {
            assert_eq!(review_action_basis_points(action), Some(expected), "{action}");
        }
        // Case and whitespace are NOT normalized here: an action that does not match exactly earns
        // nothing, so a ledger row carrying "Edit" cannot claim the edit rate.
        for unknown in ["Edit", "edit ", "", "approve", "undo", "flag"] {
            assert_eq!(review_action_basis_points(unknown), None, "{unknown:?} must not carry a rate");
        }
    }

    #[test]
    fn a_compensation_reviewer_identity_is_exact_and_bounded() {
        assert!(valid_compensation_reviewer("Alpha"));
        assert!(valid_compensation_reviewer(&"n".repeat(40)));
        for bad in ["", " Alpha", "Alpha ", "Al\u{0007}pha", "Al\npha"] {
            assert!(!valid_compensation_reviewer(bad), "{bad:?} must be refused");
        }
        assert!(!valid_compensation_reviewer(&"n".repeat(41)), "the 40-character bound must bind");
    }

    #[test]
    fn a_work_id_cannot_be_split_reattributed_or_invented() {
        // This is the anti-fraud rule the module's own comment names: without it "a forged target
        // could split one clip into several invented work ids and earn the full rate on every
        // split". A work id is only canonical if it reproduces the writer's exact namespace.
        let genuine = work_id("alpha", HASH, 0, 1_000);
        assert!(canonical_work_id_has_writer_shape(&genuine, "Alpha", 1_000), "the real shape must be accepted");
        assert_eq!(canonical_work_audio_identity(&genuine, "Alpha"), Some((HASH, 0, 1_000)));
        // The reviewer key is lowercased, so the same person typed differently still matches.
        assert!(canonical_work_id_has_writer_shape(&genuine, "  ALPHA  ", 1_000));

        // Re-attribution: one reviewer's work id must not validate under another's name, or paid
        // work could be moved between people by editing a single string.
        assert!(!canonical_work_id_has_writer_shape(&genuine, "Bravo", 1_000));
        assert_eq!(canonical_work_audio_identity(&genuine, "Bravo"), None);

        // The length prefix is why "alp" + "ha:..." cannot impersonate "alpha": the count and the
        // key must agree, so a shifted boundary fails instead of silently re-parsing.
        let forged_prefix = format!("reviewer-work-v1:3:alpha:audio-segment-v1:{HASH}:0:1000");
        assert!(!canonical_work_id_has_writer_shape(&forged_prefix, "Alpha", 1_000));

        // Splitting: half the clip claimed as a whole unit must not validate against the real
        // duration, which is what stops one paid clip becoming several.
        let split = work_id("alpha", HASH, 0, 500);
        assert!(!canonical_work_id_has_writer_shape(&split, "Alpha", 1_000), "a half span cannot claim a full clip");

        for (label, id) in [
            ("non-hex content hash", work_id("alpha", &"z".repeat(64), 0, 1_000)),
            ("short content hash", work_id("alpha", &"a".repeat(63), 0, 1_000)),
            ("uppercase content hash", work_id("alpha", &HASH.to_uppercase(), 0, 1_000)),
            ("inverted span", work_id("alpha", HASH, 1_000, 0)),
            ("unparseable span", format!("reviewer-work-v1:5:alpha:audio-segment-v1:{HASH}:zero:1000")),
            ("extra identity segment", format!("reviewer-work-v1:5:alpha:audio-segment-v1:{HASH}:0:1000:9")),
            ("wrong namespace", format!("reviewer-work-v2:5:alpha:audio-segment-v1:{HASH}:0:1000")),
            ("bare identity", format!("{HASH}:0:1000")),
        ] {
            assert!(!canonical_work_id_has_writer_shape(&id, "Alpha", 1_000), "{label} must be refused");
            assert_eq!(canonical_work_audio_identity(&id, "Alpha"), None, "{label} must yield no identity");
        }

        // A negative start is refused by the identity reader even though the shape check is
        // satisfied by the span/duration rule -- the two guards do not rely on each other.
        let negative = work_id("alpha", HASH, -1_000, 0);
        assert_eq!(canonical_work_audio_identity(&negative, "Alpha"), None);
    }

    #[test]
    fn canonical_digest_and_uuid_shapes_are_strict() {
        assert!(is_canonical_lowercase_64_hex(HASH));
        for bad in ["", &"a".repeat(63), &"a".repeat(65), &HASH.to_uppercase(), &"g".repeat(64)] {
            assert!(!is_canonical_lowercase_64_hex(bad), "{bad:?}");
        }
        // A UUID WITH HEX LETTERS in it: the first version of this test used
        // "00000000-0000-4000-8000-000000000001", whose .to_uppercase() is a no-op because it
        // contains no letters -- so the "uppercase is refused" case was asserting against the
        // valid value and failed. The case only means something with letters to re-case.
        let canonical = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
        assert!(is_canonical_lowercase_uuid(canonical));
        for bad in [canonical.to_uppercase(), canonical.replace('-', ""), "not-a-uuid".to_string(), String::new()] {
            assert!(!is_canonical_lowercase_uuid(&bad), "{bad:?}");
        }
    }
}
