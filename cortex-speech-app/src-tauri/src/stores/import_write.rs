//! Durable import and processing write boundaries for publication, evidence, metadata and rollback.

use crate::database_runtime::{DatabaseRuntime, MutationGuard};
use crate::db::{SegmentHypothesis, SourceAudioProvenance, SourceTranscriptRecord, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::fingerprint::AudioIdentity;
use std::sync::MutexGuard;

#[derive(Clone)]
pub(crate) struct ImportWriteStore {
    runtime: DatabaseRuntime,
}

impl ImportWriteStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    fn begin_mutation(&self) -> AppResult<MutationGuard<'_>> {
        self.runtime.begin_mutation().map_err(AppError::Other)
    }

    fn lock_after_mutation(
        &self,
        operation: &str,
        mutation: &MutationGuard<'_>,
    ) -> MutexGuard<'_, crate::db::Database> {
        self.runtime.lock_after_mutation(mutation).unwrap_or_else(|poisoned| {
            tracing::warn!(operation, "Recovering poisoned database lock during an import write");
            poisoned.into_inner()
        })
    }

    pub(crate) fn publish_segments(
        &self,
        segments: &[SpeechSegment],
        provenance: Option<&SourceAudioProvenance>,
    ) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("publish_import_segments", &mutation)
            .insert_segments_with_provenance_batch(segments, provenance)
    }

    pub(crate) fn publish_segments_with_identity(
        &self,
        segments: &[SpeechSegment],
        identity: &AudioIdentity,
        provenance: Option<&SourceAudioProvenance>,
    ) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("publish_import_segments_with_identity", &mutation)
            .insert_segments_with_audio_identity_and_provenance_batch(segments, identity, provenance)
    }

    pub(crate) fn publish_champion_segments(
        &self,
        segments: &[SpeechSegment],
        deployment_sha256: &str,
        identity: Option<&AudioIdentity>,
        provenance: Option<&SourceAudioProvenance>,
    ) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("publish_champion_import_segments", &mutation)
            .insert_champion_segments_with_provenance_batch(segments, deployment_sha256, identity, provenance)
    }

    pub(crate) fn rollback_segments(&self, segment_ids: &[String]) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("rollback_import_segments", &mutation).delete_segments_batch(segment_ids)
    }

    pub(crate) fn update_alignment_if_unchanged(
        &self,
        segment_id: &str,
        expected_revision: i64,
        expected_alignment: Option<&str>,
        alignment_json: &str,
        quality: &str,
    ) -> AppResult<bool> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("update_import_alignment", &mutation).update_segment_alignment_if_unchanged(
            segment_id,
            expected_revision,
            expected_alignment,
            alignment_json,
            quality,
        )
    }

    /// Read every freshly imported alignment source and its CAS revision from one restore-gated WAL
    /// snapshot. Pairing in-memory text with separately-read later revisions could manufacture a
    /// snapshot that never existed if a writer changed a row between those reads.
    pub(crate) fn alignment_sources(&self, segment_ids: &[String]) -> AppResult<Vec<(SpeechSegment, i64)>> {
        let database = self.runtime.open_read()?;
        let mut sources = Vec::with_capacity(segment_ids.len());
        for segment_id in segment_ids {
            let source = database.get_segment_by_id_with_revision(segment_id)?.ok_or_else(|| {
                AppError::Other(format!("imported segment {segment_id} disappeared before alignment"))
            })?;
            sources.push(source);
        }
        Ok(sources)
    }

    pub(crate) fn upsert_source_transcript(&self, record: &SourceTranscriptRecord) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("upsert_import_source_transcript", &mutation).upsert_source_transcript(record)
    }

    #[cfg(test)]
    pub(crate) fn upsert_source_audio_provenance(&self, record: &SourceAudioProvenance) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("upsert_import_source_provenance", &mutation).upsert_source_audio_provenance(record)
    }

    pub(crate) fn record_loop0_shadow(&self, segment_id: &str, memory_fired: bool) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("record_import_loop0_shadow", &mutation).record_loop0_shadow(segment_id, memory_fired)
    }

    pub(crate) fn update_machine_speaker(&self, segment_id: &str, speaker_id: &str) -> AppResult<bool> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("update_machine_speaker", &mutation).update_speaker_id(segment_id, Some(speaker_id))
    }

    pub(crate) fn insert_hypothesis(&self, hypothesis: &SegmentHypothesis) -> AppResult<()> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("insert_import_hypothesis", &mutation).insert_hypothesis(hypothesis)
    }

    #[cfg(test)]
    pub(crate) fn commit_champion_transcript_if_unreviewed(
        &self,
        champion: &SegmentHypothesis,
        expected_deployment_sha256: Option<&str>,
        normalized_transcript: Option<&str>,
        confidence_source: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        let mutation = self.begin_mutation()?;
        self.lock_after_mutation("commit_import_champion_transcript", &mutation)
            .commit_champion_transcript_if_unreviewed(
                champion,
                expected_deployment_sha256,
                normalized_transcript,
                confidence_source,
                cloud_call,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn store() -> (tempfile::TempDir, ImportWriteStore, DatabaseRuntime) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("imports.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        let runtime = DatabaseRuntime::isolated_for_test(database);
        let store = ImportWriteStore::new(runtime.clone());
        (directory, store, runtime)
    }

    fn segment(id: &str, alignment_json: Option<String>) -> SpeechSegment {
        SpeechSegment {
            id: id.into(),
            audio_path: format!("C:/recordings/{id}.wav"),
            raw_transcript: format!("draft-{id}"),
            duration_ms: 1_000,
            alignment_json,
            ..SpeechSegment::default()
        }
    }

    fn wait_for_mutation(runtime: &DatabaseRuntime) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !runtime.mutation_active_for_test() {
            assert!(std::time::Instant::now() < deadline, "writer never entered restore admission");
            std::thread::yield_now();
        }
    }

    #[test]
    fn import_write_and_restore_admission_are_linearized_before_the_writer_lock() {
        let (_directory, store, runtime) = store();

        let held_writer = runtime.lock().unwrap();
        let worker_store = store.clone();
        let worker = std::thread::spawn(move || worker_store.publish_segments(&[segment("writer-first", None)], None));
        wait_for_mutation(&runtime);
        assert!(
            runtime.try_reserve_restore_for_test().is_err(),
            "restore must refuse an admitted import writer even while it waits for SQLite"
        );
        drop(held_writer);
        worker.join().unwrap().unwrap();
        assert!(runtime.open_read().unwrap().get_segment_by_id("writer-first").unwrap().is_some());

        let restore = runtime.try_reserve_restore_for_test().unwrap();
        let error = store
            .publish_segments(&[segment("restore-first", None)], None)
            .expect_err("a published restore reservation must refuse a new import writer");
        assert!(error.to_string().contains("restore"), "unexpected admission error: {error}");
        assert!(runtime.open_read().is_err(), "ordinary reads stay fenced while restore owns admission");
        drop(restore);

        store.publish_segments(&[segment("restore-first", None)], None).unwrap();
        assert!(runtime.open_read().unwrap().get_segment_by_id("restore-first").unwrap().is_some());
    }

    #[test]
    fn import_publication_and_rollback_are_atomic_through_the_store() {
        let (_directory, store, runtime) = store();
        let mut forged = segment("forged", None);
        forged.annotated_transcript = Some("unbound human truth".into());
        assert!(store.publish_segments(&[segment("must-not-land", None), forged], None).is_err());
        assert_eq!(runtime.open_read().unwrap().segment_count().unwrap(), 0);

        let segments = [segment("one", None), segment("two", None)];
        store.publish_segments(&segments, None).unwrap();
        assert_eq!(runtime.open_read().unwrap().segment_count().unwrap(), 2);
        store.rollback_segments(&["one".into(), "two".into()]).unwrap();
        assert_eq!(runtime.open_read().unwrap().segment_count().unwrap(), 0);
    }

    #[test]
    fn background_alignment_cannot_clobber_metadata_that_changed_after_inference_started() {
        let (_directory, store, runtime) = store();
        let original = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1}"#;
        let first = r#"{"source_start_ms":0,"source_end_ms":1000,"chunk_index":0,"chunk_count":1,"words":[]}"#;
        let newer = r#"{"source_start_ms":100,"source_end_ms":900,"chunk_index":0,"chunk_count":1}"#;
        store.publish_segments(&[segment("aligned", Some(original.into()))], None).unwrap();

        let sources = store.alignment_sources(&["aligned".into()]).unwrap();
        assert_eq!(sources.len(), 1);
        let first_revision = sources[0].1;
        assert!(store
            .update_alignment_if_unchanged("aligned", first_revision, Some(original), first, "ctc_forced")
            .unwrap());
        let stale_revision = runtime.open_read().unwrap().segment_review_revision("aligned").unwrap().unwrap();
        runtime.lock().unwrap().update_segment_alignment("aligned", newer, "energy_heuristic").unwrap();
        assert!(!store
            .update_alignment_if_unchanged("aligned", stale_revision, Some(first), original, "ctc_forced")
            .unwrap());

        let retained = runtime.open_read().unwrap().get_segment_by_id("aligned").unwrap().unwrap();
        assert_eq!(retained.alignment_json.as_deref(), Some(newer));
        assert_eq!(retained.alignment_quality.as_deref(), Some("energy_heuristic"));
    }

    #[test]
    fn import_metadata_and_machine_evidence_share_the_serialized_runtime() {
        let (_directory, store, runtime) = store();
        let audio_path = "C:/recordings/evidence.wav";
        let mut speech = segment("evidence", None);
        speech.audio_path = audio_path.into();
        store
            .publish_segments_with_identity(
                &[speech],
                &AudioIdentity { spectral: 42, content: "recording-content-sha256".into() },
                None,
            )
            .unwrap();
        store
            .upsert_source_audio_provenance(&SourceAudioProvenance {
                audio_path: audio_path.into(),
                processing: "voice-separation".into(),
                separator_model: Some("separator-v1".into()),
                timeline_preserved: true,
                manifest_path: Some("C:/recordings/manifest.json".into()),
            })
            .unwrap();
        store
            .upsert_source_transcript(&SourceTranscriptRecord {
                audio_path: audio_path.into(),
                model_id: "reference-v1".into(),
                audio_content_hash: Some("recording-content-sha256".into()),
                audio_size_bytes: Some(123),
                transcript_path: "C:/recordings/reference.txt".into(),
                transcript_text: "reference transcript".into(),
                created_at: None,
            })
            .unwrap();
        store
            .insert_hypothesis(&SegmentHypothesis {
                segment_id: "evidence".into(),
                model_id: "draft-v1".into(),
                transcript: "machine draft".into(),
                confidence: Some(0.75),
            })
            .unwrap();
        store.record_loop0_shadow("evidence", true).unwrap();
        assert!(store.update_machine_speaker("evidence", "SPEAKER_07").unwrap());

        let read = runtime.open_read().unwrap();
        assert_eq!(read.load_audio_identities().unwrap().len(), 1);
        assert_eq!(read.source_audio_provenance(audio_path).unwrap().unwrap().processing, "voice-separation");
        assert_eq!(
            read.get_source_transcript(audio_path, "reference-v1").unwrap().unwrap().transcript_text,
            "reference transcript"
        );
        assert_eq!(read.get_hypotheses_for_segment("evidence").unwrap().len(), 1);
        assert_eq!(read.get_segment_by_id("evidence").unwrap().unwrap().speaker_id.as_deref(), Some("SPEAKER_07"));
        assert_eq!(read.intelligence_report().unwrap()["loop0Shadow"]["wouldFire"], 1);
    }

    #[test]
    fn champion_commit_through_store_preserves_human_owned_truth_and_prior_votes() {
        let (_directory, store, runtime) = store();
        store.publish_segments(&[segment("reviewed", None)], None).unwrap();
        let prior = SegmentHypothesis {
            segment_id: "reviewed".into(),
            model_id: "prior-v1".into(),
            transcript: "prior vote".into(),
            confidence: None,
        };
        store.insert_hypothesis(&prior).unwrap();
        runtime.lock().unwrap().update_verified_for_test("reviewed", true).unwrap();

        let champion = SegmentHypothesis {
            segment_id: "reviewed".into(),
            model_id: "champion-v1".into(),
            transcript: "must not overwrite".into(),
            confidence: Some(0.99),
        };
        assert!(!store
            .commit_champion_transcript_if_unreviewed(&champion, None, None, Some("external_provider"), false)
            .unwrap());

        let read = runtime.open_read().unwrap();
        assert_eq!(read.get_segment_by_id("reviewed").unwrap().unwrap().raw_transcript, "draft-reviewed");
        let hypotheses = read.get_hypotheses_for_segment("reviewed").unwrap();
        assert_eq!(hypotheses.len(), 1);
        assert_eq!(hypotheses[0].model_id, "prior-v1");
    }

    #[test]
    fn champion_file_publication_is_atomic_and_never_exposes_placeholders() {
        let (_directory, store, runtime) = store();
        let deployment_sha256 = "a".repeat(64);
        {
            let database = runtime.lock().unwrap();
            crate::registry::register_candidate(
                &database,
                &crate::registry::NewModelVersion {
                    id: "champion-import-v1".into(),
                    family: "omniasr-7b".into(),
                    model_card_name: Some("test/champion".into()),
                    checkpoint_sha256: deployment_sha256.clone(),
                    checkpoint_path: "C:/models/champion-import-v1.json".into(),
                    source: "cortex-finetuned".into(),
                    license: "test-only".into(),
                },
            )
            .unwrap();
            crate::registry::set_champion_for_test(&database, "champion-import-v1").unwrap();
        }

        let audio_path = "C:/recordings/champion.wav";
        let ready = ["one", "two"].map(|id| {
            let mut value = segment(id, None);
            value.audio_path = audio_path.into();
            value.raw_transcript = format!("champion draft {id}");
            value.model_version_id = Some("champion-import-v1".into());
            value.confidence_source = Some("external_provider".into());
            value
        });
        runtime
            .lock()
            .unwrap()
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_second_champion_hypothesis
                 BEFORE INSERT ON segment_hypotheses
                 WHEN NEW.segment_id = 'two'
                 BEGIN
                   SELECT RAISE(ABORT, 'injected champion hypothesis failure');
                 END;",
            )
            .unwrap();

        let identity = AudioIdentity { spectral: 42, content: "whole-recording-content".into() };
        let provenance = SourceAudioProvenance {
            audio_path: audio_path.into(),
            processing: "voice separated before import".into(),
            separator_model: Some("separator-v1".into()),
            timeline_preserved: false,
            manifest_path: Some("C:/recordings/manifest.json".into()),
        };
        assert!(store
            .publish_champion_segments(&ready, &deployment_sha256, Some(&identity), Some(&provenance))
            .is_err());
        let after_failure = runtime.open_read().unwrap();
        assert_eq!(after_failure.segment_count().unwrap(), 0);
        assert!(after_failure.load_audio_identities().unwrap().is_empty());
        assert!(after_failure.source_audio_provenance(audio_path).unwrap().is_none());
        drop(after_failure);

        runtime.lock().unwrap().connection().execute_batch("DROP TRIGGER fail_second_champion_hypothesis").unwrap();
        runtime
            .lock()
            .unwrap()
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_import_source_provenance
                 BEFORE INSERT ON source_audio_provenance
                 BEGIN
                   SELECT RAISE(ABORT, 'injected source provenance failure');
                 END;",
            )
            .unwrap();
        assert!(
            store.publish_champion_segments(&ready, &deployment_sha256, Some(&identity), Some(&provenance)).is_err(),
            "a provenance failure must roll back rows, champion hypotheses and recording identity"
        );
        let after_provenance_failure = runtime.open_read().unwrap();
        assert_eq!(after_provenance_failure.segment_count().unwrap(), 0);
        assert!(after_provenance_failure.load_audio_identities().unwrap().is_empty());
        assert!(after_provenance_failure.source_audio_provenance(audio_path).unwrap().is_none());
        drop(after_provenance_failure);

        runtime.lock().unwrap().connection().execute_batch("DROP TRIGGER fail_import_source_provenance").unwrap();
        store.publish_champion_segments(&ready, &deployment_sha256, Some(&identity), Some(&provenance)).unwrap();
        let published = runtime.open_read().unwrap();
        assert_eq!(published.segment_count().unwrap(), 2);
        assert_eq!(published.load_audio_identities().unwrap().len(), 1);
        assert_eq!(published.source_audio_provenance(audio_path).unwrap(), Some(provenance));
        for segment in &ready {
            let stored = published.get_segment_by_id(&segment.id).unwrap().unwrap();
            assert!(!crate::quality::is_placeholder_transcript(&stored.raw_transcript));
            let hypotheses = published.get_hypotheses_for_segment(&segment.id).unwrap();
            assert_eq!(hypotheses.len(), 1);
            assert_eq!(hypotheses[0].model_id, "champion-import-v1");
            assert_eq!(hypotheses[0].transcript, segment.raw_transcript);
        }
        drop(published);

        let mut placeholder = segment("placeholder", None);
        placeholder.audio_path = audio_path.into();
        placeholder.raw_transcript = "[Pending WSL 7B ASR]".into();
        placeholder.model_version_id = Some("champion-import-v1".into());
        assert!(store.publish_champion_segments(&[placeholder], &deployment_sha256, None, None).is_err());
        assert_eq!(runtime.open_read().unwrap().segment_count().unwrap(), 2);
    }
}
