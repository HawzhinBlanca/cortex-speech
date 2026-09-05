use crate::atomic_file::{remove_file_on_error, replace_file};
use crate::audio;
use crate::chunking;
use crate::db::{Database, SourceTranscriptRecord, SpeechSegment};
use crate::error::{AppError, AppResult};
use crate::quality::{self, TrainingGradeReport, TrainingGradeSummary};
use crate::settings::ExportFormat;
use arrow_array::{BooleanArray, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Arc;

#[path = "hf_publication.rs"]
mod hf_publication;

#[derive(serde::Serialize)]
pub struct DatasetMetadata {
    pub name: String,
    pub version: String,
    pub language: String,
    pub script: String,
    pub total_segments: usize,
    pub total_duration_ms: i64,
    pub verified_segments: usize,
    pub training_grade_summary: TrainingGradeSummary,
    /// Per-speaker composition so a skewed corpus (one voice dominating) is visible in the dataset card
    /// itself rather than only in-app — a real fine-tune quality lever for a small single-curator corpus.
    pub composition: DatasetComposition,
    /// Recordings in this export whose audio was PROCESSED before import — separated from music,
    /// cut, re-concatenated, level-normalised. Empty means no recording in this export carries such a
    /// claim, which is NOT the same as "every recording is verified original": a source imported
    /// before v54 existed, or processed by a tool that left no manifest, makes no claim either way.
    /// The wording says what is known, never more.
    pub processed_audio: Vec<ProcessedAudioNotice>,
    pub exported_at: String,
}

/// One recording's declaration that its audio is not the original, and how many clips it contributed.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ProcessedAudioNotice {
    pub audio_path: String,
    pub segments: usize,
    pub processing: String,
    pub separator_model: Option<String>,
    /// False when non-speech was cut out, so a clip's source offsets do NOT map back to the original
    /// recording. Anyone re-cutting from the source needs this before they trust a timestamp.
    pub timeline_preserved: bool,
    pub manifest_path: Option<String>,
}

