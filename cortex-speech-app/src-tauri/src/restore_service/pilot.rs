//! Controlled-pilot authority validation across restore generations.

use crate::recovery::SnapshotPilotPolicyRestore;

fn validate_pilot_hidden_structural_namespaces(db: &crate::db::Database, context: &str) -> Result<(), String> {
    let structural_violation: bool = db
        .connection()
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM (
                     SELECT after_review_event_id
                       FROM review_pilot_hidden_keys
                      GROUP BY after_review_event_id
                     HAVING COUNT(DISTINCT policy_sha256) > 1
                 )
                 UNION ALL
                 SELECT 1 FROM (
                     SELECT policy_sha256, after_review_event_id, reviewer
                       FROM review_pilot_hidden_keys
                      GROUP BY policy_sha256, after_review_event_id, reviewer COLLATE NOCASE
                     HAVING COUNT(*) > 2
                 )
                 UNION ALL
                 SELECT 1 FROM (
                     SELECT policy_sha256, after_review_event_id
                       FROM review_pilot_hidden_keys
                      GROUP BY policy_sha256, after_review_event_id
                     HAVING COUNT(*) > 4
                 )
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("{context} hidden-key structural quotas are unreadable: {error}"))?;
    if structural_violation {
        return Err(format!(
            "database restore refused: {context} contains a historical hidden-key namespace that violates one-policy-per-baseline or grant quotas"
        ));
    }
    Ok(())
}

fn validate_pilot_hidden_namespace(
    db: &crate::db::Database,
    policy: &crate::review_pilot::ReviewPilotPolicy,
    context: &str,
) -> Result<(), String> {
    let policy_sha256 = policy.policy_sha256()?;
    let baseline = policy.after_review_event_id;
    let maximum_event_id: i64 = db
        .connection()
        .query_row("SELECT COALESCE(MAX(id), 0) FROM review_events", [], |row| row.get(0))
        .map_err(|error| format!("{context} pilot review history is unreadable: {error}"))?;
    if baseline > maximum_event_id {
        return Err(format!(
            "database restore refused: {context} pilot baseline {baseline} is ahead of review history maximum {maximum_event_id}"
        ));
    }
    let inconsistent_namespace: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM review_pilot_hidden_keys
              WHERE (policy_sha256 = ?1 OR after_review_event_id = ?2)
                AND NOT (policy_sha256 = ?1 AND after_review_event_id = ?2)",
            rusqlite::params![policy_sha256, baseline],
            |row| row.get(0),
        )
        .map_err(|error| format!("{context} pilot hidden-key namespace is unreadable: {error}"))?;
    if inconsistent_namespace != 0 {
        return Err(format!(
            "database restore refused: {context} has {inconsistent_namespace} hidden-key grant(s) inconsistent with its active policy SHA/baseline"
        ));
    }

    let mut statement = db
        .connection()
        .prepare(
            "SELECT reviewer, COUNT(*) FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
              GROUP BY reviewer COLLATE NOCASE",
        )
        .map_err(|error| format!("{context} pilot hidden-key roster is unreadable: {error}"))?;
    let reviewer_counts = statement
        .query_map(rusqlite::params![policy_sha256, baseline], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|error| format!("{context} pilot hidden-key roster is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} pilot hidden-key roster is unreadable: {error}"))?;
    let mut total = 0i64;
    for (reviewer, count) in reviewer_counts {
        if policy.cap_for(&reviewer).is_none() {
            return Err(format!(
                "database restore refused: {context} hidden-key namespace contains a reviewer outside its exact policy roster"
            ));
        }
        if count > crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER {
            return Err(format!(
                "database restore refused: {context} hidden-key namespace exceeds the per-reviewer grant ceiling"
            ));
        }
        total += count;
    }
    if total > crate::review_pilot::REVIEW_PILOT_TOTAL_HIDDEN_QC {
        return Err(format!(
            "database restore refused: {context} hidden-key namespace exceeds the global grant ceiling"
        ));
    }
    Ok(())
}

