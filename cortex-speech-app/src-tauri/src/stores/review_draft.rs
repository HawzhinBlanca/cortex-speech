//! Non-authoritative crash-safe desktop review drafts.

use crate::database_runtime::DatabaseRuntime;
use crate::error::{AppError, AppResult};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewDraftRecord {
    pub(crate) segment_id: String,
    pub(crate) base_revision: i64,
    pub(crate) text: String,
    pub(crate) updated_at: String,
}

#[derive(Clone)]
pub(crate) struct ReviewDraftStore {
    runtime: DatabaseRuntime,
}

impl ReviewDraftStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    fn read_on(connection: &Connection, segment_id: &str) -> AppResult<Option<ReviewDraftRecord>> {
        connection
            .query_row(
                "SELECT segment_id, base_revision, text, updated_at
                   FROM review_drafts WHERE segment_id = ?1",
                [segment_id],
                |row| {
                    Ok(ReviewDraftRecord {
                        segment_id: row.get(0)?,
                        base_revision: row.get(1)?,
                        text: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(AppError::from)
    }

    pub(crate) fn get(&self, segment_id: &str) -> AppResult<Option<ReviewDraftRecord>> {
        let database = self.runtime.open_read()?;
        Self::read_on(database.connection(), segment_id)
    }

    pub(crate) fn reserve_write(&self, segment_id: &str, operation_id: &str) -> AppResult<()> {
        self.runtime.reserve_review_draft_write(segment_id, operation_id)
    }

    pub(crate) fn save(
        &self,
        segment_id: &str,
        base_revision: i64,
        text: &str,
        operation_id: &str,
    ) -> AppResult<ReviewDraftRecord> {
        self.runtime.with_reserved_review_draft_write(segment_id, operation_id, |mutation| {
            let database = self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
                tracing::warn!("Recovering poisoned database lock while saving a review draft");
                poisoned.into_inner()
            });
            database.with_full_sync(|| {
                let tx = rusqlite::Transaction::new_unchecked(
                    database.connection(),
                    rusqlite::TransactionBehavior::Immediate,
                )?;
                let current_revision = tx
                    .query_row("SELECT review_revision FROM speech_segments WHERE id = ?1", [segment_id], |row| {
                        row.get::<_, i64>(0)
                    })
                    .optional()?;
                let Some(current_revision) = current_revision else {
                    return Err(AppError::Validation("E_REVIEW_DRAFT_SEGMENT_NOT_FOUND".into()));
                };
                if current_revision != base_revision {
                    return Err(AppError::Validation(format!(
                        "E_STALE_REVIEW_DRAFT: expected revision {base_revision}, current revision {current_revision}"
                    )));
                }
                tx.execute(
                    "INSERT INTO review_drafts (segment_id, base_revision, text, updated_at)
                 VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 ON CONFLICT(segment_id) DO UPDATE SET
                     base_revision = excluded.base_revision,
                     text = excluded.text,
                     updated_at = excluded.updated_at",
                    params![segment_id, base_revision, text],
                )?;
                let saved = Self::read_on(&tx, segment_id)?
                    .ok_or_else(|| AppError::Other("review draft disappeared after its durable save".into()))?;
                tx.commit()?;
                Ok(saved)
            })
        })
    }

    pub(crate) fn delete_if_revision(
        &self,
        segment_id: &str,
        base_revision: i64,
        operation_id: &str,
    ) -> AppResult<bool> {
        self.runtime.with_reserved_review_draft_write(segment_id, operation_id, |mutation| {
            let database = self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
                tracing::warn!("Recovering poisoned database lock while deleting a review draft");
                poisoned.into_inner()
            });
            database.with_full_sync(|| {
                Ok(database.connection().execute(
                    "DELETE FROM review_drafts WHERE segment_id = ?1 AND base_revision = ?2",
                    params![segment_id, base_revision],
                )? > 0)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn store() -> (tempfile::TempDir, ReviewDraftStore, DatabaseRuntime) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("drafts.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .connection()
            .execute(
                "INSERT INTO speech_segments (id, audio_path, raw_transcript) VALUES ('clip', 'clip.wav', 'draft')",
                [],
            )
            .unwrap();
        let runtime = DatabaseRuntime::isolated_for_test(database);
        (directory, ReviewDraftStore::new(runtime.clone()), runtime)
    }

    fn save(
        store: &ReviewDraftStore,
        operation_id: &str,
        base_revision: i64,
        text: &str,
    ) -> AppResult<ReviewDraftRecord> {
        store.reserve_write("clip", operation_id)?;
        store.save("clip", base_revision, text, operation_id)
    }

    fn delete(store: &ReviewDraftStore, operation_id: &str, base_revision: i64) -> AppResult<bool> {
        store.reserve_write("clip", operation_id)?;
        store.delete_if_revision("clip", base_revision, operation_id)
    }

    #[test]
    fn draft_round_trip_is_server_timestamped_and_revision_guarded_on_delete() {
        let (_directory, store, _runtime) = store();
        let first = save(&store, "save-first", 0, "هەڵە").unwrap();
        assert_eq!(first.segment_id, "clip");
        assert_eq!(first.base_revision, 0);
        assert_eq!(first.text, "هەڵە");
        assert!(first.updated_at.ends_with('Z'));
        assert_eq!(store.get("clip").unwrap(), Some(first));

        assert!(!delete(&store, "delete-stale-revision", 1).unwrap());
        assert!(store.get("clip").unwrap().is_some());
        assert!(delete(&store, "delete-current-revision", 0).unwrap());
        assert!(store.get("clip").unwrap().is_none());
    }

    #[test]
    fn draft_is_not_review_truth_and_cascades_only_when_its_segment_is_deleted() {
        let (_directory, store, runtime) = store();
        save(&store, "save-local-only", 0, "local only").unwrap();
        {
            let database = runtime.lock().unwrap();
            let truth: (i64, Option<String>) = database
                .connection()
                .query_row("SELECT verified, human_decision FROM speech_segments WHERE id = 'clip'", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap();
            assert_eq!(truth, (0, None));
            database.connection().execute("DELETE FROM speech_segments WHERE id = 'clip'", []).unwrap();
        }
        assert!(store.get("clip").unwrap().is_none());
    }

    #[test]
    fn stale_in_flight_save_cannot_resurrect_a_draft_after_review_revision_advances() {
        let (_directory, store, runtime) = store();
        save(&store, "save-before-commit", 0, "before commit").unwrap();
        {
            let database = runtime.lock().unwrap();
            database
                .connection()
                .execute("UPDATE speech_segments SET review_revision = 1 WHERE id = 'clip'", [])
                .unwrap();
            database.connection().execute("DELETE FROM review_drafts WHERE segment_id = 'clip'", []).unwrap();
        }
        store.reserve_write("clip", "late-save").unwrap();
        let error = store.save("clip", 0, "late response", "late-save").expect_err("stale save must fail closed");
        assert!(error.to_string().contains("E_STALE_REVIEW_DRAFT"), "{error}");
        assert!(store.get("clip").unwrap().is_none());
    }

    #[test]
    fn durable_draft_writes_restore_normal_sync_after_success_and_failure() {
        let (_directory, store, runtime) = store();
        save(&store, "save-durable", 0, "durable").unwrap();
        {
            let database = runtime.lock().unwrap();
            let synchronous: i64 = database.connection().query_row("PRAGMA synchronous", [], |row| row.get(0)).unwrap();
            assert_eq!(synchronous, 1, "a successful draft save must restore synchronous=NORMAL");
            database
                .connection()
                .execute_batch(
                    "CREATE TRIGGER test_refuse_draft_delete BEFORE DELETE ON review_drafts
                     BEGIN SELECT RAISE(ABORT, 'injected draft delete failure'); END;",
                )
                .unwrap();
        }
        store.reserve_write("clip", "delete-trigger-failure").unwrap();
        assert!(store.delete_if_revision("clip", 0, "delete-trigger-failure").is_err());
        let database = runtime.lock().unwrap();
        let synchronous: i64 = database.connection().query_row("PRAGMA synchronous", [], |row| row.get(0)).unwrap();
        assert_eq!(synchronous, 1, "a failed draft delete must restore synchronous=NORMAL");
        let retained: i64 =
            database.connection().query_row("SELECT COUNT(*) FROM review_drafts", [], |row| row.get(0)).unwrap();
        assert_eq!(retained, 1, "the injected delete failure must preserve the draft");
    }

    #[test]
    fn newer_draft_intent_is_ordered_after_a_timed_out_native_writer_and_old_replay_is_fenced() {
        let (_directory, store, runtime) = store();
        store.reserve_write("clip", "old-save").unwrap();
        let database = runtime.lock().unwrap();

        let old_store = store.clone();
        let old_write = thread::spawn(move || old_store.save("clip", 0, "older text", "old-save"));

        let deadline = Instant::now() + Duration::from_secs(2);
        while !runtime.review_draft_write_is_active_for_test() {
            assert!(Instant::now() < deadline, "old save never acquired its native draft authority");
            thread::yield_now();
        }

        let newer_store = store.clone();
        let (reserved_tx, reserved_rx) = mpsc::channel();
        let newer_reservation = thread::spawn(move || {
            newer_store.reserve_write("clip", "new-delete").unwrap();
            reserved_tx.send(()).unwrap();
        });
        assert!(
            reserved_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "a newer reservation must not pass a mutation already at its native commit boundary"
        );

        drop(database);
        old_write.join().unwrap().unwrap();
        reserved_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        newer_reservation.join().unwrap();
        assert!(store.delete_if_revision("clip", 0, "new-delete").unwrap());
        assert!(store.get("clip").unwrap().is_none());

        let replay = store
            .save("clip", 0, "older text", "old-save")
            .expect_err("a late replay of the old native invocation must be fenced");
        assert!(replay.to_string().contains("E_STALE_REVIEW_DRAFT_WRITE"), "{replay}");
        assert!(store.get("clip").unwrap().is_none());
    }
}