/// Collect the processing declarations covering the recordings actually present in this export.
///
/// One query for the whole table, then a lookup per segment: a 550 h cleaned corpus is a few
/// thousand recordings and a quarter-million clips, so per-clip queries would dominate the export.
pub(crate) fn processed_audio_notices(
    db: &Database,
    segments: &[SpeechSegment],
) -> AppResult<Vec<ProcessedAudioNotice>> {
    let declared = db.source_audio_provenance_map()?;
    if declared.is_empty() {
        return Ok(Vec::new());
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for segment in segments {
        if declared.contains_key(&segment.audio_path) {
            *counts.entry(segment.audio_path.as_str()).or_default() += 1;
        }
    }
    let mut notices: Vec<ProcessedAudioNotice> = counts
        .into_iter()
        .filter_map(|(path, segments)| {
            let record = declared.get(path)?;
            Some(ProcessedAudioNotice {
                // Shared/public metadata never carries the curator's absolute filesystem layout.
                audio_path: export_audio_ref(&record.audio_path).to_string(),
                segments,
                processing: record.processing.clone(),
                separator_model: record.separator_model.clone(),
                timeline_preserved: record.timeline_preserved,
                manifest_path: record.manifest_path.as_deref().map(export_audio_ref).map(str::to_string),
            })
        })
        .collect();
    // Stable order so two exports of the same library produce the same file.
    notices.sort_by(|a, b| a.audio_path.cmp(&b.audio_path));
    Ok(notices)
}

#[derive(serde::Serialize)]
pub struct SpeakerComposition {
    pub speaker_id: String,
    pub segments: usize,
    pub duration_ms: i64,
}

#[derive(serde::Serialize)]
pub struct DatasetComposition {
    pub speakers: Vec<SpeakerComposition>,
    /// The largest single speaker's share of total duration (0..1).
    pub dominant_speaker_share: f64,
    /// True when one speaker exceeds 50% of the corpus by duration — flag it for the curator.
    pub dominant_speaker_over_50pct: bool,
}

/// Aggregate per-speaker segment/duration counts and the dominant speaker's share of total duration.
pub(crate) fn compute_composition(segments: &[SpeechSegment]) -> DatasetComposition {
    use std::collections::HashMap;
    let mut by_speaker: HashMap<String, (usize, i64)> = HashMap::new();
    let mut total: i64 = 0;
    for s in segments {
        let spk = s.speaker_id.clone().unwrap_or_else(|| "unknown".to_string());
        let entry = by_speaker.entry(spk).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += s.duration_ms;
        total += s.duration_ms;
    }
    let mut speakers: Vec<SpeakerComposition> = by_speaker
        .into_iter()
        .map(|(speaker_id, (segments, duration_ms))| SpeakerComposition { speaker_id, segments, duration_ms })
        .collect();
    speakers.sort_by(|a, b| b.duration_ms.cmp(&a.duration_ms).then_with(|| a.speaker_id.cmp(&b.speaker_id)));
    let dominant_speaker_share =
        if total > 0 { speakers.first().map(|s| s.duration_ms as f64 / total as f64).unwrap_or(0.0) } else { 0.0 };
    DatasetComposition { dominant_speaker_over_50pct: dominant_speaker_share > 0.5, dominant_speaker_share, speakers }
}

/// Markdown rendering of the composition for the HUMAN-READABLE dataset cards (HF README.md and the
/// bundle's dataset_card.md). The skew warning is a stated fine-tune quality lever, but it previously
/// reached only dataset.json — the two artifacts actually called dataset cards omitted it, so a
/// consumer of the HF directory never saw the imbalance (true-10 audit 2026-07-09).
pub(crate) fn composition_markdown(comp: &DatasetComposition) -> String {
    let mut md = String::from("\n## Speaker Composition\n| Speaker | Segments | Duration (s) |\n|---|---|---|\n");
    for s in &comp.speakers {
        let speaker = s.speaker_id.replace('|', "\\|");
        md.push_str(&format!("| {} | {} | {:.1} |\n", speaker, s.segments, s.duration_ms as f64 / 1000.0));
    }
    md.push_str(&format!(
        "\nDominant speaker share of total duration: {:.1}%{}\n",
        comp.dominant_speaker_share * 100.0,
        if comp.dominant_speaker_over_50pct {
            " — WARNING: one speaker exceeds 50% of the corpus; consider balancing before fine-tuning."
        } else {
            ""
        }
    ));
    md
}

/// The processed-audio declaration, for the HUMAN-readable dataset card.
///
/// The same lesson as `composition_markdown` right above: a fact that lives only in the JSON is a
/// fact the person reading the card never sees, and this one is the difference between "recordings"
/// and "recordings a neural model rebuilt". Returns an empty string when no recording in the export
/// carries a claim — silence is correct there, because an absent record means unclaimed, not
/// verified-original.
pub(crate) fn processed_audio_markdown(notices: &[ProcessedAudioNotice]) -> String {
    if notices.is_empty() {
        return String::new();
    }
    let clips: usize = notices.iter().map(|n| n.segments).sum();
    let mut md = format!(
        "\n## Processed Audio\n\n\
         {clips} clip(s) from {} recording(s) in this dataset are NOT original recordings. \
         Their audio was processed before it entered the library.\n\n\
         | Recording | Clips | Processing | Timeline maps to source |\n|---|---|---|---|\n",
        notices.len()
    );
    for n in notices {
        md.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            n.audio_path.replace('|', "\\|"),
            n.segments,
            n.processing.replace('|', "\\|"),
            if n.timeline_preserved { "yes" } else { "NO — non-speech was cut out" }
        ));
    }
    md.push_str(
        "\nRecordings absent from this table make no claim either way: nothing recorded that they \
         were processed, which is not the same as confirming they are original.\n",
    );
    md
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportSegmentRecord {
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
    fn new(segment: &SpeechSegment) -> Self {
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
fn speaker_turn_csv(segment: &SpeechSegment) -> &'static str {
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

/// Require the durable, canonical PCM identity that binds one segment's transcript/review authority
/// to its source recording. A missing v51 identity is UNKNOWN, never permission to export whatever
/// bytes happen to occupy the path today.
pub(crate) fn required_segment_audio_content_hash(db: &Database, segment_id: &str, context: &str) -> AppResult<String> {
    db.segment_audio_content_hash(segment_id)?.ok_or_else(|| {
        AppError::Validation(format!(
            "{context}: segment {segment_id} has no canonical audio-content authority; backfill/re-import it before exporting audio"
        ))
    })
}

/// Verify an already-decoded PCM buffer before any bytes derived from it are written. The caller
/// writes from this same buffer, closing the path-level TOCTOU where a source could be replaced after
/// a metadata/path check but before decode.
pub(crate) fn require_decoded_segment_audio_identity(
    db: &Database,
    segment_id: &str,
    pcm: &[i16],
    sample_rate: u32,
    context: &str,
) -> AppResult<String> {
    let actual = crate::fingerprint::AudioFingerprint::content_hash(pcm, sample_rate);
    require_segment_audio_identity_hash(db, segment_id, &actual, context)?;
    Ok(actual)
}

/// Compare a canonical PCM hash computed over the exact decode an exporter is about to consume.
/// Split out so grouped/streaming exporters hash a source once while still checking every row's
/// independently stored authority.
pub(crate) fn require_segment_audio_identity_hash(
    db: &Database,
    segment_id: &str,
    actual_content_hash: &str,
    context: &str,
) -> AppResult<()> {
    let expected = required_segment_audio_content_hash(db, segment_id, context)?;
    if expected != actual_content_hash {
        return Err(AppError::Validation(format!(
            "{context}: source audio for segment {segment_id} no longer matches its stored canonical PCM identity; refusing to pair current bytes with prior transcript/review authority"
        )));
    }
    Ok(())
}

/// `unreadable_source_audio_counts_as_verified` exists for ONE caller: the `dropped_unavailable`
/// tally, which must answer "would this row have been written had the audio been there?". Every other
/// gate below reads only the grade and DB records, but the source-reference identity check opens and
/// re-hashes the source file — so on an unmounted drive it refused every SILVER commit-evidence row,
/// and those rows were then dropped WITHOUT being counted (dataset_infos.json reporting
/// droppedUnavailableAudio = 0 while the export path promises the drop "is counted, not silent").
/// The WRITE path always passes `false` and stays fail-closed.
fn is_training_ready_for_huggingface_export(
    db: &Database,
    segment: &SpeechSegment,
    grade: &TrainingGradeReport,
    ready_agentic_segment_ids: &BTreeSet<String>,
    required_source_reference_models: &[String],
    unreadable_source_audio_counts_as_verified: bool,
) -> AppResult<bool> {
    if !grade.training_ready {
        return Ok(false);
    }
    // RIGHTS GATE (migration v49, audit #6). Checked BEFORE the expensive hypothesis/coverage work
    // below, because no amount of corroboration makes a clip usable when consent does not.
    //
    // WITHDRAWN CONSENT ONLY, deliberately — not "undeclared". This export writes a local HuggingFace
    // dataset directory, which is a training-set preparation step; publishing it to the Hub is a
    // separate, later act. Refusing every undeclared recording here would block the owner's entire
    // existing library (144 clips, none declared) the moment this migration lands, silently redefining
    // what a working command does. `permits_redistribution()` exists and is tested for the caller that
    // genuinely publishes; wiring it HERE is an owner decision, not one to smuggle in with a schema
    // change.
    //
    // A withdrawal carries no such ambiguity: it must be honoured on every path, and it is.
    if db.rights_for_segment(&segment.id)?.is_revoked() {
        return Ok(false);
    }
    if grade.grade == quality::TRAINING_GRADE_SILVER {
        if !ready_agentic_segment_ids.contains(&segment.id) {
            return Ok(false);
        }
        let hypotheses = db.get_hypotheses_for_segment(&segment.id)?;
        if !quality::hypothesis_coverage_for_model_outputs(&hypotheses).passes_minimum {
            return Ok(false);
        }
        if segment_has_source_reference_commit_evidence(segment)
            && !source_reference_identity_verified_for_huggingface_export(
                db,
                segment,
                required_source_reference_models,
                unreadable_source_audio_counts_as_verified,
            )?
        {
            return Ok(false);
        }
        return Ok(true);
    }
    Ok(true)
}

fn segment_has_source_reference_commit_evidence(segment: &SpeechSegment) -> bool {
    let Some(evidence) = segment.evidence_json.as_deref().map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence) else {
        return false;
    };
    quality::has_source_reference_commit_evidence(&value)
}

fn source_reference_identity_verified_for_huggingface_export(
    db: &Database,
    segment: &SpeechSegment,
    required_source_reference_models: &[String],
    unreadable_source_audio_counts_as_verified: bool,
) -> AppResult<bool> {
    let references = db.get_source_transcripts_for_audio(&segment.audio_path)?;
    if references.is_empty() {
        return Ok(false);
    }

    for required_model in required_source_reference_models {
        let Some(reference) = references.iter().find(|reference| reference.model_id == *required_model) else {
            return Ok(false);
        };
        if !crate::agentic::is_usable_source_reference_transcript(&reference.transcript_text)
            || !source_reference_record_matches_current_audio(reference, unreadable_source_audio_counts_as_verified)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn source_reference_record_matches_current_audio(
    reference: &SourceTranscriptRecord,
    unreadable_source_audio_counts_as_verified: bool,
) -> bool {
    // A record with no stored identity at all is refused in BOTH modes: that is a property of the
    // record, not of the audio, so the tally must skip it exactly as the write loop does.
    let Some(stored_hash) = reference.audio_content_hash.as_deref().map(str::trim).filter(|value| !value.is_empty())
    else {
        return false;
    };
    let Some(stored_size) = reference.audio_size_bytes else {
        return false;
    };
    let current_identity = match crate::pipeline::source_audio_identity(std::path::Path::new(&reference.audio_path)) {
        Ok(identity) => identity,
        // Bytes that are not there cannot be compared. Writing: fail closed. COUNTING the rows an
        // unavailable source cost us: the answer is "it would have been written" — otherwise the drive
        // being unmounted is precisely what hides the loss it caused.
        Err(_) => return unreadable_source_audio_counts_as_verified,
    };
    stored_hash == current_identity.content_hash && stored_size == current_identity.size_bytes
}

fn ready_agentic_huggingface_segment_ids(db: &Database) -> AppResult<BTreeSet<String>> {
    let Some(report) = crate::runs::list_agent_import_reports(db, Some(1))?.into_iter().next() else {
        return Ok(BTreeSet::new());
    };
    let promotion_ready = report
        .summary
        .orchestration_stages
        .iter()
        .any(|stage| stage.stage == "dataset_promotion" && stage.status == "ready");
    let readiness_ready = report
        .summary
        .agentic_readiness
        .as_ref()
        .and_then(|readiness| readiness.get("ready"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && report
            .summary
            .agentic_readiness
            .as_ref()
            .and_then(|readiness| readiness.get("status"))
            .and_then(serde_json::Value::as_str)
            == Some("ready");
    if !promotion_ready || !readiness_ready {
        return Ok(BTreeSet::new());
    }
    Ok(report.segment_ids.into_iter().collect())
}

/// Remove every segment that must never leave this machine, whatever the caller is writing.
///
/// THREE independent exclusions, because each was being missed one caller at a time:
///
/// 1. HELD-OUT GOLD (by audio_path OR content hash) — so a TRAINING export cannot leak the eval
///    set's reference transcripts and contaminate the very set the promotion gate measures against.
///    Fail-closed: a path match excludes a held-out clip even when its file is missing (its content
///    can no longer be re-hashed); a hash match catches the same content at any path.
///
/// 2. WITHDRAWN CONSENT — added 2026-08-06 after an adversarial sweep found revocation was consulted
///    in exactly THREE places (`is_training_ready_for_huggingface_export`, `export_dataset`'s row
///    loop, and the production bundle's rights gate) while `export_audio_segments` and
///    `eval::export_finetune_pack` wrote a withdrawn recording's ACTUAL VOICE — WAV/FLAC clips plus
///    its transcripts — to disk with no rights call at all. Voice is biometric data under GDPR
///    Art. 9; a withdrawal is the one instruction the whole rights schema exists to obey.
///
///    Commit 56d8855 claimed "a withdrawal must be honoured on every path, and it is". That was
///    false: it was verified on the paths that commit touched and generalised from them. It is true
///    now, and it is true HERE rather than at five call sites, because a rule enforced per-caller is
///    a rule that gets missed by the sixth caller.
///
/// 3. SPOT-CHECK ANSWER KEYS (`is_gold`) — hidden review traps with a known answer. `export_audio`
///    was the only caller that refused them, so every tabular/HF/bundle export shipped the key.
///
/// The name says "unexportable", not "holdout", so it cannot quietly under-describe what it drops
/// the next time a reason is added.
pub(crate) fn exclude_unexportable_segments(
    db: &Database,
    segments: Vec<SpeechSegment>,
) -> AppResult<Vec<SpeechSegment>> {
    exclude_unexportable_segments_with_holdout_policy(db, segments, true)
}

/// Apply every universal export exclusion while intentionally retaining eval holdouts.
///
/// Human-facing transcript/subtitle exports are not training artifacts, so the owner may export a
/// holdout transcript. Revoked consent, rejects, technical-unusable rows, placeholders, and hidden
/// review answer keys remain universal exclusions. Keeping this as a named wrapper prevents a caller
/// from bypassing the shared rights gate merely because its holdout policy differs.
pub(crate) fn exclude_unexportable_segments_including_holdouts(
    db: &Database,
    segments: Vec<SpeechSegment>,
) -> AppResult<Vec<SpeechSegment>> {
    exclude_unexportable_segments_with_holdout_policy(db, segments, false)
}

fn exclude_unexportable_segments_with_holdout_policy(
    db: &Database,
    segments: Vec<SpeechSegment>,
    exclude_holdouts: bool,
) -> AppResult<Vec<SpeechSegment>> {
    // A registered review pool IS the authority on what may ship, and it is enforced HERE for the
    // same reason as the withdrawal and human-rejected rules below: every export path already routes
    // through this function, and a rule enforced per-caller is a rule the sixth caller misses.
    //
    // `export_dataset`, the HuggingFace training export and the production bundle all start from
    // `db.get_segments(None)` — EVERY row in the library, curated or not. Measured on the live
    // library on 2026-08-29, an export taken without this guard shipped 43,722 rows of which 23,405
    // (54%, 54.8 h) were outside the pool, and ~22,700 of those carried a machine transcript with no
    // human ever having heard the clip. The curated 43.8 h would have been the minority of its own
    // dataset. `None` = no pool registered (the pre-pool corpus), which leaves scope exactly as it
    // was before this guard existed.
    let pool_scope = crate::review_pool::exportable_segment_ids(db).map_err(AppError::Validation)?;
    // OWNER CANON 2026-08-29: "a sentence is decided by any two DIFFERENT reviewers". Membership
    // says a clip is IN the corpus; it does not say anyone decided it. Without this second scope an
    // export ships one reviewer's unconfirmed opinion as training truth -- measured on 2026-08-29,
    // the fine-tune pack emitted 410 clips carrying exactly one review each, while the pool itself
    // reported resolved=0. Two quality bars were in play and the weaker one was the one that
    // shipped. Only a clip two distinct reviewers agreed on (or one the owner adjudicated) may
    // leave. `NeedsThird` and `OwnerConflict` are unresolved disagreements and are held back.
    let consensus = match pool_scope {
        Some(_) => Some(
            crate::review_pool::segment_resolutions(db, None)
                .map_err(AppError::Validation)?
                .into_iter()
                .map(|resolution| (resolution.segment_id.clone(), resolution))
                .collect::<std::collections::HashMap<_, _>>(),
        ),
        None => None,
    };
    let mut kept = Vec::with_capacity(segments.len());
    let mut in_pool = 0usize;
    let mut undecided = 0usize;
    for mut seg in segments {
        // Never trust a projection supplied by a caller or retained from an earlier export.
        seg.export_review = None;
        if pool_scope.as_ref().is_some_and(|scope| !scope.contains(&seg.id)) {
            tracing::info!(segment_id = %seg.id, "export: dropping segment outside the active review pool");
            continue;
        }
        in_pool += 1;
        if consensus.as_ref().is_some_and(|resolved| {
            !resolved.get(&seg.id).is_some_and(|row| matches!(row.status.as_str(), "resolved" | "ownerResolved"))
        }) {
            undecided += 1;
            tracing::info!(segment_id = %seg.id, "export: dropping segment no two reviewers have decided");
            continue;
        }
        if let Some(resolution) = consensus.as_ref().and_then(|rows| rows.get(&seg.id)) {
            if resolution.final_action.as_deref() == Some("reject") {
                continue;
            }
            seg.export_review = Some(crate::export_review::ExportReviewAuthority::retained(resolution)?);
        }
        if db.rights_for_segment(&seg.id)?.is_revoked() {
            tracing::info!(segment_id = %seg.id, "export: dropping segment with withdrawn consent");
            continue;
        }
        // Text-provenance audit #11/#12: enforced HERE, at the shared root, for the same reason as
        // the withdrawal rule above — export_audio applied neither filter, so a human-REJECTED clip
        // (bad data the reviewer discarded; `verified` is deliberately true on it) and a
        // placeholder-only clip both shipped in the audio export while every other exporter dropped
        // them.
        if crate::quality::is_human_rejected(&seg) {
            tracing::info!(segment_id = %seg.id, "export: dropping human-rejected segment");
            continue;
        }
        if let Some(reason) = crate::quality::technical_unusable_reason(&seg) {
            tracing::info!(segment_id = %seg.id, reason, "export: dropping technically unusable segment");
            continue;
        }
        if crate::quality::is_effective_placeholder(&seg) {
            tracing::info!(segment_id = %seg.id, "export: dropping placeholder-only segment");
            continue;
        }
        // An `is_gold` row is a hidden spot-check ANSWER KEY (owner canon: the phone must never be
        // served its own answer key). `human_export_label` in export_audio refused those rows on its
        // own — and only there, so every tabular/HF/bundle exporter shipped the key verbatim. Same
        // shape as the withdrawal rule above: enforced at the root, not at whichever caller remembered.
        // The holdout filter below does NOT cover this — it keys on the separate `gold_segments` table,
        // and a flagged answer key need not be registered there.
        if seg.is_gold {
            tracing::info!(segment_id = %seg.id, "export: dropping is_gold answer-key segment");
            continue;
        }
        kept.push(seg);
    }
    // An export that silently returns nothing reads as a broken button, not as a rule doing its job.
    // When consensus is the ONLY reason the result is empty, say so with the count, so the operator
    // learns "no clip has two reviewers yet" instead of filing a bug against the exporter. Guarded on
    // `undecided > 0` and `in_pool > 0` so a genuinely empty library still exports empty as before.
    if kept.is_empty() && undecided > 0 && in_pool > 0 {
        return Err(AppError::Validation(format!(
            "nothing is exportable yet: {undecided} of {in_pool} clips in the review pool are still \
             waiting for a decision, and none has been decided. OWNER CANON: a sentence ships only \
             once any two DIFFERENT reviewers have agreed on it. Get second opinions onto reviewed \
             clips and export again."
        )));
    }
    let segments = kept;
    if !exclude_holdouts {
        return Ok(segments);
    }
    let holdout = crate::jury::learning::holdout_content_hashes(db)?;
    let holdout_paths = crate::jury::learning::holdout_audio_paths(db)?;
    // All VAD chunks of one recording share a single audio_path. Memoize path -> held_out so a source
    // split into N segments is content-hashed at most ONCE, not N times; and when there are no holdout
    // content hashes (the common no-gold case) skip the whole-file hash entirely — the path check alone
    // decides. Without this, exporting a long recording re-hashed the same multi-MB file once per segment.
    let mut path_cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    Ok(segments
        .into_iter()
        .filter(|seg| {
            let held_out = if holdout_paths.contains(&seg.audio_path) {
                true
            } else if holdout.is_empty() {
                false
            } else {
                *path_cache.entry(seg.audio_path.clone()).or_insert_with(|| {
                    let path = std::path::Path::new(&seg.audio_path);
                    if !path.exists() {
                        // Fail CLOSED, exactly like the present-but-unhashable Err case below: a MISSING
                        // file cannot be re-hashed to prove it is NOT the same content as a holdout gold
                        // clip re-imported at a DIFFERENT path (the exact-path check above would miss
                        // that), so keeping it would leak the eval reference into the training export
                        // (eval-on-train contamination). Only reached when a content-hash holdout is
                        // registered (holdout.is_empty() short-circuits above). Mirrors the fail-closed
                        // DPO / LM-corpus missing-file guards in jury/learning.rs.
                        tracing::warn!("Holdout check: {} is missing — excluding it fail-closed", path.display());
                        return true;
                    }
                    match crate::pipeline::source_audio_identity(path) {
                        Ok(id) => holdout.contains(&id.content_hash),
                        // Fail CLOSED: a present-but-unhashable clip (transient file lock, permission
                        // blip, partial read) may be the SAME CONTENT as a holdout gold clip re-imported
                        // at a DIFFERENT path — which the exact-path check above would miss. Excluding an
                        // unverifiable present clip protects the eval set from leaking into the training
                        // export (eval-on-train contamination — silently inflated WER/CER). This mirrors
                        // the fail-closed DPO / LM-corpus holdout guards in jury/learning.rs; the
                        // holdout.is_empty() short-circuit above means this can only exclude when a
                        // content-hash holdout is actually registered.
                        Err(e) => {
                            tracing::warn!(
                                "Holdout check could not hash {}: {e} — excluding it fail-closed",
                                path.display()
                            );
                            true
                        }
                    }
                })
            };
            if held_out {
                tracing::warn!("Excluding segment {} from dataset export: matches holdout gold audio", seg.id);
            }
            !held_out
        })
        .collect())
}

pub fn export_dataset(db: &Database, path: &std::path::Path, format: &ExportFormat) -> AppResult<()> {
    crate::review_campaign::require_export_unblocked(db, "dataset export")?;
    // Drop held-out gold segments BEFORE counting or writing any format, so the training tables
    // (JSON/JSONL/CSV/Parquet) — including the production bundle that delegates through here — never
    // publish the eval set's reference transcripts; closes the eval-on-train leak the HF export
    // already guards against.
    let segments = exclude_unexportable_segments(db, db.get_segments(None)?)?;
    // A human-REJECTED clip ("mark bad" in review) is kept in the library but is bad data the reviewer
    // discarded — never publish it, and never count it as verified. The HuggingFace/training path
    // already drops it via training_grade_for_segment; do the same here so the plain JSON/JSONL/CSV/
    // Parquet tables and `verified_segments` can never label a reject as confirmed-good output.
    let segments: Vec<SpeechSegment> = segments.into_iter().filter(|s| !quality::is_human_rejected(s)).collect();
    // A segment still showing an ASR placeholder as its EFFECTIVE transcript ("[Pending WSL 7B ASR]" /
    // "[ASR unavailable…]") is not a transcript — never ship the literal placeholder string as a training
    // row (an export taken mid-import would otherwise do exactly that). Exclude and log the count.
    let before_pending = segments.len();
    let segments: Vec<SpeechSegment> = segments.into_iter().filter(|s| !quality::is_effective_placeholder(s)).collect();
    let pending_excluded = before_pending - segments.len();
    if pending_excluded > 0 {
        tracing::warn!("dataset export excluded {pending_excluded} not-yet-transcribed (placeholder) segment(s)");
    }
    // REVOKED CONSENT OUTRANKS EVERYTHING (migration v49, audit #6). This is the LOCAL export path —
    // the permissive one, which deliberately still writes rights-unknown rows so a personal library
    // keeps working. A withdrawal is different in kind from a missing licence: a withdrawal that only
    // blocked publishing, while the clip kept flowing into every local JSON/JSONL/CSV/Parquet table,
    // would not be a withdrawal at all. Dropped here, before any counting or writing, so no total in
    // this file can include a revoked recording.
    let before_revoked = segments.len();
    let mut revoked_ids = Vec::new();
    for seg in &segments {
        if db.rights_for_segment(&seg.id)?.is_revoked() {
            revoked_ids.push(seg.id.clone());
        }
    }
    let segments: Vec<SpeechSegment> = segments.into_iter().filter(|s| !revoked_ids.contains(&s.id)).collect();
    let revoked_excluded = before_revoked - segments.len();
    if revoked_excluded > 0 {
        tracing::warn!("dataset export excluded {revoked_excluded} segment(s) whose consent was withdrawn");
    }

    // Keep the prior artifact intact until the captured final decisions are re-proven.
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let pending = parent.join(format!(".review-export-{}.tmp", uuid::Uuid::new_v4().simple()));
    remove_file_on_error(
        &pending,
        (|| -> AppResult<()> {
            export_dataset_from_segments(db, &pending, format, &segments)?;
            crate::export_review::verify_current(db, &segments)?;
            crate::atomic_file::replace_file(&pending, path)?;
            Ok(())
        })(),
    )
}

/// Write one tabular dataset format from an already-selected immutable row snapshot.
///
/// The production bundle deliberately calls this for all four formats with the SAME preflight
/// snapshot. Calling [`export_dataset`] four times would query the live database four times, allowing
/// a concurrent insert/re-accept/edit to enter only some files after rights/source checks had already
/// run. This helper performs no row query and no policy selection; callers must pass the exact rows
/// they intend to describe.
pub(crate) fn export_dataset_from_segments(
    db: &Database,
    path: &std::path::Path,
    format: &ExportFormat,
    segments: &[SpeechSegment],
) -> AppResult<()> {
    let processed_audio = processed_audio_notices(db, segments)?;
    export_dataset_from_snapshot(path, format, segments, &processed_audio)
}

pub(crate) fn export_dataset_from_snapshot(
    path: &std::path::Path,
    format: &ExportFormat,
    segments: &[SpeechSegment],
    processed_audio: &[ProcessedAudioNotice],
) -> AppResult<()> {
    // Telemetry (Week-1 "measure first"): real export wall-clock. The guard records on return; metadata
    // carries the format so the owner can compare JSON/JSONL/CSV/Parquet costs via get_recent_spans /
    // get_tracing_stats. Mirrors the asr.transcribe / decode / normalizer guards.
    let _span = crate::telemetry::TRACER
        .start_span("export.dataset", crate::telemetry::Tracer::metadata(vec![("format", format!("{format:?}"))]));
    let total_duration: i64 = segments.iter().map(|s| s.duration_ms).sum();
    let verified = segments.iter().filter(|s| s.verified).count();
    // Say which recordings were processed before import. Computed AFTER every exclusion above, so the
    // counts describe what this file actually contains.
    if !processed_audio.is_empty() {
        let clips: usize = processed_audio.iter().map(|n| n.segments).sum();
        tracing::info!(
            "dataset export declares {clips} clip(s) from {} recording(s) whose audio was processed before import",
            processed_audio.len()
        );
    }

    let metadata = DatasetMetadata {
        name: "cortex-kurdish-speech-dataset".into(),
        version: "2.0".into(),
        language: "ckb".into(),
        script: "Arabic".into(),
        total_segments: segments.len(),
        total_duration_ms: total_duration,
        verified_segments: verified,
        training_grade_summary: quality::training_grade_summary(segments),
        composition: compute_composition(segments),
        processed_audio: processed_audio.to_vec(),
        exported_at: chrono::Utc::now().to_rfc3339(),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match format {
        ExportFormat::Json => export_json(path, &metadata, segments),
        ExportFormat::Jsonl => export_jsonl(path, segments),
        ExportFormat::Csv => export_csv(path, segments),
        ExportFormat::Parquet => export_parquet(path, segments),
    }
}

/// Minimal union-find (disjoint-set) over string-keyed nodes, used to build leakage-safe split groups
/// as connected components of the bipartite (recording, speaker) graph.
struct UnionFind {
    ids: std::collections::HashMap<String, usize>,
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new() -> Self {
        Self { ids: std::collections::HashMap::new(), parent: Vec::new(), rank: Vec::new() }
    }

    /// Get the id for `key`, creating a new singleton set if it is unseen.
    fn node(&mut self, key: &str) -> usize {
        if let Some(&id) = self.ids.get(key) {
            return id;
        }
        let id = self.parent.len();
        self.parent.push(id);
        self.rank.push(0);
        self.ids.insert(key.to_string(), id);
        id
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]]; // path halving
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }
}

/// Deterministic, leakage-safe train/val/test assignment for the HuggingFace export.
///
/// Two properties a training dataset must have, both of which the previous inline logic
/// broke:
/// 1. **No source-recording leakage** — every segment cut from the same source recording
///    lands in the same split; otherwise near-identical acoustic content leaks train→test.
///    With `speaker_disjoint`, the grouping unit is a connected component of the bipartite
///    (recording, speaker) graph, so a unit is BOTH speaker-disjoint AND keeps every recording
///    intact — a multi-speaker recording can never straddle two splits.
/// 2. **Seed reproducibility** — groups are visited in sorted-then-seed-shuffled order, so the
///    same segments + seed always yield the same split. (The old code shuffled `HashMap`
///    keys, whose iteration order is randomised per run, so the seed pinned nothing.)
///
/// Greedily fills each split toward its duration-proportional target. Returns
/// `(segment_id, split)` for every input segment.
pub fn assign_splits(
    segments: &[SpeechSegment],
    train_ratio: f64,
    val_ratio: f64,
    test_ratio: f64,
    seed: u64,
    speaker_disjoint: bool,
) -> Vec<(String, &'static str)> {
    let (mut tr, mut vr, mut te) = (train_ratio, val_ratio, test_ratio);
    let sum = tr + vr + te;
    if sum > 0.0 {
        tr /= sum;
        vr /= sum;
        te /= sum;
    } else {
        tr = 0.8;
        vr = 0.1;
        te = 0.1;
    }

    fn source_name(path: &str) -> &str {
        path.rsplit(['/', '\\']).next().unwrap_or(path)
    }

    /// `SPEAKER_00`, `SPEAKER_01`, ... is a diarizer's PER-RECORDING index, not a person. The
    /// numbering restarts for every file, so the same label names a different human in every
    /// recording — and the app has no global speaker identity to say otherwise (that needs CAM++
    /// embeddings clustered across files, which nothing does yet).
    ///
    /// MEASURED 2026-08-13 on the live library: `SPEAKER_00` appeared in 141 of 144 recordings.
    /// Union-ing on it as if it were an identity chained every recording into ONE connected
    /// component holding 100% of the audio, so the greedy fill put everything in `train` and
    /// emitted EMPTY validation and test splits — silently, with `hf_speaker_disjoint = true`
    /// looking like it was protecting the dataset. Scoping the label to its recording restores
    /// 145 components with the largest at 46.7%, which an 80/10/10 split can actually satisfy.
    ///
    /// A real speaker id (anything not matching this shape) still unions globally: that is where
    /// speaker-disjointness has meaning, and this must not weaken it.
    fn is_generic_diarizer_label(label: &str) -> bool {
        label
            .strip_prefix("SPEAKER_")
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    }

    // Group into leakage-safe units. With speaker_disjoint, a unit is a connected component of the
    // bipartite (recording, speaker) graph (built by union-find): each component keeps every source
    // recording INTACT (no multi-speaker recording straddles two splits) AND is speaker-disjoint (no
    // speaker spans two splits). Without speaker_disjoint, units are simply per-recording. BTreeMap
    // keeps the canonical keys in a stable sorted order for seed-reproducible shuffling.
    let mut groups: std::collections::BTreeMap<String, Vec<&SpeechSegment>> = std::collections::BTreeMap::new();
    if speaker_disjoint {
        let mut uf = UnionFind::new();
        for seg in segments {
            let r = uf.node(&format!("r:{}", source_name(&seg.audio_path)));
            let spk = seg.speaker_id.as_deref().unwrap_or("").trim();
            if !spk.is_empty() {
                let s = if is_generic_diarizer_label(spk) {
                    uf.node(&format!("s:{}:{spk}", source_name(&seg.audio_path)))
                } else {
                    uf.node(&format!("s:{spk}"))
                };
                uf.union(r, s);
            }
        }
        // Canonical key per component = the lexicographically smallest recording name in it
        // (deterministic and unique — each recording belongs to exactly one component).
        let mut root_key: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
        for seg in segments {
            let rec = source_name(&seg.audio_path).to_string();
            let r = uf.node(&format!("r:{rec}"));
            let root = uf.find(r);
            root_key
                .entry(root)
                .and_modify(|m| {
                    if rec < *m {
                        *m = rec.clone();
                    }
                })
                .or_insert(rec);
        }
        for seg in segments {
            let r = uf.node(&format!("r:{}", source_name(&seg.audio_path)));
            let root = uf.find(r);
            groups.entry(root_key[&root].clone()).or_default().push(seg);
        }
    } else {
        for seg in segments {
            groups.entry(source_name(&seg.audio_path).to_string()).or_default().push(seg);
        }
    }

    // Sorted keys, then a seeded Fisher–Yates shuffle → reproducible from `seed` alone.
    let mut keys: Vec<&String> = groups.keys().collect();
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        // splitmix64 step — strong distribution, fully deterministic.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    for i in (1..keys.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        keys.swap(i, j);
    }

    let total: i64 = segments.iter().map(|s| s.duration_ms).sum();
    let target_train = (total as f64 * tr) as i64;
    let target_val = (total as f64 * vr) as i64;
    let target_test = (total as f64 * te) as i64;
    let (mut d_train, mut d_val, mut d_test) = (0i64, 0i64, 0i64);

    // Largest group first. The rule below picks the split with the biggest ABSOLUTE deficit, and in
    // shuffled order that sends everything to `train` whenever the groups are FEW and UNEQUAL: train's
    // deficit (80% of the corpus) exceeds the whole of val's or test's target until train is nearly
    // full, so every group loses to train no matter how small it is. Pinned by
    // `three_unequal_recordings_still_fill_all_three_splits` — three recordings at 91.9/7.9/0.2 which
    // CAN fill three splits and did not.
    //
    // Honest scope: this is NOT what produced the owner's all-train export on 2026-08-15. That ran
    // over 179 recordings (splits are computed across every non-rejected clip, not just the
    // exportable ones) and correctly put the three recordings holding reviewed clips in train. This
    // fixes a real weakness for SMALL libraries, and nothing that was observed in production.
    //
    // Descending duration is the standard remedy (longest-processing-time first). Determinism is
    // unchanged: the seed-shuffled order above survives as the tie-break for equal-duration groups.
    let group_dur_of = |k: &String| -> i64 { groups[k].iter().map(|s| s.duration_ms).sum() };
    let mut ordered: Vec<&String> = keys.into_iter().collect();
    ordered.sort_by_key(|k| std::cmp::Reverse(group_dur_of(k)));
    let keys = ordered;

    let mut out: Vec<(String, &'static str)> = Vec::with_capacity(segments.len());
    for key in keys {
        let segs = &groups[key];
        let group_dur: i64 = segs.iter().map(|s| s.duration_ms).sum();
        let (def_train, def_val, def_test) = (target_train - d_train, target_val - d_val, target_test - d_test);
        let split = if def_train >= def_val && def_train >= def_test {
            d_train += group_dur;
            "train"
        } else if def_val >= def_train && def_val >= def_test {
            d_val += group_dur;
            "validation"
        } else {
            d_test += group_dur;
            "test"
        };
        for seg in segs {
            out.push((seg.id.clone(), split));
        }
    }
    out
}

/// Decide the PCM slice for a segment's exported WAV from its alignment window.
///
/// Returns `None` when the segment must be SKIPPED rather than paired with the wrong audio:
/// - alignment is present and parses but the window is out of range / degenerate (end <= start), OR
/// - alignment is PRESENT but has no source offsets (e.g. a bare word-timestamp array — the shape a
///   clobbered `alignment_json` takes). Emitting the whole file here would pair the entire recording
///   with a short clip's transcript — the exact silent training-data corruption to prevent.
///
/// Only a GENUINELY-ABSENT alignment (`None`) falls back to the whole file, which is correct for a
/// single-file segment that never carried chunk metadata. Every real chunked segment carries a
/// `SegmentSourceMeta` (even chunk_count==1 records `source_start_ms=0`), so a present-but-offset-less
/// alignment can only mean the offsets were lost — skip it, don't guess.
pub(crate) fn slice_for_export<'a>(
    full_pcm: &'a [i16],
    sample_rate: u32,
    alignment_json: Option<&str>,
) -> Option<std::borrow::Cow<'a, [i16]>> {
    match alignment_json {
        // Truly no alignment metadata -> a single-file segment; the whole file IS the clip.
        None => Some(std::borrow::Cow::Borrowed(full_pcm)),
        Some(json) => match chunking::SegmentSourceMeta::from_alignment_json(json) {
            Some(meta) => {
                let (start_ms, end_ms) = (meta.source_start_ms.max(0), meta.source_end_ms.max(0));
                // Reject an absurd offset rather than truncating via `as u32` (i64 -> u32 wraps mod 2^32,
                // which would silently slice an UNRELATED in-bounds window and EXPORT it mislabeled with
                // this segment's transcript — training-data corruption). Mirrors the identical guard in
                // chunking::slice_pcm_by_alignment; a value this large is a malformed/crafted alignment
                // blob (the app never emits an offset > u32::MAX ms ≈ 49.7 days). Skip, don't emit.
                if start_ms > u32::MAX as i64 || end_ms > u32::MAX as i64 {
                    return None;
                }
                let start = chunking::ms_to_samples(start_ms as u32, sample_rate);
                let end = chunking::ms_to_samples(end_ms as u32, sample_rate).min(full_pcm.len());
                if end > start && start < full_pcm.len() {
                    Some(std::borrow::Cow::Borrowed(&full_pcm[start..end]))
                } else {
                    None // present-but-out-of-range window -> skip, never emit the whole file
                }
            }
            // Present but no source offsets (clobbered/anomalous) -> skip, never emit the whole file.
            None => None,
        },
    }
}

/// Lowercase-hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Write a standard `SHA256SUMS` file covering every file under `dir`, so a published
/// dataset can be integrity-checked (truncation, corruption, partial copies) with
/// `sha256sum -c SHA256SUMS`. Lines are `<hex>  <relative/path>`, sorted by path with
/// forward slashes, deterministic regardless of filesystem walk order. Excludes the
/// `SHA256SUMS` file itself and any `.tmp` staging files.
///
/// pub(crate): the fine-tune pack, gold eval-set, and production bundle reuse this — a truncated/
/// corrupted clip (this machine's ledger root-caused an LTO failure to large-file I/O corruption)
/// was undetectable while a manifest-only SHA stayed green (true-10 audit 2026-07-09).
pub(crate) fn write_sha256sums(dir: &std::path::Path) -> AppResult<()> {
    fn collect(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) -> AppResult<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                collect(&path, root, out)?;
            } else if ft.is_file() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // Skip the manifest itself and any in-flight or crash-leftover STAGING file. TWO temp
                // shapes exist: `*.tmp` (atomic text writes) and `*.tmp-<pid>-<nonce>` (audio-clip
                // staging, export_audio::temporary_output_path). Matching only `.tmp` would let a
                // crashed-run clip fragment (`foo.wav.tmp-1234-567`) be hashed into the manifest as if it
                // were a real dataset artifact. The `.tmp-` arm requires an all-digits/`-` tail so a real
                // file coincidentally containing `.tmp-` (e.g. `foo.tmp-bar.wav`) is NOT excluded.
                let is_staging = name.ends_with(".tmp")
                    || name.rfind(".tmp-").is_some_and(|i| {
                        let tail = &name[i + ".tmp-".len()..];
                        !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit() || b == b'-')
                    });
                if name == "SHA256SUMS" || is_staging {
                    continue;
                }
                let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                out.push((rel, sha256_hex(&std::fs::read(&path)?)));
            }
        }
        Ok(())
    }
    let mut files: Vec<(String, String)> = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();
    let mut body = String::new();
    for (rel, hash) in &files {
        body.push_str(hash);
        body.push_str("  ");
        body.push_str(rel);
        body.push('\n');
    }
    write_text_atomic(&dir.join("SHA256SUMS"), &body)
}

