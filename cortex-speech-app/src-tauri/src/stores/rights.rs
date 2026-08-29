//! Recording-rights and consent provenance access.
//!
//! Commands validate public input and map DTOs; this store owns database authority for the domain.

use crate::database_runtime::DatabaseRuntime;
use crate::db::RecordingRights;
use crate::error::{AppError, AppResult};

#[derive(Clone)]
pub(crate) struct RightsStore {
    runtime: DatabaseRuntime,
}

impl RightsStore {
    pub(crate) fn new(runtime: DatabaseRuntime) -> Self {
        Self { runtime }
    }

    pub(crate) fn declare_recording(&self, audio_path: &str, rights: &RecordingRights) -> AppResult<usize> {
        let mutation = self.runtime.begin_mutation().map_err(AppError::Other)?;
        self.runtime
            .lock_after_mutation(&mutation)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_recording_rights(audio_path, rights)
    }

    pub(crate) fn revoke_recording(&self, audio_path: &str) -> AppResult<usize> {
        let mutation = self.runtime.begin_mutation().map_err(AppError::Other)?;
        self.runtime
            .lock_after_mutation(&mutation)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .revoke_recording(audio_path)
    }

    pub(crate) fn list_recordings(&self) -> AppResult<Vec<(String, usize, RecordingRights)>> {
        self.runtime.open_read()?.list_recording_rights()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, RightsDisposition, SpeechSegment};

    fn store_with_recordings() -> (tempfile::TempDir, RightsStore) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rights.db");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        database.initialize().unwrap();
        database
            .insert_segments_batch(&[
                SpeechSegment {
                    id: "same-1".into(),
                    audio_path: "/same.wav".into(),
                    raw_transcript: "دەق".into(),
                    duration_ms: 1_000,
                    ..SpeechSegment::default()
                },
                SpeechSegment {
                    id: "same-2".into(),
                    audio_path: "/same.wav".into(),
                    raw_transcript: "دەقی دوو".into(),
                    duration_ms: 1_000,
                    ..SpeechSegment::default()
                },
                SpeechSegment {
                    id: "other".into(),
                    audio_path: "/other.wav".into(),
                    raw_transcript: "دەقی تر".into(),
                    duration_ms: 1_000,
                    ..SpeechSegment::default()
                },
            ])
            .unwrap();
        let runtime = DatabaseRuntime::new(database);
        (directory, RightsStore::new(runtime))
    }

    fn redistributable_rights() -> RecordingRights {
        RecordingRights {
            license: Some("CC-BY-4.0".into()),
            consent_basis: Some("explicit_consent".into()),
            permitted_use: Some("train,redistribute".into()),
            attribution: Some("Speaker A".into()),
            source: Some("owner recording".into()),
            revoked_at: None,
        }
    }

    #[test]
    fn declaration_is_recording_scoped_and_visible_through_bounded_read_snapshot() {
        let (_directory, store) = store_with_recordings();
        assert_eq!(store.declare_recording("/same.wav", &redistributable_rights()).unwrap(), 2);

        let rows = store.list_recordings().unwrap();
        let same = rows.iter().find(|(path, _, _)| path == "/same.wav").unwrap();
        let other = rows.iter().find(|(path, _, _)| path == "/other.wav").unwrap();
        assert_eq!(same.1, 2);
        assert_eq!(same.2.disposition(), RightsDisposition::Redistributable);
        assert_eq!(other.1, 1);
        assert_eq!(other.2.disposition(), RightsDisposition::Unknown);
    }

    #[test]
    fn withdrawal_survives_a_later_metadata_declaration_through_the_store() {
        let (_directory, store) = store_with_recordings();
        let rights = redistributable_rights();
        store.declare_recording("/same.wav", &rights).unwrap();
        assert_eq!(store.revoke_recording("/same.wav").unwrap(), 2);
        store.declare_recording("/same.wav", &rights).unwrap();

        let rows = store.list_recordings().unwrap();
        let same = rows.iter().find(|(path, _, _)| path == "/same.wav").unwrap();
        assert_eq!(same.2.disposition(), RightsDisposition::Revoked);
    }
}
