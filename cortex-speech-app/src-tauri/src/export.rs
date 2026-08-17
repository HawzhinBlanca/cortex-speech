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
    pub exported_at: String,
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportSegmentRecord {
    #[serde(flatten)]
    segment: SpeechSegment,
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
}

impl ExportSegmentRecord {
    fn new(segment: &SpeechSegment) -> Self {
        let report = quality::training_grade_for_segment(segment);
        let dialect = crate::dialect::dialect_of(&segment.audio_path);
        // Privacy: never publish the curator's absolute filesystem path — it embeds the
        // OS username and drive layout. Emit only the basename, like the HF exporter.
        let mut sanitized = segment.clone();
        sanitized.audio_path = export_audio_ref(&segment.audio_path).to_string();
        Self {
            segment: sanitized,
            // Canonicalize the SHIPPED training text (fold ك/ک, ي/ی, heh forms, ZWNJ/tatweel; digits
            // and diacritics untouched) exactly like the HF exporter, so JSON/JSONL/CSV/Parquet emit
            // byte-identical training_transcript for a segment — mixed orthography must not re-enter the
            // corpus and fragment the CTC label space through the flat exports.
            training_transcript: crate::normalizer::canonical_training_text(&report.transcript),
            transcript_source: report.transcript_source,
            training_grade: report.grade,
            training_ready: report.training_ready,
            training_reasons: report.reasons,
            dialect,
        }
    }
}

/// The published reference for an audio file: just its basename, never the curator's
/// absolute path (which leaks the OS username and directory layout into a shared dataset).
/// `pub(crate)` so the audio-clip exporter (export_audio) reduces its metadata.csv source column
/// the same way the JSON/JSONL/CSV/Parquet/HF exporters here do.
pub(crate) fn export_audio_ref(audio_path: &str) -> &str {
    audio_path.rsplit(['/', '\\']).next().unwrap_or(audio_path)
}

