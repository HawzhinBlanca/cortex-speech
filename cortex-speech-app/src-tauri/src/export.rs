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
    pub exported_at: String,
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
}

impl ExportSegmentRecord {
    fn new(segment: &SpeechSegment) -> Self {
        let report = quality::training_grade_for_segment(segment);
        // Privacy: never publish the curator's absolute filesystem path — it embeds the
        // OS username and drive layout. Emit only the basename, like the HF exporter.
        let mut sanitized = segment.clone();
        sanitized.audio_path = export_audio_ref(&segment.audio_path).to_string();
        Self {
            segment: sanitized,
            training_transcript: report.transcript,
            transcript_source: report.transcript_source,
            training_grade: report.grade,
            training_ready: report.training_ready,
            training_reasons: report.reasons,
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

/// Remove any segment that matches a held-out gold clip (by audio_path OR content hash) from an
/// export set, so a TRAINING export can't leak the eval set's reference transcripts. EVERY
/// training-corpus export must run this — the plain JSON/JSONL/CSV/Parquet export and the production
/// bundle that wraps it, not just the HuggingFace export — or a clip registered as a holdout (the
/// WER/CER eval reference) that also exists as an ordinary training-ready segment leaks into the
/// published training data and contaminates the very eval set the promotion gate measures against.
/// Fail-closed, identical to the HF/DPO guard: a path match excludes a held-out clip even when its
/// file is missing (its content can no longer be re-hashed); a hash match also catches the same
/// content at any path.
pub(crate) fn exclude_holdout_segments(db: &Database, segments: Vec<SpeechSegment>) -> AppResult<Vec<SpeechSegment>> {
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
                    path.exists()
                        && crate::pipeline::source_audio_identity(path)
                            .map(|id| holdout.contains(&id.content_hash))
                            .unwrap_or(false)
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
    // Drop held-out gold segments BEFORE counting or writing any format, so the training tables
    // (JSON/JSONL/CSV/Parquet) — including the production bundle that delegates through here — never
    // publish the eval set's reference transcripts; closes the eval-on-train leak the HF export
    // already guards against.
    let segments = exclude_holdout_segments(db, db.get_segments(None)?)?;
    // A human-REJECTED clip ("mark bad" in review) is kept in the library but is bad data the reviewer
    // discarded — never publish it, and never count it as verified. The HuggingFace/training path
    // already drops it via training_grade_for_segment; do the same here so the plain JSON/JSONL/CSV/
    // Parquet tables and `verified_segments` can never label a reject as confirmed-good output.
    let segments: Vec<SpeechSegment> = segments.into_iter().filter(|s| !quality::is_human_rejected(s)).collect();
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
                let s = uf.node(&format!("s:{spk}"));
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
                let start = chunking::ms_to_samples(meta.source_start_ms.max(0) as u32, sample_rate);
                let end = chunking::ms_to_samples(meta.source_end_ms.max(0) as u32, sample_rate).min(full_pcm.len());
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
fn write_sha256sums(dir: &std::path::Path) -> AppResult<()> {
    fn collect(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) -> AppResult<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                collect(&path, root, out)?;
            } else if ft.is_file() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "SHA256SUMS" || name.ends_with(".tmp") {
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
    std::fs::create_dir_all(dir)?;
    let data_dir = dir.join("data");
    let train_dir = data_dir.join("train");
    let val_dir = data_dir.join("validation");
    let test_dir = data_dir.join("test");

    // Exclude held-out gold audio from the TRAINING export — the same fail-closed content-hash guard
    // the plain export, DPO, and LM-corpus exports use. Without it, a clip registered as a holdout
    // (for WER/CER eval) that ALSO exists as a normal training-ready segment leaks into data/train,
    // contaminating the very eval set the promotion gate measures against.
    let segments = exclude_holdout_segments(db, db.get_segments(None)?)?;
    if segments.is_empty() {
        // Nothing to export — a true NO-OP that PRESERVES any prior export rather than wiping it
        // (re-exporting with zero training-ready segments must not destroy a previous good dataset).
        return Ok(());
    }

    // There IS something to export: clear any prior export's split artifacts before re-writing, so a
    // re-export into the same directory never leaves orphan WAVs (a segment that no longer exports) or a
    // stale metadata.csv for a now-empty split — both would be hashed into SHA256SUMS and disagree with
    // the freshly written metadata.csv / dataset_infos.json (round-12 audit).
    if data_dir.exists() {
        std::fs::remove_dir_all(&data_dir)?;
    }
    std::fs::create_dir_all(&train_dir)?;
    std::fs::create_dir_all(&val_dir)?;
    std::fs::create_dir_all(&test_dir)?;

    let ready_agentic_segment_ids = ready_agentic_huggingface_segment_ids(db)?;
    let required_source_reference_models = settings.source_reference_models();

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

    // Helper closure to process and write a split's files
    let process_split = |split_segs: &[SpeechSegment],
                         _split_name: &str,
                         dest_dir: &std::path::Path|
     -> AppResult<(usize, f64, usize, Vec<String>)> {
        // NOTE: do NOT early-return when this split is empty on a re-export. Falling through writes a
        // HEADER-ONLY metadata.csv (the per-source loop below simply runs zero times) and runs the
        // orphan-prune below, which clears clips a prior, larger export left in this split dir. So an
        // empty split's on-disk metadata agrees with dataset_infos.json's num_examples=0 — never a
        // stale or missing file. Returning early left that stale metadata.csv + orphaned clips on disk
        // (and cemented into SHA256SUMS), disagreeing with the actual (now-empty) split.
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
                ])?;

                let mut total_exported_dur = 0.0;
                let mut count = 0;
                // Filenames written this run; used to prune orphaned clips left by a prior, larger export.
                let mut written_clips: std::collections::HashSet<String> = std::collections::HashSet::new();
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

                for (source_path_str, segs) in segs_by_source {
                    let source_path = std::path::Path::new(source_path_str);
                    // Preserve any clips a PRIOR successful export wrote for a source that is only
                    // TRANSIENTLY unavailable this run (unmounted/network drive, momentary decode
                    // error). Their expected filenames go into the keep-set so the orphan-prune
                    // below does NOT delete them — pruning them would be permanent training-data
                    // loss from a recoverable error. They stay on disk but are (correctly) absent
                    // from this run's metadata.csv until a later run can re-read the source.
                    let source_stem = source_path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    if !source_path.exists() {
                        for seg in &segs {
                            tracing::warn!("Skipping segment {} in HF export: audio not found", seg.id);
                            written_clips.insert(sanitized_clip_filename(&source_stem, &seg.id));
                        }
                        dropped_unavailable += segs.len();
                        continue;
                    }

                    // Decode the source file exactly once.
                    let (sample_rate, full_pcm) = match audio::decode_to_pcm(source_path_str) {
                        Ok(res) => res,
                        Err(e) => {
                            tracing::error!("Failed to decode {source_path_str} in HF export: {e}");
                            for seg in &segs {
                                written_clips.insert(sanitized_clip_filename(&source_stem, &seg.id));
                            }
                            dropped_unavailable += segs.len();
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
                        written_clips.insert(filename.clone());

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
                        // then the formula-injection guard on the free-text columns only.
                        let canonical_transcript = crate::normalizer::canonical_training_text(&grade.transcript);
                        let hf_transcript = csv_safe_cell(&canonical_transcript);
                        let hf_speaker = csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                        let hf_reasons = csv_safe_cell(reasons.as_str());

                        csv_wtr.write_record([
                            filename.as_str(),
                            hf_transcript.as_ref(),
                            hf_speaker.as_ref(),
                            dur_str.as_str(),
                            verified_str,
                            grade.grade.as_str(),
                            training_ready_str,
                            grade.transcript_source.as_str(),
                            hf_reasons.as_ref(),
                        ])?;

                        total_exported_dur += clip_dur_ms as f64 / 1000.0;
                        count += 1;
                        exported_ids.push(seg.id.clone());
                    }
                }

                // Prune orphaned clips from a previous, larger export so the on-disk *.wav set
                // matches the freshly-written metadata.csv exactly (stale clips otherwise disagree
                // with the manifest and get cemented into SHA256SUMS). Runs only after every current
                // clip wrote successfully, so a mid-export failure never half-empties the directory.
                if let Ok(entries) = std::fs::read_dir(dest_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let is_wav =
                            path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wav"));
                        let keep = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| written_clips.contains(n));
                        if is_wav && !keep {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }

                csv_wtr.flush()?;
                drop(csv_wtr);
                replace_file(&csv_tmp, &csv_path)?;
                Ok((count, total_exported_dur, dropped_unavailable, exported_ids))
            })(),
        )
    };

