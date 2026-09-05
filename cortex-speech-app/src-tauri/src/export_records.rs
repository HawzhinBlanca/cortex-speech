//! Export-only dataset records, privacy-safe references and measured speaker-turn labels.
//!
//! These serializers are deliberately separate from renderer/import DTOs and preserve the exact
//! final transcript authority without granting deserialization a way to manufacture that authority.

use crate::db::SpeechSegment;
use crate::quality;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExportSegmentRecord {
    #[serde(flatten)]
    segment: SpeechSegment,
    /// Final consensus belongs to the dataset record, never to a renderer/import DTO.
    #[serde(skip_serializing_if = "Option::is_none")]
    export_review: Option<crate::export_review::ExportReviewAuthority>,
    training_transcript: String,
    transcript_source: String,
    training_grade: String,
    training_ready: bool,
    training_reasons: Vec<String>,
    /// Dialect of the source recording (owner instruction 2026-08-16), from the explicit map in
    /// `dialect.rs` — e.g. every KBHP episode is Hawleri. `None` for an unmapped source: an absent
    /// label is honest, a guessed one silently poisons every per-dialect split and fairness number
    /// computed from the dataset. Derived from the ORIGINAL path (the map keys on the source), then
    /// the path itself is sanitized to a basename below.
    dialect: Option<&'static str>,
    /// Was this clip MEASURED to span a speaker turn? (CAM++ half-vs-half, the couch badge's exact
    /// predicate.) `Some(true)` = two voices — its `speaker_id` label is unreliable and a downstream
    /// consumer building speaker-attributed data should filter it. `None` = NOT MEASURED, which must
    /// never be exported as "single speaker": absence of a measurement is not evidence of one voice.
    speaker_turn: Option<bool>,
}

impl ExportSegmentRecord {
    pub(super) fn new(segment: &SpeechSegment) -> Self {
        let report = quality::training_grade_for_segment(segment);
        let dialect = crate::dialect::dialect_of(&segment.audio_path);
        let speaker_turn =
            segment.speaker_change_score.map(|s| (s as f32) < crate::diarization::SPEAKER_CHANGE_THRESHOLD);
        // Privacy: never publish the curator's absolute filesystem path — it embeds the
        // OS username and drive layout. Emit only the basename, like the HF exporter. Reviewer
        // identity is operational/private attribution kept in the database; public/shared dataset
        // rows need the decision provenance, not the worker's name.
        let mut sanitized = segment.clone();
        sanitized.audio_path = export_audio_ref(&segment.audio_path).to_string();
        sanitized.reviewed_by = None;
        Self {
            segment: sanitized,
            export_review: segment.export_review.clone(),
            // Verbatim Law: the primary training label is the exact stored authority selected by the
            // grade (human verdict > annotation > champion raw). Orthographic normalization is useful
            // as explicitly labeled derived evidence and as a dedup key, never as a replacement label
            // that still claims the original transcript_source.
            training_transcript: report.transcript,
            transcript_source: report.transcript_source,
            training_grade: report.grade,
            training_ready: report.training_ready,
            training_reasons: report.reasons,
            dialect,
            speaker_turn,
        }
    }
}

/// The CSV/metadata spelling of the tri-state: "true" / "false" / "" (unmeasured). One function so
/// the plain CSV, the HF metadata.csv and any future flat exporter cannot drift apart on it.
pub(super) fn speaker_turn_csv(segment: &SpeechSegment) -> &'static str {
    match segment.speaker_change_score.map(|s| (s as f32) < crate::diarization::SPEAKER_CHANGE_THRESHOLD) {
        Some(true) => "true",
        Some(false) => "false",
        None => "",
    }
}

/// The published reference for an audio file: just its basename, never the curator's
/// absolute path (which leaks the OS username and directory layout into a shared dataset).
/// `pub(crate)` so the audio-clip exporter (export_audio) reduces its metadata.csv source column
/// the same way the JSON/JSONL/CSV/Parquet/HF exporters here do.
pub(crate) fn export_audio_ref(audio_path: &str) -> &str {
    audio_path.rsplit(['/', '\\']).next().unwrap_or(audio_path)
}

pub(super) fn export_records(segments: &[SpeechSegment]) -> Vec<ExportSegmentRecord> {
    segments.iter().map(ExportSegmentRecord::new).collect()
}