/// Like `write_sha256sums` but covers ONLY the named files (relative to `dir`) instead of scanning the
/// whole directory. The reviewed-audio exporter passes the same explicit inventory to its staged-tree
/// verifier and public result, so the command result, metadata and integrity manifest cannot drift.
/// Missing entries are skipped here for compatibility with other callers; the reviewed-audio verifier
/// immediately rejects any such omission before publication. `SHA256SUMS` is never self-listed.
pub(crate) fn write_sha256sums_for(dir: &std::path::Path, rel_files: &[String]) -> AppResult<()> {
    let mut files: Vec<(String, String)> = Vec::new();
    for rel in rel_files {
        if rel == "SHA256SUMS" {
            continue;
        }
        let path = dir.join(rel);
        if path.is_file() {
            files.push((rel.replace('\\', "/"), sha256_hex(&std::fs::read(&path)?)));
        }
    }
    files.sort();
    files.dedup();
    let mut body = String::new();
    for (rel, hash) in &files {
        body.push_str(hash);
        body.push_str("  ");
        body.push_str(rel);
        body.push('\n');
    }
    write_text_atomic(&dir.join("SHA256SUMS"), &body)
}

/// Export a HuggingFace Datasets–compatible directory (split folders + metadata + dataset card).
/// Build the per-clip output filename for the HF export. Both the source stem and the segment id
/// are caller/import-controlled (`validate_segment` only checks non-empty), so each is reduced to
/// `[A-Za-z0-9_-]` — guaranteeing the result is a single path component that cannot escape the
/// destination directory via separators or `..` when `join`ed (path traversal, CWE-22).
fn sanitized_clip_filename(stem: &str, id: &str) -> String {
    let clean = |s: &str| {
        s.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>()
    };
    let clean_id = clean(id);
    // The segment id is the unique key, so the filename only stays unique if distinct ids map to
    // distinct cleaned ids. For the universal case (a v4 UUID, all `[A-Za-z0-9-]`) cleaning is a no-op
    // and the id alone guarantees uniqueness. But the id is import-controlled and only checked
    // non-empty, so two ids that differ ONLY in stripped characters (`a/b` and `a.b` both -> `a_b`)
    // would otherwise collide and silently overwrite each other's clip + metadata row. When cleaning
    // actually altered the id, append a short hash of the RAW id to restore one-to-one uniqueness.
    if clean_id == id {
        format!("{}_{}.wav", clean(stem), clean_id)
    } else {
        format!("{}_{}_{}.wav", clean(stem), clean_id, &sha256_hex(id.as_bytes())[..8])
    }
}