pub(crate) fn validate_active_pilot_semantics(
    db: &crate::db::Database,
    policy: &crate::review_pilot::ReviewPilotPolicy,
    context: &str,
) -> Result<(), String> {
    use std::collections::{HashMap, HashSet};

    let policy_sha256 = policy.policy_sha256()?;
    let baseline = policy.after_review_event_id;
    let authorized =
        policy.reviewer_names().into_iter().map(|name| (name.to_ascii_lowercase(), name)).collect::<HashMap<_, _>>();
    let reviewer_key = |actual: &str| {
        let key = actual.trim().to_ascii_lowercase();
        authorized.contains_key(&key).then_some(key)
    };

    let mut grants = authorized.keys().map(|key| (key.clone(), HashSet::new())).collect::<HashMap<_, _>>();
    let mut grant_statement = db
        .connection()
        .prepare(
            "SELECT reviewer, segment_id FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2
              ORDER BY reviewer COLLATE NOCASE, segment_id",
        )
        .map_err(|error| format!("{context} active pilot grants are unreadable: {error}"))?;
    let grant_rows = grant_statement
        .query_map(rusqlite::params![policy_sha256, baseline], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| format!("{context} active pilot grants are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} active pilot grants are unreadable: {error}"))?;
    drop(grant_statement);
    for (reviewer, segment_id) in grant_rows {
        let key = reviewer_key(&reviewer).ok_or_else(|| {
            format!("database restore refused: {context} active pilot grant has an unauthorized reviewer")
        })?;
        let reviewer_grants = grants
            .get_mut(&key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        if !reviewer_grants.insert(segment_id) {
            return Err(format!("database restore refused: {context} active pilot contains a duplicate grant"));
        }
    }

    let mut corpus_actions = authorized.keys().map(|key| (key.clone(), 0i64)).collect::<HashMap<_, _>>();
    let mut hidden_actions = authorized.keys().map(|key| (key.clone(), 0i64)).collect::<HashMap<_, _>>();
    let mut completed = authorized.keys().map(|key| (key.clone(), HashSet::new())).collect::<HashMap<_, _>>();
    let mut skipped = authorized.keys().map(|key| (key.clone(), HashSet::new())).collect::<HashMap<_, _>>();
    let mut hidden_event_actions = HashMap::<(String, String), String>::new();

    let mut event_statement = db
        .connection()
        .prepare(
            "SELECT id, segment_id, reviewer, action, source FROM review_events
              WHERE id > ?1 AND source IN ('couch', 'couch_spot_check')
              ORDER BY id",
        )
        .map_err(|error| format!("{context} post-baseline pilot history is unreadable: {error}"))?;
    let events = event_statement
        .query_map([baseline], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| format!("{context} post-baseline pilot history is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} post-baseline pilot history is unreadable: {error}"))?;
    drop(event_statement);

    for (event_id, segment_id, reviewer, action, source) in events {
        let key = reviewer_key(&reviewer).ok_or_else(|| {
            format!(
                "database restore refused: {context} post-baseline pilot event {event_id} has an unauthorized reviewer"
            )
        })?;
        if !matches!(action.as_str(), "accept" | "edit" | "reject" | "skip") {
            return Err(format!(
                "database restore refused: {context} post-baseline pilot event {event_id} has an invalid action"
            ));
        }
        let is_grant = grants.get(&key).is_some_and(|segments| segments.contains(&segment_id));
        if source == "couch" {
            let corpus_count = corpus_actions
                .get_mut(&key)
                .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
            *corpus_count += 1;
            if is_grant {
                // Pre-v59/session-backed hidden skips were recorded as ordinary Couch skips. Keep
                // recognizing that exact history: it consumes a corpus slot and resolves the grant,
                // but any non-skip corpus finalization of a hidden key is corruption.
                if action != "skip" {
                    return Err(format!(
                        "database restore refused: {context} reserved hidden key was non-skip finalized through the corpus path"
                    ));
                }
                let reviewer_skips = skipped
                    .get_mut(&key)
                    .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
                if !reviewer_skips.insert(segment_id) {
                    return Err(format!(
                        "database restore refused: {context} reserved hidden key was resolved more than once"
                    ));
                }
            }
            continue;
        }

        if !is_grant {
            return Err(format!(
                "database restore refused: {context} hidden-check event {event_id} has no active durable grant"
            ));
        }
        if completed.get(&key).is_some_and(|segments| segments.contains(&segment_id))
            || skipped.get(&key).is_some_and(|segments| segments.contains(&segment_id))
        {
            return Err(format!("database restore refused: {context} reserved hidden key was resolved more than once"));
        }
        if hidden_event_actions.insert((key.clone(), segment_id.clone()), action.clone()).is_some() {
            return Err(format!("database restore refused: {context} reserved hidden key has duplicate hidden events"));
        }
        if action == "skip" {
            skipped
                .get_mut(&key)
                .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?
                .insert(segment_id);
        } else {
            completed
                .get_mut(&key)
                .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?
                .insert(segment_id);
        }
        let hidden_count = hidden_actions
            .get_mut(&key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        *hidden_count += 1;
    }

    let mut result_statement = db
        .connection()
        .prepare(
            "SELECT key.reviewer, key.segment_id, result.action,
                    result.submitted_transcript, result.expected_transcript,
                    result.noticed, result.cer
               FROM review_pilot_hidden_keys key
               JOIN spot_checks result
                 ON result.segment_id = key.segment_id
                AND result.reviewer = key.reviewer COLLATE NOCASE
              WHERE key.policy_sha256 = ?1 AND key.after_review_event_id = ?2
              ORDER BY key.reviewer COLLATE NOCASE, key.segment_id",
        )
        .map_err(|error| format!("{context} active pilot results are unreadable: {error}"))?;
    let result_rows = result_statement
        .query_map(rusqlite::params![policy_sha256, baseline], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, f64>(6)?,
            ))
        })
        .map_err(|error| format!("{context} active pilot results are unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} active pilot results are unreadable: {error}"))?;
    drop(result_statement);
    let mut result_actions = HashMap::<(String, String), Vec<String>>::new();
    for (reviewer, segment_id, action, submitted, expected, noticed, cer) in result_rows {
        let key = reviewer_key(&reviewer).ok_or_else(|| {
            format!("database restore refused: {context} hidden-check result has an unauthorized reviewer")
        })?;
        let submitted = crate::db::to_nfc(submitted.trim());
        let expected = crate::db::to_nfc(expected.trim());
        let expected_noticed = action != "reject"
            && crate::normalizer::learning_text_key(&submitted) == crate::normalizer::learning_text_key(&expected);
        let expected_cer = crate::wer::compute_cer(&expected, &submitted);
        let cer_tolerance = 1e-12_f64.max(expected_cer.abs() * f64::EPSILON * 8.0);
        if !matches!(noticed, 0 | 1)
            || (noticed != 0) != expected_noticed
            || !cer.is_finite()
            || !expected_cer.is_finite()
            || (cer - expected_cer).abs() > cer_tolerance
        {
            return Err(format!(
                "database restore refused: {context} hidden-check result has impossible noticed/CER semantics"
            ));
        }
        result_actions.entry((key, segment_id)).or_default().push(action);
    }
    for (key, expected_action) in &hidden_event_actions {
        match result_actions.get(key) {
            Some(observed) if observed.len() == 1 && observed[0] == *expected_action => {}
            _ => {
                return Err(format!(
                    "database restore refused: {context} hidden-check event/result actions do not match exactly"
                ));
            }
        }
    }
    if result_actions.keys().any(|key| !hidden_event_actions.contains_key(key)) {
        return Err(format!(
            "database restore refused: {context} has an orphan hidden-check result without a matching event"
        ));
    }

    // A corpus verdict and its event/ledger are one database transaction in current builds. A
    // restored target may nevertheless contain pre-existing rows from an older half-write or a
    // crafted extra. For every CURRENT decision attributed to this active roster, require the latest
    // still-active campaign event to describe exactly that state. A fully reversed campaign chain is
    // allowed because atomic Undo deliberately restores the prior row snapshot.
    let reversed_entries = {
        let mut statement = db
            .connection()
            .prepare(
                "SELECT reverses_entry_id FROM review_compensation_ledger
                  WHERE policy_version = ?1 AND compensation_action = 'undo'
                    AND source = 'couch_undo' AND reverses_entry_id IS NOT NULL",
            )
            .map_err(|error| format!("{context} pilot undo ledger is unreadable: {error}"))?;
        let rows = statement
            .query_map([crate::db::REVIEW_PAY_POLICY_VERSION], |row| row.get::<_, String>(0))
            .map_err(|error| format!("{context} pilot undo ledger is unreadable: {error}"))?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(|error| format!("{context} pilot undo ledger is unreadable: {error}"))?;
        rows
    };
    let mut active_corpus = HashMap::<String, (i64, String, String, i64)>::new();
    let mut corpus_statement = db
        .connection()
        .prepare(
            "SELECT event.id, event.segment_id, event.reviewer, event.action,
                    (SELECT COUNT(*) FROM review_compensation_ledger ledger
                      WHERE ledger.policy_version = ?2 AND ledger.review_event_id = event.id),
                    (SELECT entry_id FROM review_compensation_ledger ledger
                      WHERE ledger.policy_version = ?2 AND ledger.review_event_id = event.id
                      ORDER BY ledger.id LIMIT 1),
                    (SELECT decision_revision FROM review_compensation_ledger ledger
                      WHERE ledger.policy_version = ?2 AND ledger.review_event_id = event.id
                      ORDER BY ledger.id LIMIT 1)
               FROM review_events event
              WHERE event.id > ?1 AND event.source = 'couch'
                AND event.action IN ('accept','edit','reject')
              ORDER BY event.id",
        )
        .map_err(|error| format!("{context} pilot corpus-state history is unreadable: {error}"))?;
    let corpus_rows = corpus_statement
        .query_map(rusqlite::params![baseline, crate::db::REVIEW_PAY_POLICY_VERSION], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(|error| format!("{context} pilot corpus-state history is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} pilot corpus-state history is unreadable: {error}"))?;
    drop(corpus_statement);
    for (event_id, segment_id, reviewer, action, ledger_count, entry_id, decision_revision) in corpus_rows {
        if ledger_count != 1 || entry_id.is_none() || decision_revision.is_none() {
            return Err(format!(
                "database restore refused: {context} corpus event {event_id} lacks one valid compensation ledger entry"
            ));
        }
        let entry_id = entry_id.ok_or_else(|| {
            format!("database restore refused: {context} corpus event {event_id} has no ledger identity")
        })?;
        if !reversed_entries.contains(&entry_id) {
            let decision_revision = decision_revision.ok_or_else(|| {
                format!("database restore refused: {context} corpus event {event_id} has no decision revision")
            })?;
            active_corpus.insert(segment_id, (event_id, reviewer, action, decision_revision));
        }
    }

    let mut current_statement = db
        .connection()
        .prepare(
            "SELECT id, reviewed_by, human_decision FROM speech_segments
              WHERE reviewed_by IS NOT NULL AND human_decision IN ('accept','edit','reject')",
        )
        .map_err(|error| format!("{context} current reviewed corpus state is unreadable: {error}"))?;
    let current_rows = current_statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        .map_err(|error| format!("{context} current reviewed corpus state is unreadable: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} current reviewed corpus state is unreadable: {error}"))?;
    drop(current_statement);
    for (segment_id, reviewer, decision) in current_rows {
        if reviewer_key(&reviewer).is_none() {
            continue;
        }
        match active_corpus.get(&segment_id) {
            Some((_, event_reviewer, event_action, _))
                if event_reviewer.trim().eq_ignore_ascii_case(reviewer.trim()) && event_action == &decision => {}
            None => {
                use rusqlite::OptionalExtension;
                let prior: Option<(String, String)> = db
                    .connection()
                    .query_row(
                        "SELECT reviewer, action FROM review_events
                          WHERE id <= ?1 AND segment_id = ?2 AND source = 'couch'
                            AND action IN ('accept','edit','reject')
                          ORDER BY id DESC LIMIT 1",
                        rusqlite::params![baseline, segment_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|error| format!("{context} pre-pilot corpus state is unreadable: {error}"))?;
                if !prior.is_some_and(|(prior_reviewer, prior_action)| {
                    prior_reviewer.trim().eq_ignore_ascii_case(reviewer.trim()) && prior_action == decision
                }) {
                    return Err(format!(
                        "database restore refused: {context} current reviewed segment {segment_id} has no matching active campaign event/ledger"
                    ));
                }
                // When all campaign entries were reversed, the exact prior event above proves the
                // reviewed row is the state atomic Undo restored. With no prior event, a normal Undo
                // restores an unreviewed row, which never enters this scan; any reviewed row is forged.
            }
            _ => {
                return Err(format!(
                    "database restore refused: {context} current reviewed segment {segment_id} has no matching active campaign event/ledger"
                ));
            }
        }
    }
    for (segment_id, (event_id, event_reviewer, event_action, decision_revision)) in &active_corpus {
        use rusqlite::OptionalExtension;
        let current: Option<(i64, Option<String>, Option<String>)> = db
            .connection()
            .query_row(
                "SELECT COALESCE(review_revision, 0), human_decision, reviewed_by
                   FROM speech_segments WHERE id = ?1",
                [segment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("{context} campaign segment state is unreadable: {error}"))?;
        if let Some((current_revision, current_decision, current_reviewer)) = current {
            if current_revision == *decision_revision
                && (current_decision.as_deref() != Some(event_action.as_str())
                    || !current_reviewer
                        .as_deref()
                        .is_some_and(|value| value.trim().eq_ignore_ascii_case(event_reviewer.trim())))
            {
                return Err(format!(
                    "database restore refused: {context} corpus event {event_id} has no matching current-revision segment state"
                ));
            }
        }
    }

    for key in authorized.keys() {
        let reviewer_completed = completed
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let reviewer_skipped = skipped
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        if reviewer_completed.intersection(reviewer_skipped).next().is_some() {
            return Err(format!(
                "database restore refused: {context} hidden key has both completed and skipped resolution"
            ));
        }
        let corpus = *corpus_actions
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let hidden = *hidden_actions
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let reviewer = authorized
            .get(key)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer map is inconsistent"))?;
        let corpus_cap = policy
            .cap_for(reviewer)
            .ok_or_else(|| format!("database restore refused: {context} pilot reviewer cap is inconsistent"))?;
        if corpus > corpus_cap {
            return Err(format!("database restore refused: {context} exceeds the per-reviewer corpus-action ceiling"));
        }
        if hidden > crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER {
            return Err(format!("database restore refused: {context} exceeds the per-reviewer hidden-action ceiling"));
        }
        if corpus + hidden
            > crate::review_pilot::REVIEW_PILOT_CORPUS_ACTIONS_PER_REVIEWER
                + crate::review_pilot::REVIEW_PILOT_HIDDEN_QC_PER_REVIEWER
        {
            return Err(format!("database restore refused: {context} exceeds the per-reviewer UI-action ceiling"));
        }
    }
    let corpus_total: i64 = corpus_actions.values().sum();
    let hidden_total: i64 = hidden_actions.values().sum();
    if corpus_total > policy.max_total_corpus_actions {
        return Err(format!("database restore refused: {context} exceeds the global corpus-action ceiling"));
    }
    if hidden_total > crate::review_pilot::REVIEW_PILOT_TOTAL_HIDDEN_QC {
        return Err(format!("database restore refused: {context} exceeds the global hidden-action ceiling"));
    }
    if corpus_total + hidden_total > crate::review_pilot::REVIEW_PILOT_MAX_COMPENSATED_UI_ACTIONS {
        return Err(format!("database restore refused: {context} exceeds the global UI-action ceiling"));
    }
    Ok(())
}

/// If the authoritative floor has begun using its active controlled-pilot identity, the target must
/// carry that exact semantic policy. Baseline alone is insufficient: changing the roster at the same
/// event id would reinterpret grants and mint a fresh paid-action namespace.
pub(crate) fn require_active_pilot_policy_binding(
    floor: &crate::db::Database,
    floor_policy: Option<&crate::review_pilot::ReviewPilotPolicy>,
    target: &crate::db::Database,
    target_action: &SnapshotPilotPolicyRestore,
) -> Result<(), String> {
    validate_pilot_hidden_structural_namespaces(target, "target snapshot")?;
    let target_policy = match target_action {
        SnapshotPilotPolicyRestore::Install(bytes) => {
            let raw = std::str::from_utf8(bytes)
                .map_err(|error| format!("target snapshot pilot policy is not UTF-8: {error}"))?;
            Some(crate::review_pilot::parse(raw)?)
        }
        SnapshotPilotPolicyRestore::ExplicitlyAbsent | SnapshotPilotPolicyRestore::PreserveLegacy => None,
    };
    if let Some(policy) = target_policy.as_ref() {
        // Triggers constrain future INSERTs but cannot prove that pre-existing rows obeyed them.
        // Validate the migrated staged generation itself before it can replace one live page.
        validate_pilot_hidden_namespace(target, policy, "target snapshot")?;
        validate_active_pilot_semantics(target, policy, "target snapshot")?;
    }
    let Some(floor_policy) = floor_policy else {
        return Ok(());
    };
    validate_pilot_hidden_structural_namespaces(floor, "authoritative floor")?;
    validate_pilot_hidden_namespace(floor, floor_policy, "authoritative floor")?;
    let policy_sha256 = floor_policy.policy_sha256()?;
    let baseline = floor_policy.after_review_event_id;
    let grants: i64 = floor
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM review_pilot_hidden_keys
              WHERE policy_sha256 = ?1 AND after_review_event_id = ?2",
            rusqlite::params![policy_sha256, baseline],
            |row| row.get(0),
        )
        .map_err(|error| format!("authoritative pilot hidden-key grants are unreadable: {error}"))?;
    let activity: i64 = floor
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM review_events
              WHERE id > ?1 AND source IN ('couch', 'couch_spot_check')",
            [baseline],
            |row| row.get(0),
        )
        .map_err(|error| format!("authoritative pilot review activity is unreadable: {error}"))?;
    if grants == 0 && activity == 0 {
        return Ok(());
    }

    let Some(target_policy) = target_policy else {
        return Err(
            "database restore refused: the authoritative floor has policy-bound pilot grants/activity, but the target does not cryptographically bind that policy"
                .to_string(),
        );
    };
    let target_sha256 = target_policy.policy_sha256()?;
    if target_policy != *floor_policy || target_sha256 != policy_sha256 {
        return Err(
            "database restore refused: target pilot policy identity differs from the authoritative policy already used for grants/activity"
                .to_string(),
        );
    }
    Ok(())
}