fn is_training_ready_for_huggingface_export(
    db: &Database,
    segment: &SpeechSegment,
    grade: &TrainingGradeReport,
    ready_agentic_segment_ids: &BTreeSet<String>,
    required_source_reference_models: &[String],
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
            || !source_reference_record_matches_current_audio(reference)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn source_reference_record_matches_current_audio(reference: &SourceTranscriptRecord) -> bool {
    let Some(stored_hash) = reference.audio_content_hash.as_deref().map(str::trim).filter(|value| !value.is_empty())
    else {
        return false;
    };
    let Some(stored_size) = reference.audio_size_bytes else {
        return false;
    };
    let Ok(current_identity) = crate::pipeline::source_audio_identity(std::path::Path::new(&reference.audio_path))
    else {
        return false;
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
/// TWO independent exclusions, because both were being missed one caller at a time:
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
/// The name says "unexportable", not "holdout", so it cannot quietly under-describe what it drops
/// the next time a reason is added.
pub(crate) fn exclude_unexportable_segments(
    db: &Database,
    segments: Vec<SpeechSegment>,
) -> AppResult<Vec<SpeechSegment>> {
    let mut kept = Vec::with_capacity(segments.len());
    for seg in segments {
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
        if crate::quality::is_effective_placeholder(&seg) {
            tracing::info!(segment_id = %seg.id, "export: dropping placeholder-only segment");
            continue;
        }
        kept.push(seg);
    }
    let segments = kept;
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
    // Telemetry (Week-1 "measure first"): real export wall-clock. The guard records on return; metadata
    // carries the format so the owner can compare JSON/JSONL/CSV/Parquet costs via get_recent_spans /
    // get_tracing_stats. Mirrors the asr.transcribe / decode / normalizer guards.
    let _span = crate::telemetry::TRACER
        .start_span("export.dataset", crate::telemetry::Tracer::metadata(vec![("format", format!("{format:?}"))]));
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
    let total_duration: i64 = segments.iter().map(|s| s.duration_ms).sum();
    let verified = segments.iter().filter(|s| s.verified).count();

    let metadata = DatasetMetadata {
        name: "cortex-kurdish-speech-dataset".into(),
        version: "2.0".into(),
        language: "ckb".into(),
        script: "Arabic".into(),
        total_segments: segments.len(),
        total_duration_ms: total_duration,
        verified_segments: verified,
        training_grade_summary: quality::training_grade_summary(&segments),
        composition: compute_composition(&segments),
        exported_at: chrono::Utc::now().to_rfc3339(),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match format {
        ExportFormat::Json => export_json(path, &metadata, &segments),
        ExportFormat::Jsonl => export_jsonl(path, &segments),
        ExportFormat::Csv => export_csv(path, &segments),
        ExportFormat::Parquet => export_parquet(path, &segments),
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
/// whole directory. `write_sha256sums`'s whole-dir scan is correct for exporters that stage into a CLEAN
/// dir, but the audio export writes into a caller-chosen dir it does not stage: a re-export of a smaller
/// selection leaves ORPHAN clips from a prior run, and a whole-dir manifest would then vouch for a stale
/// clip that this export's metadata.csv omits — an integrity manifest asserting a file the dataset itself
/// does not list (sibling of the bundle orphan-source fix). Pass the export's own file list so the
/// manifest describes exactly this dataset. Missing entries are skipped; `SHA256SUMS` is never self-listed.
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
    // Telemetry (Week-1 "measure first"): real HuggingFace-export wall-clock (audio copy + shard writes).
    let _span = crate::telemetry::TRACER.start_span("export.huggingface", crate::telemetry::Tracer::metadata(vec![]));
    std::fs::create_dir_all(dir)?;
    let data_dir = dir.join("data");
    // Splits are STAGED in a sibling directory and swapped in only once every split has written
    // successfully. Rebuilding in place (remove_dir_all(data) up front, then write) meant ANY mid-export
    // failure — a full disk, a failed rename, an unwritable clip name — destroyed the prior good dataset
    // and left the replacement partial, with nothing to recover.
    let staging_dir = dir.join(".data-staging");
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

    // There IS something to export. Stage into a FRESH tree: a leftover staging dir from a crashed run
    // is discarded first, and because every clip lands in an empty tree there are no orphan WAVs and no
    // stale metadata.csv for a now-empty split to prune — the round-12 hazard (an orphan getting hashed
    // into SHA256SUMS and disagreeing with metadata.csv / dataset_infos.json) is now structurally
    // impossible rather than cleaned up after the fact. data/ is left untouched until the swap below.
    if staging_dir.exists() {
        std::fs::remove_dir_all(&staging_dir)?;
    }
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
                // lost "training-ready" segments in the operator warnings. is_training_ready_for_huggingface_export
                // reads only the grade + DB records, never the audio, so it is valid for an unavailable source.
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
                        )? {
                            tracing::warn!(
                                    "Skipping segment {} in HF export: machine training-ready row is missing multi-model hypothesis coverage, ready agentic promotion coverage, or configured source-reference model coverage/current audio identity",
                                    seg.id
                                );
                            continue;
                        }

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
                        // Canonical Sorani orthography for the shipped transcription (ك/ک, ي/ی, ه/ھ
                        // unified — mixed codepoint variants inflate the CTC label space downstream),
                        // then the formula-injection guard on every caller-influenced column. The clip
                        // name is included: sanitized_clip_filename() maps '=', '+' and '@' to '_' but
                        // PRESERVES '-', which csv_safe_cell() itself treats as a formula lead, so a
                        // source stem beginning with '-' otherwise reached metadata.csv unguarded.
                        let canonical_transcript = crate::normalizer::canonical_training_text(&grade.transcript);
                        let hf_filename = csv_safe_cell(filename.as_str());
                        let hf_transcript = csv_safe_cell(&canonical_transcript);
                        let hf_speaker = csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                        let hf_reasons = csv_safe_cell(reasons.as_str());

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
                        ])?;

                        total_exported_dur += clip_dur_ms as f64 / 1000.0;
                        count += 1;
                        exported_ids.push(seg.id.clone());
                    }
                }

                // No orphan-prune here: each run stages into a FRESH tree (staging_dir is wiped + recreated
                // at the top of the export), so this split dir only ever contains the clips written just
                // above — there is never a stale clip from a prior, larger export to prune. Old clips are
                // removed wholesale by the atomic data/ ← staging swap at commit, so the on-disk *.wav set
                // always matches this run's metadata.csv (and SHA256SUMS) by construction.
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
    ) = match staged_splits {
        Ok(splits) => splits,
        Err(error) => {
            if let Err(cleanup) = std::fs::remove_dir_all(&staging_dir) {
                tracing::warn!("HF export: failed to remove staging dir after an export error: {cleanup}");
            }
            return Err(error);
        }
    };

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
        if let Err(cleanup) = std::fs::remove_dir_all(&staging_dir) {
            tracing::warn!("HF export: failed to remove staging dir after a zero-clip run: {cleanup}");
        }
        tracing::warn!(
            "HF export: 0 clips written ({dropped_unavailable} training-ready segment(s) had \
             unavailable/undecodable source audio or an out-of-range alignment window) — preserving \
             the prior dataset rather than replacing it with an empty one."
        );
        return Ok(());
    }

    // COMMIT: every split wrote successfully and at least one clip exists, so replace the previous dataset
    // now. The rename is atomic within the directory; if the remove succeeds but the rename fails, the
    // fully-written new dataset is still on disk at .data-staging and is recoverable by hand (the prior
    // in-place rebuild left nothing at all).
    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir)?;
    }
    std::fs::rename(&staging_dir, &data_dir)?;

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
The `transcription` column is orthographically canonicalized for Sorani: Arabic-script codepoint
variants are unified (Kaf ك→ک, Yeh ي→ی, Heh→ھ/ە forms; tatweel/ZWNJ folded) so each grapheme has
one form across human-typed and ASR-produced text. Digits are preserved exactly as written (no
number verbalization), and diacritics are left untouched.
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
    write_text_atomic(&dir.join("README.md"), &readme)?;

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
                "training_reasons": {"dtype": "string", "_type": "Value"},
            },
            "splits": {
                "train": {"num_examples": train_count},
                "validation": {"num_examples": val_count},
                "test": {"num_examples": test_count}
            },
            "droppedUnavailableAudio": dropped_unavailable
        }
    });
    write_text_atomic(&dir.join("dataset_infos.json"), &serde_json::to_string_pretty(&info)?)?;

    // Integrity manifest, written last so it covers every artifact: a consumer can run
    // `sha256sum -c SHA256SUMS` to detect any corrupted / truncated / partially-copied file.
    write_sha256sums(dir)?;

    // Every on-disk artifact is now written — persist the split columns LAST so that any failure
    // above returned Err with the DB unchanged, never leaving splits that describe an unwritten set.
    // Persist a split ONLY for segments that were actually exported: process_split drops rows whose
    // source audio is missing/undecodable, that are not training-ready, that lack coverage, or whose
    // alignment window is out of range. Recording a split for a dropped row would label a clip the
    // dataset does not contain and disagree with dataset_infos.json's num_examples.
    let exported_ids: std::collections::HashSet<&str> =
        train_ids.iter().chain(val_ids.iter()).chain(test_ids.iter()).map(String::as_str).collect();
    for (id, split) in &pending_splits {
        if !exported_ids.contains(id.as_str()) {
            continue;
        }
        db.update_segment_split(id, split)
            .map_err(|error| AppError::Other(format!("Failed to persist split {split} for {id}: {error}")))?;
    }

    Ok(())
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
                // Canonicalize the shipped training column like the HF exporter (see ExportSegmentRecord).
                let training_text = crate::normalizer::canonical_training_text(&grade.transcript);
                let training = csv_safe_cell(&training_text);
                let speaker = csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                let reasons_cell = csv_safe_cell(reasons.as_str());
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
    // Canonicalize the shipped training column like the HF/JSON/CSV exporters so all four formats emit
    // byte-identical training text for a segment (no mixed-orthography re-entry via Parquet).
    let training_texts: Vec<String> =
        grade_reports.iter().map(|report| crate::normalizer::canonical_training_text(&report.transcript)).collect();
    let training_transcript: StringArray = training_texts.iter().map(|t| Some(t.as_str())).collect();
    let transcript_source: StringArray =
        grade_reports.iter().map(|report| Some(report.transcript_source.as_str())).collect();
    let training_grade: StringArray = grade_reports.iter().map(|report| Some(report.grade.as_str())).collect();
    let training_ready: BooleanArray = grade_reports.iter().map(|report| Some(report.training_ready)).collect();
    let training_reasons: StringArray = grade_reasons.iter().map(|reasons| Some(reasons.as_str())).collect();
    let dialect: StringArray = segments.iter().map(|s| crate::dialect::dialect_of(&s.audio_path)).collect();

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