pub fn export_huggingface_dataset(
    db: &Database,
    dir: &std::path::Path,
    settings: &crate::settings::AppSettings,
) -> AppResult<()> {
    export_huggingface_dataset_with_hook(db, dir, settings, |_| Ok(()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HuggingFacePublishPoint {
    BeforeDataPromotion,
    BeforeMetadataWrite,
    AfterDataPromotion,
    AfterMetadataPromotion,
    BeforePublicationCommit,
    AfterFilesCommitted,
}

fn export_huggingface_dataset_with_hook(
    db: &Database,
    dir: &std::path::Path,
    settings: &crate::settings::AppSettings,
    mut hook: impl FnMut(HuggingFacePublishPoint) -> AppResult<()>,
) -> AppResult<()> {
    crate::review_campaign::require_export_unblocked(db, "Hugging Face dataset export")?;
    // Telemetry (Week-1 "measure first"): real HuggingFace-export wall-clock (audio copy + shard writes).
    let _span = crate::telemetry::TRACER.start_span("export.huggingface", crate::telemetry::Tracer::metadata(vec![]));
    let publication = hf_publication::Publication::begin(dir)?;
    let staged_root = publication.staging();
    let data_dir = dir.join("data");
    // Stage the complete managed generation privately. Publication preserves and journals only
    // data/ plus the three HF sidecars, leaving unrelated files in the chosen directory untouched.
    let staging_dir = staged_root.join("data");
    let train_dir = staging_dir.join("train");
    let val_dir = staging_dir.join("validation");
    let test_dir = staging_dir.join("test");

    // Exclude held-out gold audio from the TRAINING export — the same fail-closed content-hash guard
    // the plain export, DPO, and LM-corpus exports use. Without it, a clip registered as a holdout
    // (for WER/CER eval) that ALSO exists as a normal training-ready segment leaks into data/train,
    // contaminating the very eval set the promotion gate measures against.
    let segments = exclude_unexportable_segments(db, db.get_segments(None)?)?;

    let ready_agentic_segment_ids = ready_agentic_huggingface_segment_ids(db)?;
    let required_source_reference_models = settings.source_reference_models();

    // The no-op guard must test whether any row would ACTUALLY be written — NOT merely whether the
    // library holds segments. The write loop below skips every row that is not training_ready, and every
    // machine row without HF coverage, so a library with segments but zero EXPORTABLE rows used to fall
    // straight through this guard, remove_dir_all() the prior good dataset, write nothing, and return
    // Ok(()) — silently replacing a previously-good export with an empty one. That is exactly the state
    // a missing word-aligner produces (every clip grades REVIEW => training_ready=false), so the raw
    // `segments.is_empty()` test destroyed real datasets on a real, documented configuration.
    let mut has_exportable_row = false;
    for seg in &segments {
        let grade = quality::training_grade_for_segment(seg);
        if grade.training_ready
            && is_training_ready_for_huggingface_export(
                db,
                seg,
                &grade,
                &ready_agentic_segment_ids,
                &required_source_reference_models,
                false,
            )?
        {
            has_exportable_row = true;
            break;
        }
    }
    if !has_exportable_row {
        // Nothing to export — a true NO-OP that PRESERVES any prior export rather than wiping it
        // (re-exporting with zero training-ready segments must not destroy a previous good dataset).
        return Ok(());
    }

    // This invocation owns a fresh unique staging tree. Never delete another invocation's stage.
    std::fs::create_dir_all(&train_dir)?;
    std::fs::create_dir_all(&val_dir)?;
    std::fs::create_dir_all(&test_dir)?;

    // Assign each segment to a split — deterministic (seed-reproducible) and without
    // splitting a source recording across train/val/test. See assign_splits().
    let assignments = assign_splits(
        &segments,
        settings.hf_train_ratio,
        settings.hf_val_ratio,
        settings.hf_test_ratio,
        settings.hf_split_seed,
        settings.hf_speaker_disjoint,
    );
    let split_of: std::collections::HashMap<&str, &'static str> =
        assignments.iter().map(|(id, s)| (id.as_str(), *s)).collect();

    let mut train_segs = Vec::new();
    let mut val_segs = Vec::new();
    let mut test_segs = Vec::new();
    // Collect the split assignments but DON'T persist them yet. The DB must be the LAST thing
    // mutated — only after every file is written — or a mid-export file-write failure leaves split
    // columns describing a dataset that was never fully written to disk.
    let mut pending_splits: Vec<(String, &'static str)> = Vec::with_capacity(segments.len());
    for seg in &segments {
        let split = split_of.get(seg.id.as_str()).copied().unwrap_or("train");
        pending_splits.push((seg.id.clone(), split));
        match split {
            "validation" => val_segs.push(seg.clone()),
            "test" => test_segs.push(seg.clone()),
            _ => train_segs.push(seg.clone()),
        }
    }

    // A split the owner ASKED for must not come out empty. Grouping is leakage-safe by design, so a
    // group too large to fit anywhere but `train` silently starves validation and test — which is
    // exactly what unstable diarizer labels did (see `is_generic_diarizer_label`): a dataset with no
    // holdout at all, exported without a word. Fail here instead. An export that stops is
    // recoverable; a training run against a dataset with no test set is not, because nothing about
    // the resulting numbers announces that they were measured on training data.
    // Fire on the COLLAPSE, not on scarcity. An empty split is ordinary arithmetic when there are
    // fewer clips than splits — a 2-segment fixture asking for 80/10/10 must leave one empty, and the
    // first version of this guard broke 18 export tests by calling that a failure. The defect it
    // exists for looks different: with plenty of clips, EVERY one lands in a single split because the
    // leakage-safe grouping collapsed (measured 2026-08-14: 100% of the corpus in one component when
    // per-recording diarizer labels were treated as speaker identities).
    // The denominator is independent RECORDINGS, not segments. Grouping never splits a recording, so
    // the number of distinct recordings is the ceiling on how many groups can exist — with fewer
    // recordings than splits, an empty split is arithmetic no matter how well the grouping worked.
    // (Second correction: `segments.len()` as the denominator still failed a 3-segment/2-recording
    // fixture, because three clips cut from two recordings can only ever fill two splits.)
    let requested_splits = 1 + usize::from(settings.hf_val_ratio > 0.0) + usize::from(settings.hf_test_ratio > 0.0);
    let distinct_recordings = segments
        .iter()
        .map(|s| s.audio_path.rsplit(['/', '\\']).next().unwrap_or(s.audio_path.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let populated_splits =
        usize::from(!train_segs.is_empty()) + usize::from(!val_segs.is_empty()) + usize::from(!test_segs.is_empty());
    if requested_splits > 1 && distinct_recordings >= requested_splits && populated_splits == 1 {
        let (name, ratio) = if val_segs.is_empty() && settings.hf_val_ratio > 0.0 {
            ("validation", settings.hf_val_ratio)
        } else {
            ("test", settings.hf_test_ratio)
        };
        {
            return Err(crate::error::AppError::Other(format!(
                "Export stopped: every clip landed in ONE split — the {name} split is empty while {:.0}% was requested. \
                 {} segments fell into leakage-safe groups too large to divide — most often because \
                 speaker labels are per-recording diarizer indices rather than real identities. \
                 Check speaker_id values, or set hf_speaker_disjoint = false to group by recording only.",
                ratio * 100.0,
                segments.len()
            )));
        }
    }

    // Helper closure to process and write a split's files
    let process_split = |split_segs: &[SpeechSegment],
                         _split_name: &str,
                         dest_dir: &std::path::Path|
     -> AppResult<(usize, f64, usize, Vec<String>)> {
        // NOTE: do NOT early-return when this split is empty on a re-export. Falling through writes a
        // HEADER-ONLY metadata.csv (the per-source loop below simply runs zero times) into the fresh
        // staging split dir, so an empty split's on-disk metadata agrees with dataset_infos.json's
        // num_examples=0 — never a stale or missing file. Returning early would leave the split dir with no
        // metadata.csv at all, disagreeing with the declared (now-empty) split after the swap.
        let csv_path = dest_dir.join("metadata.csv");
        let csv_tmp = csv_path.with_extension("csv.tmp");
        remove_file_on_error(
            &csv_tmp,
            (|| -> AppResult<(usize, f64, usize, Vec<String>)> {
                let mut csv_wtr = csv::Writer::from_path(&csv_tmp)?;
                csv_wtr.write_record([
                    "file_name",
                    "transcription",
                    "speaker_id",
                    "duration_ms",
                    "verified",
                    "training_grade",
                    "training_ready",
                    "transcript_source",
                    "training_reasons",
                    "dialect",
                    "speaker_turn",
                    "review_authority",
                ])?;

                let mut total_exported_dur = 0.0;
                let mut count = 0;
                // Segments dropped because their source audio is unavailable (missing or
                // undecodable) — real, previously-silent data loss, surfaced after export.
                let mut dropped_unavailable = 0usize;
                // Ids of segments actually WRITTEN to disk, so the split column is later persisted ONLY
                // for exported rows — never for ones dropped by the filters below (which would label a
                // clip the dataset does not contain and disagree with dataset_infos.json counts).
                let mut exported_ids: Vec<String> = Vec::new();

                // Group segments by source audio_path so each source file is decoded only once.
                // For a 2-hour podcast split into N segments, this avoids N full re-decodes.
                // A BTreeMap (NOT HashMap) keeps the per-source iteration order DETERMINISTIC: std
                // HashMap iterates in a per-process-random order, which permuted the metadata.csv row
                // blocks across otherwise-identical exports, changing each split's recorded SHA256SUMS
                // every run and breaking the byte-reproducibility the manifest is meant to guarantee.
                // Sorting by source path makes metadata.csv (and its hash) stable; within each source
                // the Vec keeps split_segs insertion order, which is already deterministic.
                let mut segs_by_source: std::collections::BTreeMap<&str, Vec<&SpeechSegment>> =
                    std::collections::BTreeMap::new();
                for seg in split_segs {
                    segs_by_source.entry(seg.audio_path.as_str()).or_default().push(seg);
                }

                // Count toward dropped_unavailable ONLY the rows that WOULD have been written — i.e. that
                // pass the same training-ready + HF-export gate the write loop applies below. A source's
                // non-training-ready REVIEW rows are skipped regardless of availability, so counting them
                // here inflated droppedUnavailableAudio (dataset_infos.json) and mislabeled REVIEW rows as
                // lost "training-ready" segments in the operator warnings.
                //
                // Every gate in is_training_ready_for_huggingface_export reads only the grade + DB records
                // EXCEPT one: a SILVER row carrying source-reference commit evidence re-hashes its source
                // audio to prove the stored identity still matches. On an unmounted drive that check cannot
                // run, so it refused — and the rows the missing drive actually cost us went uncounted while
                // dataset_infos.json reported droppedUnavailableAudio = 0. Pass `true` so an UNREADABLE
                // source counts as "would have been written"; a readable-but-STALE identity still does not
                // (this closure also runs for a present-but-undecodable source, where the hash succeeds).
                let count_exportable = |segs: &[&SpeechSegment]| -> AppResult<usize> {
                    let mut n = 0usize;
                    for &seg in segs {
                        let grade = quality::training_grade_for_segment(seg);
                        if is_training_ready_for_huggingface_export(
                            db,
                            seg,
                            &grade,
                            &ready_agentic_segment_ids,
                            &required_source_reference_models,
                            true,
                        )? {
                            n += 1;
                        }
                    }
                    Ok(n)
                };

                for (source_path_str, segs) in segs_by_source {
                    let source_path = std::path::Path::new(source_path_str);
                    // A source unavailable this run (unmounted/network drive, or an undecodable file) is
                    // DROPPED from this snapshot: its rows are absent from metadata.csv, and since each run
                    // stages into a FRESH tree (never carrying prior clips forward), the atomic swap replaces
                    // its old clips too. This is a consistent, self-healing snapshot — the prior dataset
                    // survives an ALL-unavailable run (the total_count==0 guard before the commit), and a
                    // transiently-unavailable source reappears on the next re-export once it is readable
                    // again. The drop is counted in dropped_unavailable (surfaced in dataset_infos.json), not
                    // silent. (Carrying a transiently-unavailable source's prior clips + rows forward to keep
                    // a larger interim snapshot is possible but must preserve metadata.csv/SHA consistency;
                    // deferred as an enhancement, tracked in the ledger.)
                    if !source_path.exists() {
                        for seg in &segs {
                            tracing::warn!("Skipping segment {} in HF export: audio not found", seg.id);
                        }
                        dropped_unavailable += count_exportable(&segs)?;
                        continue;
                    }

                    // Decode the source file exactly once.
                    let (sample_rate, full_pcm) = match audio::decode_to_pcm(source_path_str) {
                        Ok(res) => res,
                        Err(e) => {
                            tracing::error!("Failed to decode {source_path_str} in HF export: {e}");
                            dropped_unavailable += count_exportable(&segs)?;
                            continue;
                        }
                    };
                    // Hash the exact in-memory decode used for every slice below. Path metadata or a
                    // pre-decode hash leaves a replacement window; this buffer identity cannot drift
                    // out from under the bytes written to the staged dataset generation.
                    let current_content_hash =
                        crate::fingerprint::AudioFingerprint::content_hash(&full_pcm, sample_rate);

                    for seg in segs {
                        let grade = quality::training_grade_for_segment(seg);
                        if !grade.training_ready {
                            tracing::warn!(
                                "Skipping segment {} in HF export: training grade {} ({})",
                                seg.id,
                                grade.grade,
                                grade.reasons.join("; ")
                            );
                            continue;
                        }
                        if !is_training_ready_for_huggingface_export(
                            db,
                            seg,
                            &grade,
                            &ready_agentic_segment_ids,
                            &required_source_reference_models,
                            false,
                        )? {
                            tracing::warn!(
                                "Skipping segment {} in HF export: machine training-ready row is missing multi-model hypothesis coverage, ready agentic promotion coverage, or configured source-reference model coverage/current audio identity",
                                seg.id
                            );
                            continue;
                        }
                        require_segment_audio_identity_hash(
                            db,
                            &seg.id,
                            &current_content_hash,
                            "Hugging Face audio export",
                        )?;

                        // Slice from the already-decoded PCM buffer. An out-of-range/degenerate
                        // alignment window skips the row instead of emitting the whole source file.
                        let pcm_slice = match slice_for_export(&full_pcm, sample_rate, seg.alignment_json.as_deref()) {
                            Some(slice) => slice,
                            None => {
                                tracing::warn!(
                                    "Skipping segment {} in HF export: alignment window out of range (pcm_len={})",
                                    seg.id,
                                    full_pcm.len()
                                );
                                continue;
                            }
                        };

                        let stem = source_path.file_stem().unwrap_or_default().to_string_lossy();
                        let filename = sanitized_clip_filename(&stem, &seg.id);
                        let out_audio_path = dest_dir.join(&filename);

                        write_wav_atomic(&out_audio_path, 16000, pcm_slice.as_ref())?;

                        // Report the duration of the clip ACTUALLY written, not the segment's stored
                        // duration_ms. The two drift when slice_for_export clamps an over-long window to
                        // the decoded length, or falls back to the whole file for a segment with no
                        // alignment — and the metadata must describe the bytes on disk, never a value the
                        // WAV doesn't back up. The clip is mono 16 kHz (see write_wav_atomic below).
                        let clip_dur_ms = (pcm_slice.len() as i64 * 1000) / audio::TARGET_SAMPLE_RATE as i64;
                        let dur_str = clip_dur_ms.to_string();
                        let verified_str = if seg.verified { "1" } else { "0" };
                        let training_ready_str = if grade.training_ready { "1" } else { "0" };
                        let reasons = grade.reasons.join("; ");
                        // Preserve the exact grade-selected Verbatim-Law transcript, then apply the
                        // formula-injection transport guard on every caller-influenced CSV column. The clip
                        // name is included: sanitized_clip_filename() maps '=', '+' and '@' to '_' but
                        // PRESERVES '-', which csv_safe_cell() itself treats as a formula lead, so a
                        // source stem beginning with '-' otherwise reached metadata.csv unguarded.
                        let hf_filename = csv_safe_cell(filename.as_str());
                        let hf_transcript = csv_safe_cell(&grade.transcript);
                        let hf_speaker = csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                        let hf_reasons = csv_safe_cell(reasons.as_str());
                        let review_authority =
                            seg.export_review.as_ref().map(serde_json::to_string).transpose()?.unwrap_or_default();

                        csv_wtr.write_record([
                            hf_filename.as_ref(),
                            hf_transcript.as_ref(),
                            hf_speaker.as_ref(),
                            dur_str.as_str(),
                            verified_str,
                            grade.grade.as_str(),
                            training_ready_str,
                            grade.transcript_source.as_str(),
                            hf_reasons.as_ref(),
                            crate::dialect::dialect_of(&seg.audio_path).unwrap_or(""),
                            speaker_turn_csv(seg),
                            review_authority.as_str(),
                        ])?;

                        total_exported_dur += clip_dur_ms as f64 / 1000.0;
                        count += 1;
                        exported_ids.push(seg.id.clone());
                    }
                }

                // A fresh private split contains only this generation's clips. The old generation
                // remains recoverable until all new data and sidecars are published and verified.
                csv_wtr.flush()?;
                drop(csv_wtr);
                replace_file(&csv_tmp, &csv_path)?;
                Ok((count, total_exported_dur, dropped_unavailable, exported_ids))
            })(),
        )
    };

    // Write every split into the staging tree first. If ANY split fails, discard the staging tree and
    // propagate — data/ has not been touched, so the prior dataset survives a failed re-export intact.
    let staged_splits = (|| -> AppResult<_> {
        let train = process_split(&train_segs, "train", &train_dir)?;
        let val = process_split(&val_segs, "validation", &val_dir)?;
        let test = process_split(&test_segs, "test", &test_dir)?;
        Ok((train, val, test))
    })();
    let (
        (train_count, train_secs, train_dropped, train_ids),
        (val_count, val_secs, val_dropped, val_ids),
        (test_count, test_secs, test_dropped, test_ids),
    ) = staged_splits?;

    let total_count = train_count + val_count + test_count;
    let total_secs = train_secs + val_secs + test_secs;
    let dropped_unavailable = train_dropped + val_dropped + test_dropped;

    // A staged export that wrote ZERO clips must NOT replace an EXISTING prior dataset. has_exportable_row
    // (above) only proves the DB holds a training-ready row — it grades on transcript/verified/metrics and
    // can't see whether the source audio still exists. When every training-ready source is unavailable this
    // run (drive unmounted, recordings folder moved/deleted) or its alignment window is out of range, every
    // row is dropped and total_count is 0. Committing here would remove_dir_all(data_dir) then swap in the
    // empty staging tree — the exact "replace a previously-good export with an empty one" data-loss the
    // no-op guard exists to prevent, in the audio-availability dimension it cannot see. Discard staging and
    // PRESERVE the prior dataset (a later run, once the sources are back, re-exports it normally).
    // Gated on data_dir.exists() so a FIRST-ever export with all sources unavailable still writes an empty,
    // honestly-documented dataset (droppedUnavailableAudio in dataset_infos.json) — there is nothing to lose.
    if total_count == 0 && data_dir.exists() {
        tracing::warn!(
            "HF export: 0 clips written ({dropped_unavailable} training-ready segment(s) had \
             unavailable/undecodable source audio or an out-of-range alignment window) — preserving \
             the prior dataset rather than replacing it with an empty one."
        );
        return Ok(());
    }

    if dropped_unavailable > 0 {
        tracing::warn!(
            "HF export: {dropped_unavailable} segment(s) dropped — source audio unavailable \
             (missing or undecodable). They are NOT in the exported dataset; the count is \
             recorded as droppedUnavailableAudio in dataset_infos.json."
        );
    }

    // Write dataset card (README.md)
    // Provenance: name the ASR model(s) that ACTUALLY produced the WRITTEN rows — the stored per-segment
    // model_version_id — not the export-day settings.asr_model_size. A corpus assembled across model
    // switches lists every distinct model, honestly, instead of one current-setting label (the milder
    // cousin of the H3 export-day-state lie the bundle manifest already closed). Distinct + sorted
    // (BTreeSet) so the card — and its recorded SHA256 — stays byte-reproducible across runs.
    let written_ids: std::collections::BTreeSet<&str> =
        train_ids.iter().chain(&val_ids).chain(&test_ids).map(String::as_str).collect();
    let written_models: std::collections::BTreeSet<&str> = segments
        .iter()
        .filter(|seg| written_ids.contains(seg.id.as_str()))
        .map(|seg| seg.model_version_id.as_deref().unwrap_or("unknown"))
        .collect();
    let model_str = if written_models.is_empty() {
        // No clips written (a first-ever export of an empty/all-filtered library still writes the card).
        "unknown".to_string()
    } else {
        written_models.into_iter().collect::<Vec<_>>().join(", ")
    };
    // Round-24 #5: the HuggingFace size_categories tag must reflect the ACTUAL example count, not a
    // hardcoded `n<1K` that contradicts the split-statistics table once the dataset exceeds 1000 rows.
    let size_category = match total_count {
        0..=999 => "n<1K",
        1_000..=9_999 => "1K<n<10K",
        10_000..=99_999 => "10K<n<100K",
        100_000..=999_999 => "100K<n<1M",
        _ => "n>1M",
    };
    let readme = format!(
        r#"---
language:
- ckb
task_categories:
- automatic-speech-recognition
tags:
- audio
- speech
- kurdish
license: {}
pretty_name: Cortex Kurdish Speech Dataset
size_categories:
- {size_category}
---

# Cortex Kurdish (Sorani) Speech Dataset

This dataset was exported from Cortex Speech Processor.

## Dataset Summary
- **Language**: Central Kurdish (Sorani, ckb)
- **License**: {}
- **Provenance**: Exported via Cortex Speech App v{}, ASR model(s): {} on {}

## Split Statistics
| Split | Examples | Duration (seconds) |
|---|---|---|
| Train | {} | {:.2} |
| Validation | {} | {:.2} |
| Test | {} | {:.2} |
| **Total** | {} | {:.2} |

## Text Normalization Policy
The `transcription` column preserves the exact stored Verbatim-Law authority selected for each row:
human verdict, then human annotation, then champion raw. It is not silently orthographically
normalized or rewritten. Consumers that need normalized Sorani must create and label that derived
view explicitly; the source label and its codepoints remain recoverable unchanged.
{composition_md}"#,
        settings.hf_license,
        settings.hf_license,
        env!("CARGO_PKG_VERSION"),
        model_str,
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        train_count,
        train_secs,
        val_count,
        val_secs,
        test_count,
        test_secs,
        total_count,
        total_secs,
        composition_md = {
            // Composition of the rows ACTUALLY exported (post drop-unavailable), so the card
            // describes the shipped dataset, not the pre-filter library.
            let exported_id_set: std::collections::HashSet<&str> =
                train_ids.iter().chain(val_ids.iter()).chain(test_ids.iter()).map(String::as_str).collect();
            let exported: Vec<SpeechSegment> = train_segs
                .iter()
                .chain(val_segs.iter())
                .chain(test_segs.iter())
                .filter(|s| exported_id_set.contains(s.id.as_str()))
                .cloned()
                .collect();
            composition_markdown(&compute_composition(&exported))
        }
    );
    hook(HuggingFacePublishPoint::BeforeMetadataWrite)?;
    write_text_atomic(&staged_root.join("README.md"), &readme)?;

    // Write dataset_infos.json
    let info = serde_json::json!({
        "cortex-kurdish-split-speech": {
            "description": "Sorani Kurdish speech segments split into train/validation/test with relative paths",
            "features": {
                "file_name": {"dtype": "string", "_type": "Value"},
                "transcription": {"dtype": "string", "_type": "Value"},
                "speaker_id": {"dtype": "string", "_type": "Value"},
                "duration_ms": {"dtype": "int64", "_type": "Value"},
                // Round-24 #4: the metadata.csv writes these as "1"/"0" strings. Declaring them `bool`
                // made a consumer's bool-cast read "0" as truthy True (inverting unverified rows).
                // Declare int64 to match the bytes — "1"/"0" parse cleanly to 1/0, like duration_ms.
                "verified": {"dtype": "int64", "_type": "Value"},
                "training_grade": {"dtype": "string", "_type": "Value"},
                "training_ready": {"dtype": "int64", "_type": "Value"},
                "transcript_source": {"dtype": "string", "_type": "Value"},
                "dialect": {"dtype": "string", "_type": "Value"},
                "speaker_turn": {"dtype": "string", "_type": "Value"},
                "training_reasons": {"dtype": "string", "_type": "Value"},
                "review_authority": {"dtype": "string", "_type": "Value"},
            },
            "splits": {
                "train": {"num_examples": train_count},
                "validation": {"num_examples": val_count},
                "test": {"num_examples": test_count}
            },
            "droppedUnavailableAudio": dropped_unavailable
        }
    });
    write_text_atomic(&staged_root.join("dataset_infos.json"), &serde_json::to_string_pretty(&info)?)?;

    // Integrity manifest, written last so it covers every artifact: a consumer can run
    // `sha256sum -c SHA256SUMS` to detect any corrupted / truncated / partially-copied file.
    write_sha256sums(&staged_root)?;

    // Every on-disk artifact is now written — persist the split columns LAST so that any failure
    // above returned Err with the DB unchanged, never leaving splits that describe an unwritten set.
    // Persist a split ONLY for segments that were actually exported: process_split drops rows whose
    // source audio is missing/undecodable, that are not training-ready, that lack coverage, or whose
    // alignment window is out of range. Recording a split for a dropped row would label a clip the
    // dataset does not contain and disagree with dataset_infos.json's num_examples.
    let exported_ids: std::collections::HashSet<&str> =
        train_ids.iter().chain(val_ids.iter()).chain(test_ids.iter()).map(String::as_str).collect();
    publication.publish(
        |point| {
            hook(point)?;
            if point == HuggingFacePublishPoint::BeforeDataPromotion {
                crate::export_review::verify_current(db, &segments)?;
            }
            Ok(())
        },
        || {
            // Keep the DB write lock short: audio staging and filesystem verification run before
            // this transaction. Ordinary split-write/commit failures roll back both DB rows and
            // the managed files; an abrupt process death may leave DB split hints lagging the
            // sealed export, whose own split metadata is the training consumer's authority.
            let transaction =
                rusqlite::Transaction::new_unchecked(db.connection(), rusqlite::TransactionBehavior::Immediate)?;
            crate::export_review::verify_current(db, &segments)?;
            for (id, split) in &pending_splits {
                if exported_ids.contains(id.as_str()) {
                    db.update_segment_split(id, split).map_err(|error| {
                        AppError::Other(format!("Failed to persist split {split} for {id}: {error}"))
                    })?;
                }
            }
            transaction.commit()?;
            Ok(())
        },
    )
}

fn export_json(path: &std::path::Path, metadata: &DatasetMetadata, segments: &[SpeechSegment]) -> AppResult<()> {
    let records = export_records(segments);
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "metadata": metadata,
        "segments": records,
    }))?;
    // Atomic write: write to .tmp then rename to avoid truncated output on crash.
    let tmp = path.with_extension("json.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            std::fs::write(&tmp, &json)?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn export_jsonl(path: &std::path::Path, segments: &[SpeechSegment]) -> AppResult<()> {
    // Atomic write: accumulate into a temp file, then rename.
    let tmp = path.with_extension("jsonl.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let mut file = std::fs::File::create(&tmp)?;
            for seg in segments {
                let line = serde_json::to_string(&ExportSegmentRecord::new(seg))?;
                writeln!(file, "{line}")?;
            }
            file.flush()?;
            drop(file);
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

/// CWE-1236 mitigation — neutralize CSV / spreadsheet formula injection.
///
/// A CSV cell whose first byte is one of `= + - @` (or a leading TAB/CR, which some
/// spreadsheet apps treat as a formula lead-in) is executed as a live formula when the
/// exported dataset CSV is opened in Excel / LibreOffice Calc / Google Sheets — enabling
/// exfiltration or command execution on the reviewer's machine. Transcript, speaker, and
/// verdict text is human/cloud/third-party-controlled (imported datasets are not
/// content-validated), so prefix any such cell with a single quote: the value is then shown
/// literally and can never execute. Non-triggering cells (the vast majority) are returned
/// borrowed, so there is no allocation on the common path.
///
/// Only free-text columns are routed through this — structural columns (filenames, audio
/// paths, numeric and enum/boolean literals) are left untouched so the dataset stays valid.
pub(crate) fn csv_safe_cell(value: &str) -> Cow<'_, str> {
    match value.as_bytes().first() {
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r') => Cow::Owned(format!("'{value}")),
        _ => Cow::Borrowed(value),
    }
}

fn export_csv(path: &std::path::Path, segments: &[SpeechSegment]) -> AppResult<()> {
    // Atomic write: write CSV to .tmp then rename.
    let tmp = path.with_extension("csv.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let mut wtr = csv::Writer::from_path(&tmp)?;
            wtr.write_record([
                "id",
                "audio_path",
                "raw_transcript",
                "normalized_transcript",
                "annotated_transcript",
                "duration_ms",
                "speaker_id",
                "verified",
                "training_transcript",
                "transcript_source",
                "training_grade",
                "training_ready",
                "training_reasons",
                "dialect",
                "speaker_turn",
                "review_authority",
            ])?;

            for seg in segments {
                let grade = quality::training_grade_for_segment(seg);
                let reasons = grade.reasons.join("; ");
                // Formula-injection guard on EVERY column that can carry attacker-influenced text.
                // audio_path is the imported file's basename and id can come from an imported dataset —
                // both are caller-controlled, and `=SUM(1+1).wav` is a valid filename on Windows and
                // Linux, so leaving them raw let a crafted name execute when the exported dataset.csv is
                // opened in Excel/LibreOffice/Sheets (CWE-1236). The remaining columns are app-generated
                // numerics/enums ("1"/"0", duration, grade), which cannot carry a formula lead.
                let id_cell = csv_safe_cell(seg.id.as_str());
                let audio_ref = csv_safe_cell(export_audio_ref(&seg.audio_path));
                let raw = csv_safe_cell(seg.raw_transcript.as_str());
                let normalized = csv_safe_cell(seg.normalized_transcript.as_deref().unwrap_or(""));
                let annotated = csv_safe_cell(seg.annotated_transcript.as_deref().unwrap_or(""));
                // Preserve the exact grade-selected Verbatim-Law label in the primary training column.
                let training = csv_safe_cell(&grade.transcript);
                let speaker = csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                let reasons_cell = csv_safe_cell(reasons.as_str());
                let review_authority =
                    seg.export_review.as_ref().map(serde_json::to_string).transpose()?.unwrap_or_default();
                wtr.write_record([
                    id_cell.as_ref(),
                    audio_ref.as_ref(),
                    raw.as_ref(),
                    normalized.as_ref(),
                    annotated.as_ref(),
                    &seg.duration_ms.to_string(),
                    speaker.as_ref(),
                    if seg.verified { "1" } else { "0" },
                    training.as_ref(),
                    grade.transcript_source.as_str(),
                    grade.grade.as_str(),
                    if grade.training_ready { "1" } else { "0" },
                    reasons_cell.as_ref(),
                    crate::dialect::dialect_of(&seg.audio_path).unwrap_or(""),
                    speaker_turn_csv(seg),
                    review_authority.as_str(),
                ])?;
            }
            wtr.flush()?;
            drop(wtr);
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn export_records(segments: &[SpeechSegment]) -> Vec<ExportSegmentRecord> {
    segments.iter().map(ExportSegmentRecord::new).collect()
}

pub(crate) fn write_text_atomic(path: &std::path::Path, text: &str) -> AppResult<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|ext| ext.to_str()).unwrap_or("tmp")));
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            std::fs::write(&tmp, text)?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

pub(crate) fn write_wav_atomic(path: &std::path::Path, sample_rate: u32, samples: &[i16]) -> AppResult<()> {
    let tmp = unique_tmp_path(path);
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut wav_writer = hound::WavWriter::create(&tmp, spec)
                .map_err(|e| crate::error::AppError::Other(format!("Failed to create WAV: {e}")))?;
            for &sample in samples {
                wav_writer
                    .write_sample(sample)
                    .map_err(|e| crate::error::AppError::Other(format!("Failed to write sample: {e}")))?;
            }
            wav_writer.finalize().map_err(|e| crate::error::AppError::Other(format!("Failed to finalize WAV: {e}")))?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

fn unique_tmp_path(path: &std::path::Path) -> std::path::PathBuf {
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("export.wav");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!("{file_name}.tmp-{}-{nonce}", std::process::id()))
}

fn export_parquet(path: &std::path::Path, segments: &[SpeechSegment]) -> AppResult<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("audio_path", DataType::Utf8, false),
        Field::new("raw_transcript", DataType::Utf8, false),
        Field::new("normalized_transcript", DataType::Utf8, true),
        Field::new("annotated_transcript", DataType::Utf8, true),
        Field::new("alignment_json", DataType::Utf8, true),
        // Honesty (audit P1 #8): ship the per-word timing PRECISION marker alongside the timestamps, so a
        // consumer/trainer can tell `ctc_forced` (precise forced alignment) from `energy_heuristic`
        // (approximate) and never treat approximate timing as ground truth. JSON/JSONL carry it (flattened
        // SpeechSegment); Parquet was the one format shipping alignment_json while dropping the marker.
        // (CSV is a hand-rolled flat header that ships NO alignment fields at all — honest by omission.)
        Field::new("alignment_quality", DataType::Utf8, true),
        Field::new("duration_ms", DataType::Int64, false),
        Field::new("speaker_id", DataType::Utf8, true),
        Field::new("verified", DataType::Boolean, false),
        Field::new("training_transcript", DataType::Utf8, false),
        Field::new("transcript_source", DataType::Utf8, false),
        Field::new("training_grade", DataType::Utf8, false),
        Field::new("training_ready", DataType::Boolean, false),
        Field::new("training_reasons", DataType::Utf8, false),
        Field::new("dialect", DataType::Utf8, true),
        // Nullable on purpose: an unmeasured clip is NULL, never false — absence of a measurement is
        // not evidence of a single speaker (same honesty rule as the couch badge and the CSV column).
        Field::new("speaker_turn", DataType::Boolean, true),
        Field::new("review_authority", DataType::Utf8, true),
    ]));

    let grade_reports: Vec<TrainingGradeReport> = segments.iter().map(quality::training_grade_for_segment).collect();
    let grade_reasons: Vec<String> = grade_reports.iter().map(|report| report.reasons.join("; ")).collect();
    let ids: StringArray = segments.iter().map(|s| Some(s.id.as_str())).collect();
    let audio_paths: StringArray = segments.iter().map(|s| Some(export_audio_ref(&s.audio_path))).collect();
    let raw: StringArray = segments.iter().map(|s| Some(s.raw_transcript.as_str())).collect();
    let normalized: StringArray = segments.iter().map(|s| s.normalized_transcript.as_deref()).collect();
    let annotated: StringArray = segments.iter().map(|s| s.annotated_transcript.as_deref()).collect();
    let alignment: StringArray = segments.iter().map(|s| s.alignment_json.as_deref()).collect();
    let alignment_quality: StringArray = segments.iter().map(|s| s.alignment_quality.as_deref()).collect();
    let duration_ms: Int64Array = segments.iter().map(|s| Some(s.duration_ms)).collect();
    let speaker_id: StringArray = segments.iter().map(|s| s.speaker_id.as_deref()).collect();
    let verified: BooleanArray = segments.iter().map(|s| Some(s.verified)).collect();
    // Primary labels preserve the exact grade-selected Verbatim-Law authority. A consumer may derive
    // a separately labeled normalized view, but Parquet must not rewrite the source transcript bytes.
    let training_transcript: StringArray =
        grade_reports.iter().map(|report| Some(report.transcript.as_str())).collect();
    let transcript_source: StringArray =
        grade_reports.iter().map(|report| Some(report.transcript_source.as_str())).collect();
    let training_grade: StringArray = grade_reports.iter().map(|report| Some(report.grade.as_str())).collect();
    let training_ready: BooleanArray = grade_reports.iter().map(|report| Some(report.training_ready)).collect();
    let training_reasons: StringArray = grade_reasons.iter().map(|reasons| Some(reasons.as_str())).collect();
    let dialect: StringArray = segments.iter().map(|s| crate::dialect::dialect_of(&s.audio_path)).collect();
    let speaker_turn: BooleanArray = segments
        .iter()
        .map(|s| s.speaker_change_score.map(|v| (v as f32) < crate::diarization::SPEAKER_CHANGE_THRESHOLD))
        .collect();

    let review_authority_values = segments
        .iter()
        .map(|seg| seg.export_review.as_ref().map(serde_json::to_string).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    let review_authority: StringArray = review_authority_values.iter().map(|value| value.as_deref()).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ids),
            Arc::new(audio_paths),
            Arc::new(raw),
            Arc::new(normalized),
            Arc::new(annotated),
            Arc::new(alignment),
            Arc::new(alignment_quality),
            Arc::new(duration_ms),
            Arc::new(speaker_id),
            Arc::new(verified),
            Arc::new(training_transcript),
            Arc::new(transcript_source),
            Arc::new(training_grade),
            Arc::new(training_ready),
            Arc::new(training_reasons),
            Arc::new(dialect),
            Arc::new(speaker_turn),
            Arc::new(review_authority),
        ],
    )
    .map_err(|e| crate::error::AppError::Other(format!("Parquet batch build failed: {e}")))?;

    let tmp = path.with_extension("parquet.tmp");
    remove_file_on_error(
        &tmp,
        (|| -> AppResult<()> {
            let file = std::fs::File::create(&tmp)?;
            let props = WriterProperties::builder().set_compression(Compression::SNAPPY).build();
            let mut writer = ArrowWriter::try_new(file, schema, Some(props))
                .map_err(|e| crate::error::AppError::Other(format!("Parquet writer failed: {e}")))?;
            writer.write(&batch).map_err(|e| crate::error::AppError::Other(format!("Parquet write failed: {e}")))?;
            writer.close().map_err(|e| crate::error::AppError::Other(format!("Parquet close failed: {e}")))?;
            replace_file(&tmp, path)?;
            Ok(())
        })(),
    )
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;