    let (train_count, train_secs, train_dropped, train_ids) = process_split(&train_segs, "train", &train_dir)?;
    let (val_count, val_secs, val_dropped, val_ids) = process_split(&val_segs, "validation", &val_dir)?;
    let (test_count, test_secs, test_dropped, test_ids) = process_split(&test_segs, "test", &test_dir)?;

    let total_count = train_count + val_count + test_count;
    let total_secs = train_secs + val_secs + test_secs;
    let dropped_unavailable = train_dropped + val_dropped + test_dropped;
    if dropped_unavailable > 0 {
        tracing::warn!(
            "HF export: {dropped_unavailable} segment(s) dropped — source audio unavailable \
             (missing or undecodable). They are NOT in the exported dataset; the count is \
             recorded as droppedUnavailableAudio in dataset_infos.json."
        );
    }

    // Write dataset card (README.md)
    let model_str = format!("{:?}", settings.asr_model_size);
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
- **Provenance**: Exported via Cortex Speech App v{}, using ASR Model {} on {}

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
"#,
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
        total_secs
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
            ])?;

            for seg in segments {
                let grade = quality::training_grade_for_segment(seg);
                let reasons = grade.reasons.join("; ");
                // Formula-injection guard on the free-text columns only.
                let raw = csv_safe_cell(seg.raw_transcript.as_str());
                let normalized = csv_safe_cell(seg.normalized_transcript.as_deref().unwrap_or(""));
                let annotated = csv_safe_cell(seg.annotated_transcript.as_deref().unwrap_or(""));
                let training = csv_safe_cell(grade.transcript.as_str());
                let speaker = csv_safe_cell(seg.speaker_id.as_deref().unwrap_or(""));
                let reasons_cell = csv_safe_cell(reasons.as_str());
                wtr.write_record([
                    seg.id.as_str(),
                    export_audio_ref(&seg.audio_path),
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

fn write_text_atomic(path: &std::path::Path, text: &str) -> AppResult<()> {
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
        Field::new("duration_ms", DataType::Int64, false),
        Field::new("speaker_id", DataType::Utf8, true),
        Field::new("verified", DataType::Boolean, false),
        Field::new("training_transcript", DataType::Utf8, false),
        Field::new("transcript_source", DataType::Utf8, false),
        Field::new("training_grade", DataType::Utf8, false),
        Field::new("training_ready", DataType::Boolean, false),
        Field::new("training_reasons", DataType::Utf8, false),
    ]));

    let grade_reports: Vec<TrainingGradeReport> = segments.iter().map(quality::training_grade_for_segment).collect();
    let grade_reasons: Vec<String> = grade_reports.iter().map(|report| report.reasons.join("; ")).collect();
    let ids: StringArray = segments.iter().map(|s| Some(s.id.as_str())).collect();
    let audio_paths: StringArray = segments.iter().map(|s| Some(export_audio_ref(&s.audio_path))).collect();
    let raw: StringArray = segments.iter().map(|s| Some(s.raw_transcript.as_str())).collect();
    let normalized: StringArray = segments.iter().map(|s| s.normalized_transcript.as_deref()).collect();
    let annotated: StringArray = segments.iter().map(|s| s.annotated_transcript.as_deref()).collect();
    let alignment: StringArray = segments.iter().map(|s| s.alignment_json.as_deref()).collect();
    let duration_ms: Int64Array = segments.iter().map(|s| Some(s.duration_ms)).collect();
    let speaker_id: StringArray = segments.iter().map(|s| s.speaker_id.as_deref()).collect();
    let verified: BooleanArray = segments.iter().map(|s| Some(s.verified)).collect();
    let training_transcript: StringArray =
        grade_reports.iter().map(|report| Some(report.transcript.as_str())).collect();
    let transcript_source: StringArray =
        grade_reports.iter().map(|report| Some(report.transcript_source.as_str())).collect();
    let training_grade: StringArray = grade_reports.iter().map(|report| Some(report.grade.as_str())).collect();
    let training_ready: BooleanArray = grade_reports.iter().map(|report| Some(report.training_ready)).collect();
    let training_reasons: StringArray = grade_reasons.iter().map(|reasons| Some(reasons.as_str())).collect();

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(ids),
            Arc::new(audio_paths),
            Arc::new(raw),
            Arc::new(normalized),
            Arc::new(annotated),
            Arc::new(alignment),
            Arc::new(duration_ms),
            Arc::new(speaker_id),
            Arc::new(verified),
            Arc::new(training_transcript),
            Arc::new(transcript_source),
            Arc::new(training_grade),
            Arc::new(training_ready),
            Arc::new(training_reasons),
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
mod tests {
    use super::*;
    use crate::db::{Database, SegmentHypothesis, SourceTranscriptRecord};
    use tempfile::NamedTempFile;

    #[test]
    fn sanitized_clip_filename_blocks_path_traversal() {
        // A crafted stem/id from an imported dataset must never produce a separator or `..` escape:
        // the result is always a single, join-safe component (CWE-22 regression).
        for (stem, id) in [
            ("../../etc/pass", "../../../root/id"),
            ("a/b\\c", ".."),
            ("normal", "..\\..\\win"),
            ("clip\0", "seg/../../x"),
        ] {
            let f = sanitized_clip_filename(stem, id);
            assert!(!f.contains('/') && !f.contains('\\'), "no separators: {f:?}");
            assert!(!f.contains(".."), "no parent-dir tokens: {f:?}");
            assert_eq!(std::path::Path::new(&f).components().count(), 1, "must be a single path component: {f:?}");
            assert!(std::path::Path::new("/export/dir").join(&f).starts_with("/export/dir"), "stays under dir: {f:?}");
        }
        // Normal stems/ids pass through unchanged (already filename-safe -> no disambiguating hash).
        assert_eq!(sanitized_clip_filename("clip01", "seg_42"), "clip01_seg_42.wav");
        // A v4-UUID id is filename-safe, so it passes through verbatim (no hash suffix, no collision).
        assert_eq!(
            sanitized_clip_filename("rec", "550e8400-e29b-41d4-a716-446655440000"),
            "rec_550e8400-e29b-41d4-a716-446655440000.wav"
        );
        // Two ids that collapse to the same cleaned form must NOT collide: the raw-id hash disambiguates.
        let a = sanitized_clip_filename("rec", "a/b");
        let b = sanitized_clip_filename("rec", "a.b");
        assert_ne!(a, b, "distinct ids that clean to the same value must not share a filename");
        assert!(a.starts_with("rec_a_b_") && b.starts_with("rec_a_b_"), "{a:?} {b:?}");
    }

    fn insert_machine_silver_segment_with_hf_coverage(
        db: &Database,
        wav_path: &std::path::Path,
        id: &str,
    ) -> SpeechSegment {
        let mut segment = sample_segment(id);
        segment.audio_path = wav_path.to_string_lossy().to_string();
        segment.verified = false;
        segment.annotated_transcript = None;
        segment.confidence = Some(0.95);
        segment.clipping_ratio = Some(0.0);
        segment.rms_db = Some(-20.0);
        segment.snr_db = Some(20.0);
        db.insert_segment(&segment).unwrap();
        let evidence_json = serde_json::json!({
            "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
            "selectedModelId": "omniasr-wsl-7b",
            "selectedTranscript": "reference candidate",
            "shouldCommit": true
        })
        .to_string();
        db.write_segment_verdict(
            &segment.id,
            "jury_accept",
            Some("reference candidate"),
            Some("multi-reference consensus committed agent text"),
            Some(evidence_json.as_str()),
            Some(0.92),
            false,
        )
        .unwrap();
        for model_id in ["omniasr-wsl-7b", "omniasr-ctc-300m"] {
            db.insert_hypothesis(&SegmentHypothesis {
                segment_id: segment.id.clone(),
                model_id: model_id.to_string(),
                transcript: "reference candidate".to_string(),
                confidence: Some(0.95),
            })
            .unwrap();
        }
        segment
    }

    fn record_ready_agent_report(db: &Database, audio_path: &str, segment_id: &str, run_id: &str) {
        crate::runs::record_agent_import_report_with_options(
            db,
            "file",
            &[audio_path.to_string()],
            &[segment_id.to_string()],
            Some(&serde_json::json!({
                "referenceCommitted": 1,
                "humanInbox": 0,
            })),
            None,
            crate::runs::AgentImportReportOptions {
                agent_run_id: Some(run_id.to_string()),
                agentic_readiness: Some(serde_json::json!({
                    "status": "ready",
                    "ready": true,
                    "sourceReferenceModels": ["gemini-2.5-pro", "gemini-2.5-flash"],
                    "availableHypothesisModels": ["omniasr-wsl-7b", "omniasr-ctc-300m"],
                    "requiredHypothesisModels": 2,
                    "checks": [{
                        "id": "hypothesis_coverage",
                        "label": "Multi-model hypothesis coverage",
                        "status": "ready",
                        "detail": "Two hypothesis models are ready"
                    }]
                })),
                ..crate::runs::AgentImportReportOptions::default()
            },
        )
        .unwrap();
    }

    fn insert_source_reference_with_identity(db: &Database, audio_path: &str, model_id: &str) {
        let identity = crate::pipeline::source_audio_identity(std::path::Path::new(audio_path)).unwrap();
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.to_string(),
            model_id: model_id.to_string(),
            audio_content_hash: Some(identity.content_hash),
            audio_size_bytes: Some(identity.size_bytes),
            transcript_path: format!("source_transcripts\\{model_id}.whole_file_reference.txt"),
            transcript_text: "whole file reference transcript".to_string(),
            created_at: None,
        })
        .unwrap();
    }

    fn insert_source_reference_without_identity(db: &Database, audio_path: &str, model_id: &str) {
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.to_string(),
            model_id: model_id.to_string(),
            audio_content_hash: None,
            audio_size_bytes: None,
            transcript_path: format!("source_transcripts\\{model_id}.whole_file_reference.txt"),
            transcript_text: "whole file reference transcript".to_string(),
            created_at: None,
        })
        .unwrap();
    }

    fn insert_stale_source_reference_identity(db: &Database, audio_path: &str, model_id: &str) {
        db.upsert_source_transcript(&SourceTranscriptRecord {
            audio_path: audio_path.to_string(),
            model_id: model_id.to_string(),
            audio_content_hash: Some("stale-audio-hash".to_string()),
            audio_size_bytes: Some(1),
            transcript_path: format!("source_transcripts\\{model_id}.whole_file_reference.txt"),
            transcript_text: "whole file reference transcript".to_string(),
            created_at: None,
        })
        .unwrap();
    }

    fn all_huggingface_metadata(out_dir: &std::path::Path) -> String {
        let train_metadata = std::fs::read_to_string(out_dir.join("data/train/metadata.csv")).unwrap_or_default();
        let validation_metadata =
            std::fs::read_to_string(out_dir.join("data/validation/metadata.csv")).unwrap_or_default();
        let test_metadata = std::fs::read_to_string(out_dir.join("data/test/metadata.csv")).unwrap_or_default();
        format!("{train_metadata}\n{validation_metadata}\n{test_metadata}")
    }

    fn sample_segment(id: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            created_at: None,
            audio_path: format!("/audio/{id}.wav"),
            raw_transcript: "سڵاو".to_string(),
            normalized_transcript: Some("سلاو".to_string()),
            annotated_transcript: None,
            alignment_json: None,
            duration_ms: 1200,
            speaker_id: Some("speaker_a".to_string()),
            verified: true,
            confidence: None,
            ctc_score: None,
            clipping_ratio: None,
            rms_db: None,
            snr_db: None,
            split: None,
            ood_score: None,
            ..SpeechSegment::default()
        }
    }

    fn sample_metadata() -> DatasetMetadata {
        DatasetMetadata {
            name: "test-dataset".to_string(),
            version: "1.0".to_string(),
            language: "ckb".to_string(),
            script: "Arabic".to_string(),
            total_segments: 1,
            total_duration_ms: 1200,
            verified_segments: 1,
            training_grade_summary: TrainingGradeSummary {
                total_segments: 1,
                training_ready_segments: 1,
                gold_segments: 1,
                silver_segments: 0,
                review_segments: 0,
                rejected_segments: 0,
            },
            exported_at: "2026-06-16T00:00:00Z".to_string(),
        }
    }

    fn write_silent_wav(path: &std::path::Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..16000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn assign_splits_reproducible_and_no_recording_leakage() {
        use std::collections::HashMap;
        let mk = |id: &str, src: &str, spk: Option<&str>, dur: i64| SpeechSegment {
            id: id.to_string(),
            audio_path: format!("/data/{src}"),
            speaker_id: spk.map(str::to_string),
            duration_ms: dur,
            ..SpeechSegment::default()
        };
        let mut segs = Vec::new();
        for (src, spk) in [
            ("recA.wav", Some("S1")),
            ("recB.wav", Some("S2")),
            ("recC.wav", None),
            ("recD.wav", Some("S1")), // same speaker as recA — disjoint must keep S1 together
        ] {
            for i in 0..6 {
                segs.push(mk(&format!("{src}-{i}"), src, spk, 5000));
            }
        }

        // 1. Reproducible: same segments + seed → identical assignment, every run.
        let a = assign_splits(&segs, 0.8, 0.1, 0.1, 7, false);
        let b = assign_splits(&segs, 0.8, 0.1, 0.1, 7, false);
        assert_eq!(a, b, "same seed must yield the same split");

        // 2. No source recording leaks across splits (non-disjoint groups by recording).
        let split_of: HashMap<&str, &str> = a.iter().map(|(id, s)| (id.as_str(), *s)).collect();
        let mut rec_split: HashMap<&str, &str> = HashMap::new();
        for s in &segs {
            let src = s.audio_path.rsplit(['/', '\\']).next().unwrap();
            let split = split_of[s.id.as_str()];
            if let Some(prev) = rec_split.insert(src, split) {
                assert_eq!(prev, split, "recording {src} leaked across splits");
            }
        }

        // 3. Speaker-disjoint: a known speaker never spans two splits (S1 is in recA + recD).
        let d = assign_splits(&segs, 0.8, 0.1, 0.1, 7, true);
        let dsplit: HashMap<&str, &str> = d.iter().map(|(id, s)| (id.as_str(), *s)).collect();
        let mut spk_split: HashMap<&str, &str> = HashMap::new();
        for s in &segs {
            if let Some(spk) = s.speaker_id.as_deref() {
                let split = dsplit[s.id.as_str()];
                if let Some(prev) = spk_split.insert(spk, split) {
                    assert_eq!(prev, split, "speaker {spk} leaked across splits");
                }
            }
        }
    }

    #[test]
    fn multi_speaker_recording_stays_in_one_split_under_speaker_disjoint() {
        use std::collections::{HashMap, HashSet};
        // Round-10 audit HIGH: in speaker-disjoint mode the OLD grouping keyed purely on speaker, so a
        // single recording diarized into two speakers could land its chunks in DIFFERENT splits — the
        // same room/mic acoustic content leaking train<->test. The connected-components grouping must
        // keep every recording intact AND stay speaker-disjoint.
        let mk = |id: &str, src: &str, spk: &str, dur: i64| SpeechSegment {
            id: id.to_string(),
            audio_path: format!("/data/{src}"),
            speaker_id: Some(spk.to_string()),
            duration_ms: dur,
            ..SpeechSegment::default()
        };
        let mut segs = Vec::new();
        // One recording diarized into TWO speakers (an interview), plus two single-speaker recordings.
        for i in 0..4 {
            segs.push(mk(&format!("interview-A{i}"), "interview.wav", "SPEAKER_00", 5000));
        }
        for i in 0..4 {
            segs.push(mk(&format!("interview-B{i}"), "interview.wav", "SPEAKER_01", 5000));
        }
        for i in 0..6 {
            segs.push(mk(&format!("solo1-{i}"), "solo1.wav", "SPEAKER_02", 5000));
        }
        for i in 0..6 {
            segs.push(mk(&format!("solo2-{i}"), "solo2.wav", "SPEAKER_03", 5000));
        }

        let a = assign_splits(&segs, 0.34, 0.33, 0.33, 11, true);
        let split_of: HashMap<&str, &str> = a.iter().map(|(id, s)| (id.as_str(), *s)).collect();

        // The multi-speaker recording must be entirely within ONE split (no recording leakage).
        let interview_splits: HashSet<&str> =
            segs.iter().filter(|s| s.audio_path.ends_with("interview.wav")).map(|s| split_of[s.id.as_str()]).collect();
        assert_eq!(
            interview_splits.len(),
            1,
            "a multi-speaker recording must not straddle splits: {interview_splits:?}"
        );

        // And speaker-disjointness still holds: no speaker spans two splits.
        let mut spk_split: HashMap<&str, &str> = HashMap::new();
        for s in &segs {
            let spk = s.speaker_id.as_deref().unwrap();
            let split = split_of[s.id.as_str()];
            if let Some(prev) = spk_split.insert(spk, split) {
                assert_eq!(prev, split, "speaker {spk} leaked across splits");
            }
        }
    }

    #[test]
    fn sha256sums_manifest_covers_files_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"abc").unwrap();
        std::fs::create_dir_all(dir.path().join("data/train")).unwrap();
        std::fs::write(dir.path().join("data/train/clip.wav"), b"hello world").unwrap();
        std::fs::write(dir.path().join("metadata.csv.tmp"), b"staging").unwrap();

        write_sha256sums(dir.path()).unwrap();
        let sums = std::fs::read_to_string(dir.path().join("SHA256SUMS")).unwrap();

        // Known vector for sha256("abc").
        assert!(sums.contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  a.txt"));
        // Nested file present with a forward-slash relative path.
        assert!(sums.lines().any(|l| l.ends_with("  data/train/clip.wav")));
        // .tmp staging files and the manifest itself are excluded.
        assert!(!sums.contains(".tmp"));
        assert!(!sums.contains("SHA256SUMS"));
        // Every listed hash matches an independent recompute.
        for line in sums.lines() {
            let (hash, rel) = line.split_once("  ").unwrap();
            let bytes = std::fs::read(dir.path().join(rel)).unwrap();
            assert_eq!(hash, sha256_hex(&bytes), "hash mismatch for {rel}");
        }
    }

    #[test]
    fn export_json_replaces_existing_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("dataset.json");
        std::fs::write(&path, "{\"old\":true}").unwrap();

        export_json(&path, &sample_metadata(), &[sample_segment("json-1")]).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("json-1"));
        assert!(saved.contains("training_grade_summary"));
        assert!(saved.contains("trainingTranscript"));
        assert!(saved.contains("trainingGrade"));
        assert!(!saved.contains("\"old\""));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn export_jsonl_replaces_existing_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("dataset.jsonl");
        std::fs::write(&path, "__stale_jsonl_payload__\n").unwrap();

        export_jsonl(&path, &[sample_segment("jsonl-1")]).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("jsonl-1"));
        assert!(saved.contains("trainingTranscript"));
        assert!(saved.contains("trainingGrade"));
        assert!(!saved.contains("__stale_jsonl_payload__"));
        assert!(!path.with_extension("jsonl.tmp").exists());
    }

    #[test]
    fn export_csv_replaces_existing_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("dataset.csv");
        std::fs::write(&path, "__stale_csv_payload__\n").unwrap();

        export_csv(&path, &[sample_segment("csv-1")]).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("csv-1"));
        assert!(saved.contains("training_transcript"));
        assert!(saved.contains("training_grade"));
        assert!(!saved.contains("__stale_csv_payload__"));
        assert!(!path.with_extension("csv.tmp").exists());
    }

    #[test]
    fn export_csv_survives_adversarial_transcript_content() {
        // Dataset integrity: a Kurdish transcript containing a comma, double quotes, and a newline
        // must NOT corrupt the CSV — the csv crate quotes/escapes it. This locks that contract so a
        // future hand-rolled writer (which would silently split rows / inject) is caught immediately.
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("adv.csv");
        let mut seg = sample_segment("adv-1");
        let nasty = "کوردی، \"دەربڕین\"\nدێڕی نوێ";
        seg.raw_transcript = nasty.to_string();
        export_csv(&path, &[seg]).unwrap();

        let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_path(&path).unwrap();
        let records: Vec<csv::StringRecord> = rdr.records().map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1, "the embedded comma/newline must not create extra rows");
        assert_eq!(&records[0][0], "adv-1");
        assert_eq!(&records[0][2], nasty, "the transcript round-trips intact through CSV escaping");
    }

    #[test]
    fn csv_safe_cell_quotes_only_formula_leads() {
        // Contract: a leading formula trigger is single-quote-prefixed; everything else is
        // returned untouched (and borrowed, no allocation).
        assert_eq!(csv_safe_cell("=1+1").as_ref(), "'=1+1");
        assert_eq!(csv_safe_cell("+1").as_ref(), "'+1");
        assert_eq!(csv_safe_cell("-1").as_ref(), "'-1");
        assert_eq!(csv_safe_cell("@SUM(A1)").as_ref(), "'@SUM(A1)");
        assert_eq!(csv_safe_cell("\tx").as_ref(), "'\tx");
        assert_eq!(csv_safe_cell("\rx").as_ref(), "'\rx");
        // Normal Sorani text and an embedded (non-leading) '=' are left exactly as-is.
        assert_eq!(csv_safe_cell("کوردی").as_ref(), "کوردی");
        assert_eq!(csv_safe_cell("a=b").as_ref(), "a=b");
        assert_eq!(csv_safe_cell("").as_ref(), "");
    }

    #[test]
    fn export_csv_neutralizes_spreadsheet_formula_injection() {
        // CWE-1236: a transcript/speaker that begins with a formula trigger must be neutralized
        // so it can never execute when the exported dataset CSV is opened in a spreadsheet app.
        let tmp_dir = tempfile::tempdir().unwrap();
        let path = tmp_dir.path().join("inject.csv");
        let mut seg = sample_segment("inj-1");
        seg.raw_transcript = "=HYPERLINK(\"http://evil/\",\"x\")".to_string();
        seg.speaker_id = Some("@SUM(1+1)".to_string());
        export_csv(&path, &[seg]).unwrap();

        let mut rdr = csv::ReaderBuilder::new().has_headers(true).from_path(&path).unwrap();
        let rec = rdr.records().next().unwrap().unwrap();
        // Header order: 0 id, 2 raw_transcript, 6 speaker_id.
        assert!(rec[2].starts_with("'="), "leading = must be quote-prefixed, got {:?}", &rec[2]);
        assert!(rec[6].starts_with("'@"), "leading @ must be quote-prefixed, got {:?}", &rec[6]);
    }

    #[test]
    fn slice_for_export_skips_out_of_range_and_degenerate_windows() {
        // Round-2 audit: a present-but-out-of-range window must SKIP (None), not emit the whole file.
        let full = vec![0i16; 1000]; // ~62ms at 16kHz
                                     // start beyond the (shortened) buffer:
        let beyond = crate::chunking::SegmentSourceMeta {
            source_start_ms: 5000,
            source_end_ms: 6000,
            chunk_index: 0,
            chunk_count: 1,
        };
        assert!(slice_for_export(&full, 16000, Some(&beyond.to_alignment_json())).is_none(), "out-of-range -> skip");
        // degenerate end <= start:
        let degenerate = crate::chunking::SegmentSourceMeta {
            source_start_ms: 30,
            source_end_ms: 30,
            chunk_index: 0,
            chunk_count: 1,
        };
        assert!(slice_for_export(&full, 16000, Some(&degenerate.to_alignment_json())).is_none(), "degenerate -> skip");
    }

    #[test]
    fn slice_for_export_valid_window_and_whole_file_fallback() {
        let full: Vec<i16> = (0..16000).collect::<Vec<i32>>().iter().map(|&i| i as i16).collect();
        // Valid 0..500ms = 0..8000 samples.
        let valid = crate::chunking::SegmentSourceMeta {
            source_start_ms: 0,
            source_end_ms: 500,
            chunk_index: 0,
            chunk_count: 1,
        };
        let s = slice_for_export(&full, 16000, Some(&valid.to_alignment_json())).expect("valid window");
        assert_eq!(s.len(), 8000, "valid window slices to exactly its sample span");
        // No alignment -> whole file (intended fallback).
        let whole = slice_for_export(&full, 16000, None).expect("whole file");
        assert_eq!(whole.len(), full.len());
    }

    #[test]
    fn slice_for_export_skips_present_but_offsetless_alignment() {
        // The clobbered shape: alignment_json is a bare word-timestamp array (no source_start_ms), the
        // exact state a broken background aligner leaves. It must SKIP (None), never fall back to the
        // whole file — otherwise a 10s clip's transcript would be paired with the entire recording.
        let full = vec![0i16; 16000];
        let bare_array = r#"[{"word":"x","start":0.0,"end":1.0,"confidence":0.5}]"#;
        assert!(
            slice_for_export(&full, 16000, Some(bare_array)).is_none(),
            "a present-but-offset-less (clobbered) alignment must be skipped, never emitted whole-file"
        );
        // A MERGED object (offsets + words) still slices correctly — the good post-fix shape.
        let meta = crate::chunking::SegmentSourceMeta {
            source_start_ms: 100,
            source_end_ms: 500,
            chunk_index: 0,
            chunk_count: 2,
        };
        let words = vec![crate::aligner::WordTimestamp { word: "x".into(), start: 0.0, end: 0.4, confidence: 0.5 }];
        let merged = crate::chunking::merge_word_timestamps(Some(&meta.to_alignment_json()), &words);
        let s = slice_for_export(&full, 16000, Some(&merged)).expect("merged alignment still slices");
        assert_eq!(s.len(), chunking::ms_to_samples(400, 16000), "merged object slices to its 100..500ms window");
    }

    #[test]
    fn hf_export_persists_splits_only_after_a_successful_write() {
        // Round-2 audit MEDIUM: splits were committed to the DB BEFORE files were written. They must
        // now be persisted last — present after a successful export, absent before.
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-split.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for _ in 0..16000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let mut seg = sample_segment("hf-split-1");
        seg.audio_path = wav_path.to_string_lossy().to_string();
        db.insert_segment(&seg).unwrap();
        assert!(db.get_segment_by_id("hf-split-1").unwrap().unwrap().split.is_none(), "split unset before export");

        let out_dir = tempfile::tempdir().unwrap();
        export_huggingface_dataset(&db, out_dir.path(), &crate::settings::AppSettings::default()).unwrap();

        let split = db.get_segment_by_id("hf-split-1").unwrap().unwrap().split;
        assert!(
            matches!(split.as_deref(), Some("train" | "validation" | "test")),
            "split persisted after a successful export, got {split:?}"
        );
    }

    #[test]
    fn hf_export_excludes_holdout_gold_audio_from_training() {
        // Round-3 audit HIGH: a clip registered as a holdout (for WER/CER eval) that also exists as a
        // training-ready segment leaked into data/train, contaminating the eval set. It must be
        // excluded, while a non-holdout segment is still exported.
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let tmp_dir = tempfile::tempdir().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let write_wav = |name: &str, val: i16| {
            let p = tmp_dir.path().join(name);
            let mut w = hound::WavWriter::create(&p, spec).unwrap();
            for _ in 0..16000 {
                w.write_sample(val).unwrap();
            }
            w.finalize().unwrap();
            p.to_string_lossy().to_string()
        };
        // Distinct audio content -> distinct content hashes.
        let holdout_path = write_wav("holdout.wav", 0);
        let keep_path = write_wav("keep.wav", 1000);

        let mut hseg = sample_segment("hold-1");
        hseg.audio_path = holdout_path.clone();
        db.insert_segment(&hseg).unwrap();
        let mut kseg = sample_segment("keep-1");
        kseg.audio_path = keep_path;
        db.insert_segment(&kseg).unwrap();

        // Register the holdout clip's source as a holdout gold reference.
        crate::eval::import_gold_segments(
            &db,
            vec![crate::eval::GoldSegmentInput { audio_path: holdout_path, reference: "ref".into(), is_holdout: true }],
        )
        .unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        export_huggingface_dataset(&db, out_dir.path(), &crate::settings::AppSettings::default()).unwrap();

        let all_csv: String = ["train", "validation", "test"]
            .iter()
            .map(|s| {
                std::fs::read_to_string(out_dir.path().join("data").join(s).join("metadata.csv")).unwrap_or_default()
            })
            .collect();
        assert!(all_csv.contains("keep-1"), "the non-holdout segment must still be exported");
        assert!(!all_csv.contains("hold-1"), "the holdout gold clip must NOT leak into any split");
    }

    #[test]
    fn dataset_export_excludes_holdout_gold_audio_from_training_tables() {
        // The bundle/tabular export (JSON/JSONL/CSV/Parquet) must apply the SAME holdout exclusion as the
        // HF export — otherwise a clip registered as an eval holdout that also exists as a normal segment
        // leaks its reference transcript into the training tables.
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let tmp_dir = tempfile::tempdir().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let write_wav = |name: &str, val: i16| {
            let p = tmp_dir.path().join(name);
            let mut w = hound::WavWriter::create(&p, spec).unwrap();
            for _ in 0..16000 {
                w.write_sample(val).unwrap();
            }
            w.finalize().unwrap();
            p.to_string_lossy().to_string()
        };
        let holdout_path = write_wav("h.wav", 0);
        let keep_path = write_wav("k.wav", 1000);

        let mut hseg = sample_segment("hold-x");
        hseg.audio_path = holdout_path.clone();
        hseg.raw_transcript = "SECRETHOLDOUTREF".into();
        db.insert_segment(&hseg).unwrap();
        let mut kseg = sample_segment("keep-x");
        kseg.audio_path = keep_path;
        kseg.raw_transcript = "KEPTTRAININGTEXT".into();
        db.insert_segment(&kseg).unwrap();

        crate::eval::import_gold_segments(
            &db,
            vec![crate::eval::GoldSegmentInput { audio_path: holdout_path, reference: "ref".into(), is_holdout: true }],
        )
        .unwrap();

        let out = NamedTempFile::new().unwrap();
        let out_path = out.path().with_extension("json");
        export_dataset(&db, &out_path, &ExportFormat::Json).unwrap();
        let body = std::fs::read_to_string(&out_path).unwrap();
        assert!(body.contains("KEPTTRAININGTEXT"), "the non-holdout segment must still be exported");
        assert!(!body.contains("SECRETHOLDOUTREF"), "the holdout gold clip must NOT leak into the dataset export");
        assert!(!body.contains("hold-x"), "the holdout segment id must NOT appear in the export");
    }

    #[test]
    fn plain_export_excludes_holdout_gold_audio() {
        // Round-22 #1: the plain JSON/JSONL/CSV/Parquet export (and the production bundle that wraps
        // it) must apply the SAME holdout exclusion as the HF export, or a clip registered as a
        // holdout WER/CER reference leaks into the published training set and contaminates the gate.
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        let tmp_dir = tempfile::tempdir().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let write_wav = |name: &str, val: i16| {
            let p = tmp_dir.path().join(name);
            let mut w = hound::WavWriter::create(&p, spec).unwrap();
            for _ in 0..16000 {
                w.write_sample(val).unwrap();
            }
            w.finalize().unwrap();
            p.to_string_lossy().to_string()
        };
        let holdout_path = write_wav("holdout.wav", 0);
        let keep_path = write_wav("keep.wav", 1000);
        let mut hseg = sample_segment("hold-1");
        hseg.audio_path = holdout_path.clone();
        db.insert_segment(&hseg).unwrap();
        let mut kseg = sample_segment("keep-1");
        kseg.audio_path = keep_path;
        db.insert_segment(&kseg).unwrap();
        crate::eval::import_gold_segments(
            &db,
            vec![crate::eval::GoldSegmentInput { audio_path: holdout_path, reference: "ref".into(), is_holdout: true }],
        )
        .unwrap();

        let out = tmp_dir.path().join("dataset.csv");
        export_dataset(&db, &out, &ExportFormat::Csv).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        assert!(body.contains("keep-1"), "the non-holdout segment must still be exported");
        assert!(!body.contains("hold-1"), "the holdout gold clip must NOT leak into the plain export");
    }

    #[test]
    fn plain_export_excludes_human_rejected_segments() {
        // A human-REJECTED clip ("mark bad" in review) is KEPT in the library but must never appear in a
        // plain JSON/JSONL/CSV/Parquet export, nor be counted as verified — it carries verified=true only
        // to leave the review queue. Mirrors the human_reject exclusion the HF/training path already does.
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        db.insert_segment(&sample_segment("keep-1")).unwrap();
        db.insert_segment(&sample_segment("bad-1")).unwrap();
        // Reviewer marks it bad: a human 'reject' decision (verdict=human_reject) while verified stays true.
        db.record_human_decision("bad-1", "reject", None, None).unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let out = tmp_dir.path().join("dataset.json");
        export_dataset(&db, &out, &ExportFormat::Json).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        assert!(body.contains("keep-1"), "the confirmed-good segment must still be exported");
        assert!(!body.contains("bad-1"), "a human-rejected clip must NOT appear in the plain export");

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["metadata"]["verified_segments"].as_u64(),
            Some(1),
            "a human-rejected clip must not be counted as verified"
        );
        assert_eq!(
            parsed["metadata"]["total_segments"].as_u64(),
            Some(1),
            "a human-rejected clip must not be counted in the exported total"
        );
    }

    #[test]
    fn export_parquet_writes_valid_file() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        db.insert_segment(&sample_segment("pq-1")).unwrap();

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().with_extension("parquet");
        export_dataset(&db, &out_path, &ExportFormat::Parquet).unwrap();

        assert!(out_path.exists());
        assert!(out_path.metadata().unwrap().len() > 0);

        let file = std::fs::File::open(&out_path).unwrap();
        let reader =
            parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap();
        let batches: Vec<_> = reader.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert!(batches[0].schema().field_with_name("training_transcript").is_ok());
        assert!(batches[0].schema().field_with_name("training_grade").is_ok());
        assert!(batches[0].schema().field_with_name("training_ready").is_ok());
    }

    #[test]
    fn export_parquet_replaces_existing_file() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();
        db.insert_segment(&sample_segment("pq-replace")).unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let out_path = tmp_dir.path().join("dataset.parquet");
        std::fs::write(&out_path, "__stale_parquet_payload__").unwrap();

        export_dataset(&db, &out_path, &ExportFormat::Parquet).unwrap();

        assert!(out_path.exists());
        assert!(out_path.metadata().unwrap().len() > "__stale_parquet_payload__".len() as u64);
        assert!(!out_path.with_extension("parquet.tmp").exists());
    }

    #[test]
    fn exports_never_leak_absolute_paths() {
        // A curator's absolute path embeds their OS username + drive layout; publishing it into a
        // shared CSV/JSONL/JSON/Parquet is a real PII leak. Every exporter must emit only the audio
        // basename. Fixture uses a SYNTHETIC absolute path (no real home dir — keeps the repo's
        // private-path hygiene gate green) standing in for that username + drive layout.
        let tmp_dir = tempfile::tempdir().unwrap();
        let d = tmp_dir.path();
        let mut seg = sample_segment("p1");
        seg.audio_path = "C:\\SynthHome\\synth_user\\private_recordings\\clip_001.wav".to_string();
        let segs = [seg];

        export_json(&d.join("o.json"), &sample_metadata(), &segs).unwrap();
        export_jsonl(&d.join("o.jsonl"), &segs).unwrap();
        export_csv(&d.join("o.csv"), &segs).unwrap();
        export_parquet(&d.join("o.parquet"), &segs).unwrap();

        for name in ["o.json", "o.jsonl", "o.csv"] {
            let body = std::fs::read_to_string(d.join(name)).unwrap();
            assert!(body.contains("clip_001.wav"), "{name} should keep the basename");
            assert!(!body.contains("synth_user"), "{name} leaked the OS username: {body}");
            assert!(!body.contains("SynthHome"), "{name} leaked an absolute path");
            assert!(!body.contains("private_recordings"), "{name} leaked a directory");
        }
        // Parquet is binary — scan the raw bytes for the same leaked substrings.
        let pq = std::fs::read(d.join("o.parquet")).unwrap();
        let has = |needle: &str| pq.windows(needle.len()).any(|w| w == needle.as_bytes());
        assert!(has("clip_001.wav"), "parquet should keep the basename");
        assert!(!has("synth_user"), "parquet leaked the OS username");
        assert!(!has("private_recordings"), "parquet leaked a directory");
    }

    #[test]
    fn export_writers_error_cleanly_on_unwritable_destination() {
        // Use an existing FILE as the would-be parent directory: writing the temp file
        // underneath it must fail with a clean Err — never panic, never half-write.
        let tmp_dir = tempfile::tempdir().unwrap();
        let blocker = tmp_dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let seg = [sample_segment("x")];

        assert!(export_json(&blocker.join("d.json"), &sample_metadata(), &seg).is_err());
        assert!(export_jsonl(&blocker.join("d.jsonl"), &seg).is_err());
        assert!(export_csv(&blocker.join("d.csv"), &seg).is_err());
        assert!(export_parquet(&blocker.join("d.parquet"), &seg).is_err());

        // The blocker is untouched and no stray artifact was created.
        assert_eq!(std::fs::read_to_string(&blocker).unwrap(), "i am a file, not a directory");
    }

    #[test]
    fn export_writers_handle_empty_dataset() {
        // Exporting a dataset with zero segments must produce well-formed, non-panicking
        // output (a fresh install or a fully-filtered export hits this).
        let tmp_dir = tempfile::tempdir().unwrap();
        let d = tmp_dir.path();
        let empty: [SpeechSegment; 0] = [];

        export_json(&d.join("e.json"), &sample_metadata(), &empty).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(d.join("e.json")).unwrap()).unwrap();
        assert!(parsed.get("segments").is_some_and(|s| s.as_array().is_some_and(|a| a.is_empty())));

        export_jsonl(&d.join("e.jsonl"), &empty).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("e.jsonl")).unwrap(), "");

        export_csv(&d.join("e.csv"), &empty).unwrap();
        assert!(d.join("e.csv").exists());

        export_parquet(&d.join("e.parquet"), &empty).unwrap();
        assert!(d.join("e.parquet").exists());
    }

    #[test]
    fn export_huggingface_writes_dataset_files() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-1.wav");

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for _ in 0..16000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let mut seg = sample_segment("hf-1");
        seg.audio_path = wav_path.to_string_lossy().to_string();
        db.insert_segment(&seg).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let settings = crate::settings::AppSettings::default();
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        assert!(out_dir.path().join("data/train/metadata.csv").exists());
        assert!(out_dir.path().join("README.md").exists());
        assert!(out_dir.path().join("dataset_infos.json").exists());
        let metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
        assert!(metadata.contains("training_grade"));
        assert!(metadata.contains("gold"));

        // The real export emits a correct integrity manifest covering every artifact.
        let sums = std::fs::read_to_string(out_dir.path().join("SHA256SUMS")).unwrap();
        assert!(sums.lines().any(|l| l.ends_with("  README.md")));
        for line in sums.lines() {
            let (hash, rel) = line.split_once("  ").unwrap();
            assert_eq!(hash, sha256_hex(&std::fs::read(out_dir.path().join(rel)).unwrap()));
        }
    }

    #[test]
    fn export_huggingface_counts_dropped_missing_audio() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        // A segment whose source audio simply does not exist on disk.
        let mut seg = sample_segment("missing-1");
        seg.audio_path = "/nonexistent/does_not_exist.wav".to_string();
        db.insert_segment(&seg).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let settings = crate::settings::AppSettings::default();
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        // The drop is surfaced (no longer silent) in dataset_infos.json, and the segment
        // is absent from the exported metadata.
        let info = std::fs::read_to_string(out_dir.path().join("dataset_infos.json")).unwrap();
        assert!(info.contains("\"droppedUnavailableAudio\": 1"), "info: {info}");
        let train_meta = out_dir.path().join("data/train/metadata.csv");
        if train_meta.exists() {
            assert!(!std::fs::read_to_string(train_meta).unwrap().contains("missing-1"));
        }
    }

    #[test]
    fn export_huggingface_skips_rows_not_ready_for_training() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-filter.wav");

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for _ in 0..16000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let mut ready = sample_segment("hf-ready");
        ready.audio_path = wav_path.to_string_lossy().to_string();
        db.insert_segment(&ready).unwrap();

        let mut reject = sample_segment("hf-reject");
        reject.audio_path = wav_path.to_string_lossy().to_string();
        reject.raw_transcript.clear();
        reject.normalized_transcript = None;
        reject.annotated_transcript = None;
        reject.verified = false;
        db.insert_segment(&reject).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
        let validation_metadata =
            std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
        let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
        let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

        assert!(all_metadata.contains("hf-ready"));
        assert!(!all_metadata.contains("hf-reject"));
    }

    #[test]
    fn hf_metadata_rows_are_ordered_deterministically_by_source_path() {
        // Round-15: metadata.csv rows were emitted in HashMap (per-process-random) iteration order, so
        // two identical exports produced different bytes and different SHA256SUMS, breaking the
        // byte-reproducibility the manifest promises. With a BTreeMap the per-source row blocks are
        // written in sorted source-path order — deterministic. Insert the sources OUT of sorted order
        // and assert the rows still come out sorted, proving the ordering is by source path (not
        // insertion/segment order) and is therefore stable across runs.
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_a = tmp_dir.path().join("a_source.wav");
        let wav_z = tmp_dir.path().join("z_source.wav");
        write_silent_wav(&wav_a);
        write_silent_wav(&wav_z);

        // Insert the z-source segment FIRST (non-sorted insertion order).
        let mut sz = sample_segment("seg-from-z");
        sz.audio_path = wav_z.to_string_lossy().to_string();
        db.insert_segment(&sz).unwrap();
        let mut sa = sample_segment("seg-from-a");
        sa.audio_path = wav_a.to_string_lossy().to_string();
        db.insert_segment(&sa).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let settings = crate::settings::AppSettings {
            hf_speaker_disjoint: false,
            hf_train_ratio: 1.0,
            hf_val_ratio: 0.0,
            hf_test_ratio: 0.0,
            ..crate::settings::AppSettings::default()
        };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let meta = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
        let a_idx = meta.find("a_source_seg-from-a.wav").expect("a_source row present");
        let z_idx = meta.find("z_source_seg-from-z.wav").expect("z_source row present");
        assert!(
            a_idx < z_idx,
            "metadata.csv rows must be sorted by source path (a_source before z_source) so the file \
             is byte-reproducible:\n{meta}"
        );
    }

    #[test]
    fn export_huggingface_skips_machine_ready_rows_without_hypothesis_coverage() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-weak-machine.wav");
        write_silent_wav(&wav_path);

        let mut weak = sample_segment("hf-weak-machine");
        weak.audio_path = wav_path.to_string_lossy().to_string();
        weak.verified = false;
        weak.annotated_transcript = None;
        weak.confidence = Some(0.95);
        weak.clipping_ratio = Some(0.0);
        weak.rms_db = Some(-20.0);
        weak.snr_db = Some(20.0);
        db.insert_segment(&weak).unwrap();
        let evidence_json = serde_json::json!({
            "referenceModelId": "multi-reference-consensus:gemini-2.5-pro+gemini-2.5-flash",
            "selectedModelId": "omniasr-wsl-7b",
            "shouldCommit": true
        })
        .to_string();
        db.write_segment_verdict(
            &weak.id,
            "jury_accept",
            Some("reference candidate"),
            Some("legacy source-reference commit"),
            Some(evidence_json.as_str()),
            Some(0.92),
            false,
        )
        .unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: weak.id.clone(),
            model_id: "omniasr-wsl-7b".to_string(),
            transcript: "reference candidate".to_string(),
            confidence: Some(0.95),
        })
        .unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
        let validation_metadata =
            std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
        let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
        let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

        assert!(!all_metadata.contains("hf-weak-machine"));
        assert!(!out_dir.path().join("data/train/hf-weak-machine_hf-weak-machine.wav").exists());
    }

    #[test]
    fn export_huggingface_skips_machine_ready_rows_without_ready_agentic_report() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-no-agent-report.wav");
        write_silent_wav(&wav_path);
        insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-no-agent-report");

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
        let validation_metadata =
            std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
        let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
        let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

        assert!(!all_metadata.contains("hf-no-agent-report"));
        assert!(!out_dir.path().join("data/train/hf-no-agent-report_hf-no-agent-report.wav").exists());
    }

    #[test]
    fn export_huggingface_skips_machine_ready_rows_not_covered_by_latest_ready_agentic_report() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let covered_wav = tmp_dir.path().join("hf-covered-agent-report.wav");
        let uncovered_wav = tmp_dir.path().join("hf-uncovered-agent-report.wav");
        write_silent_wav(&covered_wav);
        write_silent_wav(&uncovered_wav);
        let covered = insert_machine_silver_segment_with_hf_coverage(&db, &covered_wav, "hf-covered-agent-report");
        insert_machine_silver_segment_with_hf_coverage(&db, &uncovered_wav, "hf-uncovered-agent-report");
        insert_source_reference_with_identity(&db, covered.audio_path.as_str(), "gemini-2.5-pro");
        insert_source_reference_with_identity(&db, covered.audio_path.as_str(), "gemini-2.5-flash");
        record_ready_agent_report(&db, covered.audio_path.as_str(), covered.id.as_str(), "run-hf-covered-agent-report");

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
        let validation_metadata =
            std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
        let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
        let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

        assert!(all_metadata.contains("hf-covered-agent-report"));
        assert!(!all_metadata.contains("hf-uncovered-agent-report"));
    }

    #[test]
    fn export_huggingface_skips_machine_ready_rows_without_current_source_reference_identity() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-legacy-source-reference.wav");
        write_silent_wav(&wav_path);
        let segment = insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-legacy-source-reference");
        insert_source_reference_without_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
        insert_source_reference_without_identity(&db, segment.audio_path.as_str(), "gemini-2.5-flash");
        record_ready_agent_report(
            &db,
            segment.audio_path.as_str(),
            segment.id.as_str(),
            "run-hf-legacy-source-reference",
        );

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let all_metadata = all_huggingface_metadata(out_dir.path());

        assert!(!all_metadata.contains("hf-legacy-source-reference"));
        assert!(!out_dir.path().join("data/train/hf-legacy-source-reference_hf-legacy-source-reference.wav").exists());
    }

    #[test]
    fn export_huggingface_skips_machine_ready_rows_missing_configured_source_reference_model() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-missing-source-reference-model.wav");
        write_silent_wav(&wav_path);
        let segment =
            insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-missing-source-reference-model");
        insert_source_reference_with_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
        record_ready_agent_report(
            &db,
            segment.audio_path.as_str(),
            segment.id.as_str(),
            "run-hf-missing-source-reference-model",
        );

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let all_metadata = all_huggingface_metadata(out_dir.path());

        assert!(!all_metadata.contains("hf-missing-source-reference-model"));
        assert!(!out_dir
            .path()
            .join("data/train/hf-missing-source-reference-model_hf-missing-source-reference-model.wav")
            .exists());
    }

    #[test]
    fn export_huggingface_skips_machine_ready_rows_with_stale_source_reference_identity() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-stale-source-reference.wav");
        write_silent_wav(&wav_path);
        let segment = insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-stale-source-reference");
        insert_stale_source_reference_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
        insert_stale_source_reference_identity(&db, segment.audio_path.as_str(), "gemini-2.5-flash");
        record_ready_agent_report(
            &db,
            segment.audio_path.as_str(),
            segment.id.as_str(),
            "run-hf-stale-source-reference",
        );

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let all_metadata = all_huggingface_metadata(out_dir.path());

        assert!(!all_metadata.contains("hf-stale-source-reference"));
        assert!(!out_dir.path().join("data/train/hf-stale-source-reference_hf-stale-source-reference.wav").exists());
    }

    #[test]
    fn export_huggingface_writes_machine_ready_rows_with_matching_ready_agentic_report() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-ready-agent-report.wav");
        write_silent_wav(&wav_path);
        let segment = insert_machine_silver_segment_with_hf_coverage(&db, &wav_path, "hf-ready-agent-report");
        insert_source_reference_with_identity(&db, segment.audio_path.as_str(), "gemini-2.5-pro");
        insert_source_reference_with_identity(&db, segment.audio_path.as_str(), "gemini-2.5-flash");
        record_ready_agent_report(&db, segment.audio_path.as_str(), segment.id.as_str(), "run-hf-ready-agent-report");

        let out_dir = tempfile::tempdir().unwrap();
        let settings =
            crate::settings::AppSettings { hf_speaker_disjoint: false, ..crate::settings::AppSettings::default() };
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let train_metadata = std::fs::read_to_string(out_dir.path().join("data/train/metadata.csv")).unwrap();
        let validation_metadata =
            std::fs::read_to_string(out_dir.path().join("data/validation/metadata.csv")).unwrap_or_default();
        let test_metadata = std::fs::read_to_string(out_dir.path().join("data/test/metadata.csv")).unwrap_or_default();
        let all_metadata = format!("{train_metadata}\n{validation_metadata}\n{test_metadata}");

        assert!(all_metadata.contains("hf-ready-agent-report"));
        assert!(all_metadata.contains("silver"));
        assert!(all_metadata.contains("jury_verdict"));
    }

    #[test]
    fn export_huggingface_replaces_metadata_files() {
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("hf-atomic.wav");

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for _ in 0..16000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let mut seg = sample_segment("hf-atomic");
        seg.audio_path = wav_path.to_string_lossy().to_string();
        db.insert_segment(&seg).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let train_dir = out_dir.path().join("data/train");
        std::fs::create_dir_all(&train_dir).unwrap();
        std::fs::write(train_dir.join("metadata.csv"), "__stale_split_metadata__").unwrap();
        let stale_wav_path = train_dir.join("hf-atomic_hf-atomic.wav");
        std::fs::write(&stale_wav_path, "__stale_wav_payload__").unwrap();
        // Orphan from a prior, larger export: a clip whose name this export does NOT produce.
        let orphan_wav_path = train_dir.join("old-recording_orphan-seg.wav");
        std::fs::write(&orphan_wav_path, "__orphan_wav_payload__").unwrap();
        std::fs::write(out_dir.path().join("README.md"), "__stale_readme__").unwrap();
        std::fs::write(out_dir.path().join("dataset_infos.json"), "__stale_info__").unwrap();
        let settings = crate::settings::AppSettings::default();

        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let readme = std::fs::read_to_string(out_dir.path().join("README.md")).unwrap();
        let info = std::fs::read_to_string(out_dir.path().join("dataset_infos.json")).unwrap();
        let split_metadata = std::fs::read_to_string(train_dir.join("metadata.csv")).unwrap();
        assert!(!readme.contains("__stale_readme__"));
        assert!(!info.contains("__stale_info__"));
        assert!(!split_metadata.contains("__stale_split_metadata__"));
        assert!(split_metadata.contains("hf-atomic_hf-atomic.wav"));
        assert_ne!(std::fs::read(&stale_wav_path).unwrap(), b"__stale_wav_payload__");
        assert_eq!(hound::WavReader::open(&stale_wav_path).unwrap().spec().sample_rate, 16000);
        assert!(!out_dir.path().join("README.md.tmp").exists());
        assert!(!out_dir.path().join("dataset_infos.json.tmp").exists());
        assert!(!train_dir.join("metadata.csv.tmp").exists());
        assert_no_tmp_exports(&train_dir);
    }

    #[test]
    fn hf_reexport_removes_orphan_wav_for_a_dropped_segment() {
        // Round-12 audit (#5/#6): a re-export must remove the WAV of a segment that no longer exports
        // (so it isn't left orphaned and hashed into SHA256SUMS with no metadata row), while keeping the
        // still-exporting segments and a metadata.csv for every split. An EMPTY re-export is a separate
        // no-op that preserves the prior export, so this scenario keeps one segment exporting.
        let db_tmp = NamedTempFile::new().unwrap();
        let db = Database::open(db_tmp.path().to_str().unwrap()).unwrap();
        db.initialize().unwrap();

        let tmp_dir = tempfile::tempdir().unwrap();
        let wav_path = tmp_dir.path().join("clip.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for _ in 0..16000 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        for id in ["orphan-seg", "keep-seg"] {
            let mut seg = sample_segment(id);
            seg.audio_path = wav_path.to_string_lossy().to_string();
            db.insert_segment(&seg).unwrap();
        }

        let out_dir = tempfile::tempdir().unwrap();
        let settings = crate::settings::AppSettings::default();
        let data_dir = out_dir.path().join("data");

        let find_wavs = |root: &std::path::Path| -> Vec<std::path::PathBuf> {
            ["train", "validation", "test"]
                .iter()
                .flat_map(|s| std::fs::read_dir(root.join("data").join(s)).ok().into_iter().flatten().flatten())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
                .collect()
        };

        // Run 1: both segments export.
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();
        assert_eq!(find_wavs(out_dir.path()).len(), 2, "run 1 exports two wavs");

        // One segment no longer exports; re-export into the SAME directory.
        db.delete_segment("orphan-seg").unwrap();
        export_huggingface_dataset(&db, out_dir.path(), &settings).unwrap();

        let after = find_wavs(out_dir.path());
        assert_eq!(after.len(), 1, "only the kept segment's wav remains, got {after:?}");
        assert!(after[0].to_string_lossy().contains("keep-seg"), "kept wav is keep-seg: {after:?}");

        let sums = std::fs::read_to_string(out_dir.path().join("SHA256SUMS")).unwrap();
        assert!(!sums.contains("orphan-seg"), "SHA256SUMS must not list the orphan WAV:\n{sums}");
        assert!(sums.contains("keep-seg"), "SHA256SUMS must list the kept WAV:\n{sums}");

        // Every declared split still has a metadata.csv (header-only for the empty ones).
        for s in ["train", "validation", "test"] {
            assert!(data_dir.join(s).join("metadata.csv").exists(), "split {s} must have a metadata.csv");
        }
    }

    fn assert_no_tmp_exports(dir: &std::path::Path) {
        let tmp_left = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!tmp_left, "temporary export files should be promoted or removed");
    }
}
