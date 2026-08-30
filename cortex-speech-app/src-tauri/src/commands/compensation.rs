//! Owner-desktop reviewer-compensation commands: the exact balances, and the canon-mandated
//! immutable settlement record. Until 2026-08-30 `record_review_compensation_settlement` had no
//! production caller, so a real cash payout could not be recorded anywhere — the phone kept
//! showing the full outstanding balance forever and a dispute had no ledger anchor.

use crate::error::AppError;
use crate::ipc_contract::{CommandErrorV1, ReviewCompensationSettlementV1, ReviewerCompensationOverviewV1};
use crate::AppState;
use tauri::State;

use super::RATE_LIMITER;

fn public_money_error(error: &AppError) -> CommandErrorV1 {
    match error {
        // Validation text here is already closed and bounded (policy identity, range, reference
        // rules) — it is exactly what the owner needs to correct the request.
        AppError::Validation(message) => CommandErrorV1::new("SETTLEMENT_REFUSED", message, false),
        other => {
            tracing::error!(error = %other, "compensation command failed");
            CommandErrorV1::new(
                "COMPENSATION_UNAVAILABLE",
                "The compensation ledger could not be read or written. Nothing was recorded; retry.",
                true,
            )
        }
    }
}

/// Every ledger payee with exact earned / settled / outstanding totals under the active policy,
/// plus the inclusive ledger boundary a settlement may claim.
#[tauri::command]
#[specta::specta]
pub fn get_review_compensation_overview_v1(
    state: State<'_, AppState>,
) -> Result<Vec<ReviewerCompensationOverviewV1>, CommandErrorV1> {
    RATE_LIMITER
        .check("get_review_compensation_overview_v1")
        .map_err(|_| CommandErrorV1::new("RATE_LIMITED", "Too many balance reads. Retry in a moment.", true))?;
    let rows = state.compensation_store().overview().map_err(|error| public_money_error(&error))?;
    Ok(rows
        .into_iter()
        .map(|row| ReviewerCompensationOverviewV1 {
            reviewer: row.reviewer,
            policy_version: row.summary.policy_version,
            earned_micro_iqd: row.summary.earned_micro_iqd.to_string(),
            settled_micro_iqd: row.summary.settled_micro_iqd.to_string(),
            outstanding_micro_iqd: row.summary.outstanding_micro_iqd.to_string(),
            corrected_audio_ms: row.summary.corrected_audio_ms,
            legacy_events_pending_reconciliation: row.summary.legacy_events_pending_reconciliation as u32,
            max_ledger_id: row.max_ledger_id,
        })
        .collect())
}

/// Record one immutable external payout for one reviewer, through the exact ledger boundary the
/// overview reported. Idempotent by payout reference (a lost response replays the original
/// allocation); a reused reference with different parameters is a hard error; paid history is
/// never rewritten — a later reversal stays visible as outstanding adjustment.
#[tauri::command]
#[specta::specta]
pub fn record_review_compensation_settlement_v1(
    state: State<'_, AppState>,
    reviewer: String,
    through_ledger_id_inclusive: i64,
    payout_reference: String,
) -> Result<ReviewCompensationSettlementV1, CommandErrorV1> {
    RATE_LIMITER
        .check("record_review_compensation_settlement_v1")
        .map_err(|_| CommandErrorV1::new("RATE_LIMITED", "Too many settlement attempts. Retry in a moment.", true))?;
    let reviewer = reviewer.trim();
    let payout_reference = payout_reference.trim();
    if reviewer.is_empty() || payout_reference.is_empty() || payout_reference.len() > 200 {
        return Err(CommandErrorV1::new(
            "SETTLEMENT_REFUSED",
            "A settlement needs the reviewer and a payout reference (at most 200 characters).",
            false,
        ));
    }
    if through_ledger_id_inclusive <= 0 {
        return Err(CommandErrorV1::new(
            "SETTLEMENT_REFUSED",
            "The settlement boundary must be the positive ledger id the overview reported.",
            false,
        ));
    }
    let settlement = state
        .compensation_store()
        .settle(reviewer, through_ledger_id_inclusive, payout_reference)
        .map_err(|error| public_money_error(&error))?;
    Ok(ReviewCompensationSettlementV1 {
        settlement_id: settlement.settlement_id,
        policy_version: settlement.policy_version,
        reviewer: settlement.reviewer,
        from_ledger_id_exclusive: settlement.from_ledger_id_exclusive,
        through_ledger_id_inclusive: settlement.through_ledger_id_inclusive,
        allocated_micro_iqd: settlement.allocated_micro_iqd.to_string(),
        payout_reference: settlement.payout_reference,
    })
}

#[cfg(test)]
mod tests {
    // The deep money semantics (range contiguity, idempotent replay, reversal visibility, trigger
    // re-validation) are pinned where they live, in db tests around
    // `record_review_compensation_settlement`. These tests pin THIS boundary: the wire totals are
    // exact decimal strings and the store round-trips a real settlement.
    use crate::db::Database;

    #[test]
    fn overview_reports_exact_string_totals_and_settlement_round_trips() {
        // File-backed, not :memory:: the store reads through bounded WAL snapshots (open_read),
        // which an in-memory database cannot serve.
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("compensation.db").to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        seed_one_paid_credit(&database);
        let store = crate::stores::CompensationStore::new(crate::database_runtime::DatabaseRuntime::new(database));

        let overview = store.overview().unwrap();
        assert_eq!(overview.len(), 1, "one folded payee expected");
        let row = &overview[0];
        assert_eq!(row.summary.earned_micro_iqd, 5_000_000, "1s edit = 5_000_000 micro-IQD under the canon rate");
        assert_eq!(row.summary.outstanding_micro_iqd, 5_000_000);
        assert!(row.max_ledger_id > 0);

        let settlement = store.settle(&row.reviewer, row.max_ledger_id, "payout-test-0001").unwrap();
        assert_eq!(settlement.allocated_micro_iqd, 5_000_000);
        // Idempotent replay: the same durable reference returns the ORIGINAL allocation.
        let replay = store.settle(&row.reviewer, row.max_ledger_id, "payout-test-0001").unwrap();
        assert_eq!(replay.settlement_id, settlement.settlement_id);

        let after = store.overview().unwrap();
        assert_eq!(after[0].summary.settled_micro_iqd, 5_000_000);
        assert_eq!(after[0].summary.outstanding_micro_iqd, 0, "a recorded payout must retire the balance");
    }

    fn seed_one_paid_credit(database: &Database) {
        database
            .insert_segment(&crate::db::SpeechSegment {
                id: "clip".into(),
                audio_path: "clip.wav".into(),
                raw_transcript: "draft".into(),
                duration_ms: 1_000,
                ..crate::db::SpeechSegment::default()
            })
            .unwrap();
        // Pay demands CANONICAL work identity (owner canon: every credit snapshots its exact work),
        // which is the audio content hash plus a real source span — same shape as the db_tests pay
        // fixtures. Without it the recorder refuses rather than minting a row-identity credit.
        database
            .connection()
            .execute(
                "UPDATE speech_segments
                    SET audio_content_hash = ?1,
                        alignment_json = json_object('source_start_ms', 0, 'source_end_ms', duration_ms)
                  WHERE id = 'clip'",
                rusqlite::params!["a".repeat(64)],
            )
            .unwrap();
        let revision = database.segment_review_revision("clip").unwrap().unwrap();
        database
            .record_phone_human_decision_by_at_revision("clip", "edit", Some("truth"), "Rubar", revision)
            .unwrap()
            .unwrap();
    }
}