/// Regressions for the two shared-root export rules fixed in this file. A separate module from the
/// `#[path]`-included `export_tests.rs` only so the fix and its gate stay in one file.
#[cfg(test)]
mod shared_exclusion_tests {
    use super::*;

    #[test]
    fn an_is_gold_answer_key_is_excluded_at_the_shared_export_root() {
        // The spot-check answer key was refused by export_audio's own `human_export_label` and by
        // nothing else, so the tabular/HuggingFace/bundle exporters all shipped it. Enforced at the
        // shared root now, which is the only place every exporter routes through.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        let keep = SpeechSegment {
            id: "keep".to_string(),
            audio_path: "/kept.wav".to_string(),
            raw_transcript: "دەقی یەکەم".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        };
        let answer_key = SpeechSegment {
            id: "answer-key".to_string(),
            audio_path: "/key.wav".to_string(),
            raw_transcript: "وەڵامی نهێنی".to_string(),
            duration_ms: 1000,
            is_gold: true,
            ..SpeechSegment::default()
        };

        let kept = exclude_unexportable_segments(&db, vec![keep, answer_key]).unwrap();
        let ids: Vec<&str> = kept.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["keep"], "an is_gold answer key must never leave the app: {ids:?}");
    }

    #[test]
    fn unreadable_source_audio_is_counted_as_would_have_been_written_but_never_written() {
        // The dropped_unavailable tally asks "would this row have been written had the audio been
        // there?". The identity check re-hashes the source file, so on an unmounted drive it refused —
        // and dataset_infos.json then reported droppedUnavailableAudio = 0 while the export path
        // promises the drop "is counted, not silent".
        let missing = SourceTranscriptRecord {
            audio_path: "/definitely/not/mounted/source.wav".to_string(),
            model_id: "reference-model".to_string(),
            audio_content_hash: Some("abc123".to_string()),
            audio_size_bytes: Some(4096),
            transcript_path: "/refs/source.txt".to_string(),
            transcript_text: "دەقی سەرچاوە".to_string(),
            created_at: None,
        };
        assert!(!source_reference_record_matches_current_audio(&missing, false), "the WRITE gate stays fail-closed");
        assert!(
            source_reference_record_matches_current_audio(&missing, true),
            "the tally must count a row only an unreadable source blocked"
        );

        // A record with NO stored identity is refused in BOTH modes: that is a property of the record,
        // not of the audio, so leniency must not smuggle it into the count either.
        let legacy = SourceTranscriptRecord { audio_content_hash: None, audio_size_bytes: None, ..missing };
        assert!(!source_reference_record_matches_current_audio(&legacy, false));
        assert!(!source_reference_record_matches_current_audio(&legacy, true));
    }
}
