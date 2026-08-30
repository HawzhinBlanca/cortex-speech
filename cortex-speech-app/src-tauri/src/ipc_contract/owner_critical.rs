//! Typed public contracts for the owner's import, transcription, alignment, waveform and export
//! loop. All diagnostic strings stay native; only closed, bounded messages cross into the webview.

use super::{CommandErrorV1, SuggestedActionV1};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ImportStartStatusV1 {
    Started,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ImportSourceV1 {
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryImportStartedV1 {
    pub status: ImportStartStatusV1,
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileImportStartedV1 {
    pub status: ImportStartStatusV1,
    pub source: ImportSourceV1,
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscribedSegmentV1 {
    pub text: String,
    pub raw_transcript: String,
    pub confidence: Option<f64>,
    pub confidence_source: Option<String>,
    pub model_version_id: Option<String>,
    pub cloud_call: bool,
    pub segment_id: String,
}

impl TranscribedSegmentV1 {
    pub(crate) fn from_committed_segment(segment: &crate::db::SpeechSegment, text: String) -> Self {
        Self {
            text,
            raw_transcript: segment.raw_transcript.clone(),
            confidence: segment.confidence,
            confidence_source: segment.confidence_source.clone(),
            model_version_id: segment.model_version_id.clone(),
            cloud_call: segment.cloud_call,
            segment_id: segment.id.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
pub struct WordTimestampV1 {
    pub word: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f64,
}

impl From<crate::aligner::WordTimestamp> for WordTimestampV1 {
    fn from(value: crate::aligner::WordTimestamp) -> Self {
        Self { word: value.word, start: value.start, end: value.end, confidence: value.confidence }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConsensusWordV1 {
    pub text: String,
    pub agreement: f64,
    pub models_agreeing: usize,
    pub total_models: usize,
    pub alternatives: Vec<String>,
}

impl From<crate::quality::irt::ConsensusWord> for ConsensusWordV1 {
    fn from(value: crate::quality::irt::ConsensusWord) -> Self {
        Self {
            text: value.text,
            agreement: value.agreement,
            models_agreeing: value.models_agreeing,
            total_models: value.total_models,
            alternatives: value.alternatives,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SegmentConsensusV1 {
    pub draft: String,
    pub words: Vec<ConsensusWordV1>,
    pub model_count: usize,
    pub min_agreement: f64,
    pub mean_agreement: f64,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct MergeDatasetResultV1 {
    pub created: usize,
    pub updated: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
pub enum AudioExportFormatV1 {
    Wav,
    Flac,
}

impl From<AudioExportFormatV1> for crate::export_audio::AudioExportFormat {
    fn from(value: AudioExportFormatV1) -> Self {
        match value {
            AudioExportFormatV1::Wav => Self::Wav,
            AudioExportFormatV1::Flac => Self::Flac,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct AudioExportOptionsV1 {
    pub output_dir: String,
    pub format: AudioExportFormatV1,
    pub sample_rate: u32,
    pub include_metadata: bool,
}

impl From<AudioExportOptionsV1> for crate::export_audio::AudioExportOptions {
    fn from(value: AudioExportOptionsV1) -> Self {
        Self {
            output_dir: value.output_dir,
            format: value.format.into(),
            sample_rate: value.sample_rate,
            include_metadata: value.include_metadata,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
pub struct AudioExportResultV1 {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub output_dir: String,
    pub files: Vec<String>,
    pub errors: Vec<String>,
}

impl From<crate::export_audio::AudioExportResult> for AudioExportResultV1 {
    fn from(value: crate::export_audio::AudioExportResult) -> Self {
        Self {
            total: value.total,
            succeeded: value.succeeded,
            failed: value.failed,
            output_dir: value.output_dir,
            files: value.files,
            // The native exporter keeps exact path-bearing diagnostics in its logs. The renderer only
            // needs the stable fact that one requested item failed; its count and result totals remain
            // exact without disclosing source paths or decoder internals.
            errors: value.errors.into_iter().map(|_| "AUDIO_EXPORT_ITEM_FAILED".to_string()).collect(),
        }
    }
}

fn retryable(code: &str, message: &str) -> CommandErrorV1 {
    CommandErrorV1::new(code, message, true).suggested(SuggestedActionV1::Retry)
}

fn health(code: &str, message: &str) -> CommandErrorV1 {
    CommandErrorV1::new(code, message, false).suggested(SuggestedActionV1::OpenHealth)
}

pub(crate) fn owner_critical_rate_limited(operation: &str) -> CommandErrorV1 {
    let message = match operation {
        "open_audio_file" => "The audio picker is busy. Wait a moment, then retry.",
        "import_directory" | "import_audio_file" => "Audio import is busy. Wait a moment, then retry.",
        "transcribe_segment" => "Transcription is busy. Wait a moment, then retry.",
        "align_segment" => "Alignment is busy. Wait a moment, then retry.",
        "get_segment_consensus" => "Consensus is busy. Wait a moment, then retry.",
        "get_waveform" => "Waveform loading is busy. Wait a moment, then retry.",
        "get_audio_health" => "Audio health inspection is busy. Wait a moment, then retry.",
        "relink_audio" => "Audio relinking is busy. Wait a moment, then retry.",
        "validate_dataset_cmd" => "Dataset validation is busy. Wait a moment, then retry.",
        "merge_dataset_json" => "Dataset merge is busy. Wait a moment, then retry.",
        "create_gold_from_file" | "import_verified_segments_as_gold" => {
            "Gold-set maintenance is busy. Wait a moment, then retry."
        }
        "export_dataset"
        | "export_transcript"
        | "export_huggingface_dataset"
        | "export_audio"
        | "export_gold_eval_set"
        | "export_finetune_pack" => "Export is busy. Wait a moment, then retry.",
        _ => "This operation is busy. Wait a moment, then retry.",
    };
    retryable("RATE_LIMITED", message)
}

pub(crate) fn invalid_audio_path_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_AUDIO_PATH", "The selected local audio path is invalid or unavailable.", false)
}

pub(crate) fn invalid_import_source_path_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_IMPORT_SOURCE", "The selected local import folder is invalid or unavailable.", false)
}

pub(crate) fn invalid_output_path_error() -> CommandErrorV1 {
    CommandErrorV1::new(
        "INVALID_OUTPUT_PATH",
        "The selected local export destination is invalid or unavailable.",
        false,
    )
}

pub(crate) fn invalid_relink_path_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_RELINK_FOLDER", "The selected local relink folder is invalid or unavailable.", false)
}

pub(crate) fn invalid_dataset_payload_error() -> CommandErrorV1 {
    CommandErrorV1::new(
        "INVALID_DATASET_PAYLOAD",
        "The dataset payload is empty, malformed, or exceeds the supported size.",
        false,
    )
}

pub(crate) fn invalid_gold_audio_path_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_GOLD_AUDIO_PATH", "The selected gold-set audio path is invalid.", false)
}

pub(crate) fn invalid_alignment_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_ALIGNMENT", "The clip timing metadata is invalid; reload the clip.", false)
        .suggested(SuggestedActionV1::ReloadClip)
}

pub(crate) fn invalid_segment_id_error() -> CommandErrorV1 {
    CommandErrorV1::new("INVALID_SEGMENT_ID", "The clip identity is invalid.", false)
}

pub(crate) fn invalid_alignment_text_error() -> CommandErrorV1 {
    CommandErrorV1::new(
        "INVALID_ALIGNMENT_TEXT",
        "Alignment requires non-empty transcript text within the size limit.",
        false,
    )
}

pub(crate) fn public_file_picker_error(private_detail: &str) -> CommandErrorV1 {
    match private_detail {
        "E_FILE_PICKER_BUSY" => retryable("E_FILE_PICKER_BUSY", "Another audio picker is already open."),
        "E_FILE_PICKER_TIMEOUT" => retryable(
            "E_FILE_PICKER_TIMEOUT",
            "The audio picker did not respond in time. Close any open picker and retry.",
        ),
        "E_FILE_PICKER_CLOSED" => retryable(
            "E_FILE_PICKER_CLOSED",
            "The audio picker closed without returning a result. Retry the selection.",
        ),
        "E_FILE_PICKER_CANCELLED" => {
            CommandErrorV1::new("E_FILE_PICKER_CANCELLED", "The audio selection was cancelled.", false)
        }
        _ => health("FILE_PICKER_FAILED", "The audio picker could not be opened. Open Health before retrying."),
    }
}

pub(crate) fn public_directory_picker_error(private_detail: &str) -> CommandErrorV1 {
    match private_detail {
        "E_DIRECTORY_PICKER_CANCELLED" => {
            CommandErrorV1::new("E_DIRECTORY_PICKER_CANCELLED", "The folder selection was cancelled.", false)
        }
        "E_DIRECTORY_PICKER_TIMEOUT" => retryable(
            "E_DIRECTORY_PICKER_TIMEOUT",
            "The folder picker did not respond in time. Close any open picker and retry.",
        ),
        "E_DIRECTORY_PICKER_CLOSED" => retryable(
            "E_DIRECTORY_PICKER_CLOSED",
            "The folder picker closed without returning a result. Retry the selection.",
        ),
        _ => health("DIRECTORY_PICKER_FAILED", "The folder picker failed. Open Health before retrying."),
    }
}

pub(crate) fn public_import_start_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Import cannot start while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    if private_detail == "Import already in progress" {
        return retryable(
            "IMPORT_IN_PROGRESS",
            "Another import is already running. Wait for it to finish or cancel it, then retry.",
        );
    }
    if private_detail.contains(crate::DEDUP_INDEX_UNAVAILABLE_CODE) {
        return health(
            crate::DEDUP_INDEX_UNAVAILABLE_CODE,
            "Audio import is disabled because duplicate protection could not be verified. Open Health before importing.",
        );
    }
    if private_detail == crate::INTERRUPTED_IMPORT_RECOVERY_REQUIRED_MESSAGE {
        return CommandErrorV1::new(
            "IMPORT_RECOVERY_REQUIRED",
            "Recover or discard the interrupted import before starting another import.",
            false,
        );
    }
    if private_detail == crate::IMPORT_RECOVERY_AUTHORITY_UNAVAILABLE_MESSAGE {
        return health(
            "IMPORT_RECOVERY_UNAVAILABLE",
            "Import is disabled because interrupted-import state could not be verified. Open Health before importing.",
        );
    }
    if private_detail == "Import run identity already used" {
        return CommandErrorV1::new(
            "IMPORT_RUN_ID_REUSED",
            "This import run identity has already been used. Start a new import operation.",
            false,
        );
    }
    retryable("IMPORT_START_FAILED", "The import could not be started. No import worker was accepted; retry.")
}

pub(crate) fn import_worker_start_error() -> CommandErrorV1 {
    retryable("IMPORT_WORKER_START_FAILED", "Cortex could not start the import worker. Retry the import.")
}

pub(crate) fn public_transcription_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::pipeline::ASR_7B_UNAVAILABLE_TAG) {
        return CommandErrorV1::new(
            crate::pipeline::ASR_7B_UNAVAILABLE_TAG,
            "E_ASR_7B_UNAVAILABLE: The pinned OmniASR-7B champion is unavailable. The existing transcript is unchanged.",
            true,
        )
        .suggested(SuggestedActionV1::OpenModels);
    }
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Transcription cannot start while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    if private_detail.contains("E_TRANSCRIPTION_SOURCE_UNBOUND") {
        return CommandErrorV1::new(
            "TRANSCRIPTION_SOURCE_UNBOUND",
            "Transcription requires an imported clip identity; reload the clip.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("no longer exists") {
        return CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists; reload the library.", false)
            .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("audio path changed") {
        return CommandErrorV1::new(
            "TRANSCRIPTION_SOURCE_CHANGED",
            "The clip source changed before transcription. Reload the clip; no transcript was written.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("produced no text") {
        return CommandErrorV1::new(
            "TRANSCRIPTION_EMPTY",
            "The champion produced no speech text. The existing transcript is unchanged.",
            false,
        );
    }
    if private_detail.contains("Automatic alignment failed") {
        return retryable(
            "TRANSCRIPTION_ALIGNMENT_FAILED",
            "Automatic alignment failed before commit. The existing transcript and timings are unchanged.",
        );
    }
    if private_detail.contains("gained a human decision") {
        return CommandErrorV1::new(
            "REVIEW_CONFLICT",
            "This clip was reviewed while transcription was running. Human truth was preserved; reload the clip.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("exact Undo authority could not be recorded") {
        return health(
            "TRANSCRIPTION_HISTORY_FAILED",
            "The transcript committed, but exact Undo authority could not be recorded. Stop editing and open Health.",
        );
    }
    if private_detail.contains("Failed to commit champion transcript") {
        return retryable(
            "TRANSCRIPTION_COMMIT_FAILED",
            "The champion transcript could not be committed. Existing transcript truth is unchanged; retry.",
        );
    }
    if private_detail.contains("background task failed") {
        return health(
            "TRANSCRIPTION_WORKER_FAILED",
            "The transcription worker stopped unexpectedly. Existing transcript truth is unchanged; open Health.",
        );
    }
    retryable(
        "TRANSCRIPTION_FAILED",
        "Champion transcription failed before a verified result was returned. Existing transcript truth is unchanged.",
    )
}

pub(crate) fn public_alignment_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Alignment cannot start while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    if private_detail.contains("no longer exists") {
        return CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists; reload the library.", false)
            .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("has no authoritative transcript to align") {
        return CommandErrorV1::new(
            "ALIGNMENT_TRANSCRIPT_REQUIRED",
            "This clip has no authoritative transcript to align. Transcribe or correct it first.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("audio path changed")
        || private_detail.contains("transcript changed")
        || private_detail.contains("changed while alignment was running")
    {
        return CommandErrorV1::new(
            "ALIGNMENT_SOURCE_CHANGED",
            "The clip changed while alignment was running. Nothing was overwritten; reload the clip.",
            false,
        )
        .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.contains("Failed to persist word timings") {
        return retryable(
            "ALIGNMENT_SAVE_FAILED",
            "Word timings could not be saved. Existing clip timing truth is unchanged; retry.",
        );
    }
    if private_detail.contains("background task failed") {
        return health(
            "ALIGNMENT_WORKER_FAILED",
            "The alignment worker stopped unexpectedly. Open Health before retrying.",
        );
    }
    retryable("ALIGNMENT_FAILED", "The clip could not be aligned. Existing clip timing truth is unchanged.")
}

pub(crate) fn public_consensus_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains("no longer exists") {
        return CommandErrorV1::new("SEGMENT_NOT_FOUND", "This clip no longer exists; reload the library.", false)
            .suggested(SuggestedActionV1::ReloadClip);
    }
    if private_detail.to_ascii_lowercase().contains("locked") || private_detail.to_ascii_lowercase().contains("busy") {
        return retryable("DATABASE_BUSY", "The library is busy. Retry loading consensus in a moment.");
    }
    health("CONSENSUS_READ_FAILED", "Consensus evidence could not be read. Open Health before relying on it.")
}

pub(crate) fn public_waveform_error(private_detail: &str) -> CommandErrorV1 {
    if private_detail.to_ascii_lowercase().contains("timed out") {
        return retryable("WAVEFORM_TIMEOUT", "Audio decoding timed out. Retry or verify the local audio file.");
    }
    if private_detail.contains("background task failed") {
        return health(
            "WAVEFORM_WORKER_FAILED",
            "The waveform worker stopped unexpectedly. Open Health before retrying.",
        );
    }
    retryable("WAVEFORM_FAILED", "The local waveform could not be loaded. Verify the audio file and retry.")
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum OwnerDataOperationV1 {
    AudioHealth,
    RelinkAudio,
    ValidateDataset,
    MergeDataset,
    CreateGold,
    ImportVerifiedGold,
}

pub(crate) fn public_owner_data_error(operation: OwnerDataOperationV1, private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "This operation cannot run while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    let normalized = private_detail.to_ascii_lowercase();
    if normalized.contains("database is locked") || normalized.contains("database is busy") {
        return retryable("DATABASE_BUSY", "The library is busy. Wait a moment, then retry.");
    }
    if private_detail.contains("background task failed") {
        return health(
            "OWNER_DATA_WORKER_FAILED",
            "The background worker stopped unexpectedly. Open Health before retrying.",
        );
    }
    match operation {
        OwnerDataOperationV1::AudioHealth => health(
            "AUDIO_HEALTH_FAILED",
            "Audio health could not be inspected. Open Health before relying on source availability.",
        ),
        OwnerDataOperationV1::RelinkAudio => retryable(
            "AUDIO_RELINK_FAILED",
            "Audio relinking did not complete. Existing source links were not reported as complete; retry.",
        ),
        OwnerDataOperationV1::ValidateDataset => health(
            "DATASET_VALIDATION_FAILED",
            "Dataset validation could not produce a complete report. Open Health before exporting.",
        ),
        OwnerDataOperationV1::MergeDataset => CommandErrorV1::new(
            "DATASET_MERGE_FAILED",
            "Dataset merge did not return a verified completion. Open Health before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth),
        OwnerDataOperationV1::CreateGold => CommandErrorV1::new(
            "GOLD_CREATE_FAILED",
            "The source was not promoted to the gold set. Verify review and source authority before retrying.",
            false,
        ),
        OwnerDataOperationV1::ImportVerifiedGold => CommandErrorV1::new(
            "GOLD_IMPORT_FAILED",
            "Verified sources could not be promoted to the gold set. Open Health before retrying.",
            false,
        )
        .suggested(SuggestedActionV1::OpenHealth),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExportOperationV1 {
    Dataset,
    Transcript,
    HuggingFace,
    Audio,
    GoldEval,
    Finetune,
}

pub(crate) fn public_export_error(operation: ExportOperationV1, private_detail: &str) -> CommandErrorV1 {
    if private_detail.contains(crate::database_runtime::RESTORE_IN_PROGRESS_MSG) {
        return retryable(
            "RESTORE_IN_PROGRESS",
            "Export cannot start while database recovery is in progress. Wait for it to finish, then retry.",
        );
    }
    if private_detail.contains("blocked: campaign") {
        return CommandErrorV1::new(
            "EXPORT_REVIEW_BLOCKED",
            "Export is blocked until the active independent-review campaign is completed.",
            false,
        );
    }
    if matches!(operation, ExportOperationV1::Transcript)
        && private_detail.contains("Subtitles (SRT/VTT) are per-source")
    {
        return CommandErrorV1::new(
            "TRANSCRIPT_MULTIPLE_SOURCES",
            "SRT/VTT requires one source timeline. Export TXT for a multi-source library.",
            false,
        );
    }
    if private_detail.contains("background task failed") {
        return health("EXPORT_WORKER_FAILED", "The export worker stopped unexpectedly. Open Health before retrying.");
    }
    match operation {
        ExportOperationV1::Dataset => retryable(
            "DATASET_EXPORT_FAILED",
            "The dataset export failed and no completed artifact was published. Verify the destination and retry.",
        ),
        ExportOperationV1::Transcript => retryable(
            "TRANSCRIPT_EXPORT_FAILED",
            "The transcript export failed and no completed artifact was published. Verify the destination and retry.",
        ),
        ExportOperationV1::HuggingFace => retryable(
            "HUGGINGFACE_EXPORT_FAILED",
            "The Hugging Face export failed and no completed artifact was published. Verify the destination and retry.",
        ),
        ExportOperationV1::Audio => retryable(
            "AUDIO_EXPORT_FAILED",
            "The reviewed-audio export failed and no completed generation was published. Verify the destination and retry.",
        ),
        ExportOperationV1::GoldEval => retryable(
            "GOLD_EXPORT_FAILED",
            "The gold evaluation export failed and no completed artifact was published. Verify source authority and retry.",
        ),
        ExportOperationV1::Finetune => retryable(
            "FINETUNE_EXPORT_FAILED",
            "The fine-tune export failed and no completed artifact was published. Verify source authority and retry.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_scrubbed(error: &CommandErrorV1) {
        let wire = serde_json::to_string(error).expect("serialize public error");
        for private in ["Z:\\private-vault\\secret.wav", "SELECT * FROM speech_segments", "token=owner-secret"] {
            assert!(!wire.contains(private), "private diagnostic escaped public wire: {wire}");
        }
        assert!(error.message.chars().count() <= 180, "public message must stay bounded");
    }

    #[test]
    fn champion_unavailable_keeps_exact_public_sentinel_and_scrubs_private_detail() {
        let error = public_transcription_error(&format!(
            "{}: Z:\\private-vault\\secret.wav token=owner-secret",
            crate::pipeline::ASR_7B_UNAVAILABLE_TAG
        ));
        assert_eq!(error.code, crate::pipeline::ASR_7B_UNAVAILABLE_TAG);
        assert!(error.message.contains(crate::pipeline::ASR_7B_UNAVAILABLE_TAG));
        assert_eq!(error.suggested_action, Some(SuggestedActionV1::OpenModels));
        assert_scrubbed(&error);
    }

    #[test]
    fn every_owner_critical_fallback_is_bounded_and_scrubbed() {
        let private = "Z:\\private-vault\\secret.wav SELECT * FROM speech_segments token=owner-secret";
        let errors = [
            public_file_picker_error(private),
            public_directory_picker_error(private),
            public_import_start_error(private),
            public_transcription_error(private),
            public_alignment_error(private),
            public_consensus_error(private),
            public_waveform_error(private),
            public_owner_data_error(OwnerDataOperationV1::AudioHealth, private),
            public_owner_data_error(OwnerDataOperationV1::RelinkAudio, private),
            public_owner_data_error(OwnerDataOperationV1::ValidateDataset, private),
            public_owner_data_error(OwnerDataOperationV1::MergeDataset, private),
            public_owner_data_error(OwnerDataOperationV1::CreateGold, private),
            public_owner_data_error(OwnerDataOperationV1::ImportVerifiedGold, private),
            public_export_error(ExportOperationV1::Dataset, private),
            public_export_error(ExportOperationV1::Transcript, private),
            public_export_error(ExportOperationV1::HuggingFace, private),
            public_export_error(ExportOperationV1::Audio, private),
            public_export_error(ExportOperationV1::GoldEval, private),
            public_export_error(ExportOperationV1::Finetune, private),
        ];
        for error in &errors {
            assert_scrubbed(error);
        }
    }

    #[test]
    fn dto_wire_shapes_match_the_existing_owner_loop() {
        let directory = serde_json::to_value(DirectoryImportStartedV1 {
            status: ImportStartStatusV1::Started,
            run_id: "run-1".into(),
        })
        .unwrap();
        assert_eq!(directory, serde_json::json!({ "status": "started", "runId": "run-1" }));

        let file = serde_json::to_value(FileImportStartedV1 {
            status: ImportStartStatusV1::Started,
            source: ImportSourceV1::File,
            run_id: "run-1".into(),
        })
        .unwrap();
        assert_eq!(file, serde_json::json!({ "status": "started", "source": "file", "runId": "run-1" }));

        let merge = serde_json::to_value(MergeDatasetResultV1 { created: 2, updated: 3 }).unwrap();
        assert_eq!(merge, serde_json::json!({ "created": 2, "updated": 3 }));

        let audio = AudioExportResultV1::from(crate::export_audio::AudioExportResult {
            total: 2,
            succeeded: 1,
            failed: 1,
            output_dir: "D:/owner-export".into(),
            files: vec!["segment-1.wav".into()],
            // A private-looking path plus a secret, WITHOUT a real Windows profile prefix: the
            // repo hygiene gate (test_windows_repo_hygiene.py) refuses profile-directory literals
            // in tracked sources, and this proof only needs SOME sensitive content to vanish —
            // the wire type must redact whatever the error carried, not this string in particular.
            errors: vec!["segment-2: Z:\\private-vault\\secret.wav token=owner-secret".into()],
        });
        let audio_wire = serde_json::to_value(audio).unwrap();
        assert_eq!(audio_wire["output_dir"], "D:/owner-export");
        assert_eq!(audio_wire["errors"], serde_json::json!(["AUDIO_EXPORT_ITEM_FAILED"]));
        assert!(!audio_wire.to_string().contains("secret.wav"));
    }
}
