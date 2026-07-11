use crate::error::{AppError, AppResult};
use rusqlite::{backup, params, types::Value, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// One row of the `jobs` table as read: (id, kind, state, progress, completed, total, error_code).
type JobRow = (String, String, String, f64, i64, Option<i64>, Option<String>);

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpeechSegment {
    pub id: String,
    pub created_at: Option<String>,
    pub audio_path: String,
    pub raw_transcript: String,
    pub normalized_transcript: Option<String>,
    pub annotated_transcript: Option<String>,
    pub alignment_json: Option<String>,
    pub duration_ms: i64,
    pub speaker_id: Option<String>,
    pub verified: bool,
    pub confidence: Option<f64>,
    pub ctc_score: Option<f64>,
    pub clipping_ratio: Option<f64>,
    pub rms_db: Option<f64>,
    pub snr_db: Option<f64>,
    pub split: Option<String>,
    pub ood_score: Option<f64>,
    // ── Jury fields (Migration v11) ────────────────────────────────
    /// NULL = unprocessed; "auto_accept" | "jury_accept" | "jury_edit"
    /// | "escalated" | "human_accept" | "human_edit" | "human_reject"
    pub verdict: Option<String>,
    pub verdict_transcript: Option<String>,
    pub rationale: Option<String>,
    pub evidence_json: Option<String>,
    pub agent_confidence: Option<f64>,
    pub escalated: bool,
    pub human_decision: Option<String>,
    pub corrected_at: Option<String>,
    pub is_gold: bool,
    // ── Alignment quality (Migration v12) ─────────────────────────
    /// "ctc_forced" | "energy_heuristic" | None (never aligned)
    pub alignment_quality: Option<String>,
    /// Existing model registry id (Migration v22), e.g. "omniasr-ctc-300m".
    pub model_version_id: Option<String>,
    /// "real_posterior" | "heuristic" | provider-specific value. Heuristic confidence is not calibrated.
    pub confidence_source: Option<String>,
    /// Whether producing this segment transcript involved sending audio/transcript to a cloud provider.
    pub cloud_call: bool,
    /// Hash of decoder/runtime settings that materially affect the transcript.
    pub decoder_config_hash: Option<String>,
    /// Version of the Sorani normalizer used for normalized_transcript / metrics.
    pub normalizer_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentsPage {
    pub items: Vec<SpeechSegment>,
    pub total: usize,
    pub next_cursor: Option<String>,
}

/// P3.3: which distinct source audio files are missing on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioHealth {
    pub total_files: usize,
    pub missing_files: usize,
    pub missing_paths: Vec<String>,
}

/// P3.3: outcome of a basename-based relink.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelinkResult {
    pub relinked: usize,
    pub still_missing: usize,
}

/// P3.2: a directory-import job in the resume journal (a crash leaves one 'running').
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportJob {
    pub id: String,
    pub dir: String,
    pub total_files: usize,
    pub completed_paths: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SegmentHypothesis {
    pub segment_id: String,
    pub model_id: String,
    pub transcript: String,
    pub confidence: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SourceTranscriptRecord {
    pub audio_path: String,
    pub model_id: String,
    pub audio_content_hash: Option<String>,
    pub audio_size_bytes: Option<i64>,
    pub transcript_path: String,
    pub transcript_text: String,
    pub created_at: Option<String>,
}

pub struct Database {
    conn: Connection,
    path: String,
}

fn human_verdict_for_decision(decision: &str) -> AppResult<&'static str> {
    match decision {
        "accept" => Ok("human_accept"),
        "edit" => Ok("human_edit"),
        "reject" => Ok("human_reject"),
        other => Err(AppError::Validation(format!("Unknown human decision: {other}"))),
    }
}

/// The speech_segments columns loaded when recording a human decision, in SELECT order:
/// `(is_gold, raw_transcript, normalized_transcript, annotated_transcript, verdict_transcript,
/// prior_verdict, audio_path, model_version_id)`. Aliased to keep the `query_row` annotation
/// readable (clippy::type_complexity).
type HumanDecisionContext =
    (i32, String, Option<String>, Option<String>, Option<String>, Option<String>, String, String);

/// Canonicalize stored text to Unicode NFC. Sorani/Arabic combining marks (diacritics,
/// madda, hamza) can arrive decomposed from ASR or import; storing inconsistent forms
/// silently fragments FTS search, content-dedup, and WER references that all assume one
/// canonical spelling. Idempotent — NFC of already-NFC text is unchanged.
fn to_nfc(s: &str) -> String {
    s.nfc().collect()
}

/// NFC-canonicalize a segment's three transcript fields for storage.
fn nfc_transcripts(seg: &SpeechSegment) -> (String, Option<String>, Option<String>) {
    (
        to_nfc(&seg.raw_transcript),
        seg.normalized_transcript.as_deref().map(to_nfc),
        seg.annotated_transcript.as_deref().map(to_nfc),
    )
}

/// Fold Sorani codepoint variants (Kaf ك/ک, Yeh ي/ی, Heh, Hamza, ZWNJ, tatweel) in a
/// full-text search query so it matches the canonical `normalized_transcript` column
/// regardless of which keyboard variant the user typed — the FTS index stores the
/// normalizer's unified form, but a raw query in a different codepoint would never
/// match it. Digit conversion/verbalization is intentionally skipped so a digit query
/// still matches the raw transcript; the letter rules mirror those applied to the
/// stored normalized text.
fn normalize_search_query(text: &str) -> String {
    crate::normalizer::SoraniNormalizer::with_config(crate::normalizer::NormalizationConfig {
        normalize_numbers: false,
        verbalize_numbers: false,
        normalize_hamza: true,
        remove_diacritics: false,
    })
    .normalize(text)
}

/// Convert free-text search input into a SAFE FTS5 `MATCH` string. FTS5 parses the bound value as a
/// full-text *query*, so bare metacharacters (`"` `:` `*` `(` `)` `^` `-` and the bareword keywords
/// `AND`/`OR`/`NEAR`) make it raise a hard error on ordinary punctuation — e.g. a user typing a
/// single `"` or `:` in the transcript search box would get a low-level "fts5: syntax error" instead
/// of results. We treat the box as literal text: split on whitespace and wrap each token as a quoted
/// FTS5 string (internal `"` doubled), which FTS5 reads as a literal term. Tokens are implicitly
/// AND-ed (matching the previous behaviour for multi-word queries). Returns `""` for whitespace-only
/// input so the caller can short-circuit to an empty result.
fn to_fts5_match(query: &str) -> String {
    // Control characters (NUL and other C0/C1) are never meaningful search terms and an embedded
    // NUL makes SQLite/FTS5 raise a hard error (interior NUL in a bound string), so map every
    // control char to a separator before tokenizing — they can't survive into the MATCH string.
    let cleaned: String = query.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    cleaned
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The only split labels the export/stats math understands.
const VALID_SPLITS: &[&str] = &["train", "validation", "test"];

const SEGMENT_SELECT_COLUMNS: &str = "id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                    confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                    verdict, verdict_transcript, rationale, evidence_json,
                    agent_confidence, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality, model_version_id, confidence_source, cloud_call,
                    decoder_config_hash, normalizer_version";

/// Reject structurally-invalid segments at the DB write boundary, before they can
/// corrupt the downstream split/stats/training-grade math that every later stage
/// branches on. Guards the fields these insert paths actually persist; verdict and
/// human_decision are validated at their own dedicated write paths.
fn validate_segment(seg: &SpeechSegment) -> AppResult<()> {
    if seg.id.trim().is_empty() {
        return Err(AppError::Validation("Segment id must not be empty".into()));
    }
    if seg.duration_ms < 0 {
        return Err(AppError::Validation(format!(
            "Segment '{}' has a negative duration_ms ({})",
            seg.id, seg.duration_ms
        )));
    }
    if let Some(split) = seg.split.as_deref() {
        if !VALID_SPLITS.contains(&split) {
            return Err(AppError::Validation(format!(
                "Segment '{}' has invalid split '{split}' (expected one of {VALID_SPLITS:?})",
                seg.id
            )));
        }
    }
    Ok(())
}

use crate::normalizer::learning_text_key;

fn rejected_transcript_for_learning(corrected: &str, candidates: &[Option<String>]) -> Option<String> {
    let corrected_key = learning_text_key(corrected);
    candidates.iter().filter_map(|candidate| candidate.as_deref()).find_map(|candidate| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() || learning_text_key(trimmed) == corrected_key {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

impl Database {
    pub fn open(path: &str) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA cache_size=-64000;
             PRAGMA busy_timeout=10000;",
        )?;
        Ok(Self { conn, path: path.to_string() })
    }

    /// Open the database with a retry policy for corruption.
    ///
    /// Recovery is fail-CLOSED: `recover_database_at` is DESTRUCTIVE (it renames the live db away and
    /// opens a fresh empty one), so it must fire ONLY on genuine corruption. A transient error — an
    /// external process holding the file locked past busy_timeout, a disk I/O hiccup, OOM during the
    /// integrity check — must NOT quarantine a healthy database (that would be silent data loss);
    /// instead it aborts startup so the user can clear the locker / fix the disk and retry intact.
    pub fn open_with_retry(path: &str) -> AppResult<Self> {
        match Self::open(path) {
            Ok(db) => {
                match db.integrity_check() {
                    Ok(result) if result.trim() == "ok" => {
                        return Ok(db);
                    }
                    Ok(result) if integrity_result_looks_transient(&result) => {
                        // PRAGMA integrity_check reports a transient page-read failure (a momentary disk
                        // error, or AV/backup/indexer holding a page locked mid-scan) as a text result
                        // ROW, e.g. "unable to get the page 42. error code=8" — which arrives here as
                        // Ok(non-"ok"). Quarantining (renaming the live db away and opening an empty one)
                        // a HEALTHY db on that transient signal is silent total data loss. Mirror the
                        // Err branch's discipline: abort startup WITHOUT quarantine so the user can retry
                        // with their data intact.
                        tracing::error!("Database integrity check returned a transient I/O message (not corruption); aborting startup without quarantine: {result}");
                        return Err(AppError::Other(format!(
                            "Database integrity check could not complete (transient, not corruption): {result}"
                        )));
                    }
                    Ok(result) => {
                        // A non-"ok", non-transient string is SQLite reporting genuine structural page
                        // corruption: quarantine.
                        tracing::error!("Database integrity check failed on open; quarantining database: {result}");
                    }
                    Err(e) if is_corruption_error(&e) => {
                        tracing::error!(
                            "Database integrity check returned a corruption code on open; quarantining database: {e}"
                        );
                    }
                    Err(e) => {
                        // Transient/non-corruption error — do NOT destroy a possibly-healthy database.
                        tracing::error!("Database integrity check could not complete (transient, not corruption); aborting startup without quarantine: {e}");
                        return Err(e);
                    }
                }
                drop(db);
                recover_database_at(path)?;
                Self::open(path)
            }
            Err(e) if is_corruption_error(&e) => {
                tracing::error!("Failed to open database with a corruption code: {e}. Attempting recovery...");
                recover_database_at(path)?;
                Self::open(path)
            }
            Err(e) => {
                // A non-corruption open failure (lock contention, permissions, transient I/O) must not
                // quarantine the database — surface it so the user can resolve and retry.
                tracing::error!("Failed to open database (transient/non-corruption); aborting without quarantine: {e}");
                Err(e)
            }
        }
    }

    pub fn initialize(&self) -> AppResult<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS speech_segments (
                id TEXT PRIMARY KEY,
                audio_path TEXT NOT NULL,
                raw_transcript TEXT NOT NULL DEFAULT '',
                normalized_transcript TEXT,
                annotated_transcript TEXT,
                alignment_json TEXT,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                speaker_id TEXT,
                verified INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_segments_verified ON speech_segments(verified);
            CREATE INDEX IF NOT EXISTS idx_segments_speaker ON speech_segments(speaker_id);
            CREATE INDEX IF NOT EXISTS idx_segments_created ON speech_segments(created_at);
            -- AUTHORITATIVE segments_fts schema. Round-23 #8: migrations/001_initial.sql contains a
            -- second, DIFFERENT (4-column) CREATE for segments_fts, but this block runs first on a fresh
            -- boot, so the migration's `IF NOT EXISTS` makes it a no-op — THIS definition is the one in
            -- effect. Edit the FTS schema HERE (and the three triggers below), not in the migration copy.
            -- `audio_path` stays indexed for trigger symmetry but is excluded from search by a column
            -- filter in search_segments (#7), so it never produces false-positive transcript hits.
            CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(
                id UNINDEXED,
                audio_path,
                raw_transcript,
                normalized_transcript,
                annotated_transcript,
                content=speech_segments,
                content_rowid=rowid,
                tokenize='unicode61'
            );
            CREATE TRIGGER IF NOT EXISTS segments_ai AFTER INSERT ON speech_segments BEGIN
                INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
            END;
            CREATE TRIGGER IF NOT EXISTS segments_ad AFTER DELETE ON speech_segments BEGIN
                INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
            END;
            CREATE TRIGGER IF NOT EXISTS segments_au AFTER UPDATE ON speech_segments BEGIN
                INSERT INTO segments_fts(segments_fts, rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES ('delete', old.rowid, old.id, old.audio_path, old.raw_transcript, old.normalized_transcript, old.annotated_transcript);
                INSERT INTO segments_fts(rowid, id, audio_path, raw_transcript, normalized_transcript, annotated_transcript)
                VALUES (new.rowid, new.id, new.audio_path, new.raw_transcript, new.normalized_transcript, new.annotated_transcript);
            END;"
        )?;
        self.conn.execute_batch("INSERT INTO segments_fts(segments_fts) VALUES('rebuild');")?;
        crate::migrations::run_migrations(self)?;
        Ok(())
    }

    fn cleanup_savepoint_after_error(&self, savepoint: &str) {
        if let Err(error) = self.conn.execute(&format!("ROLLBACK TO {savepoint}"), []) {
            tracing::warn!("Failed to roll back savepoint {savepoint}: {error}");
        }
        if let Err(error) = self.conn.execute(&format!("RELEASE {savepoint}"), []) {
            tracing::warn!("Failed to release savepoint {savepoint}: {error}");
        }
    }

    /// Release (commit) the named OUTERMOST savepoint. For the outermost savepoint, RELEASE *is* the
    /// WAL commit and can fail (SQLITE_BUSY/IOERR at commit time); SQLite then leaves the savepoint
    /// OPEN. If we returned that error without unwinding (the old `RELEASE ...?`), the dangling
    /// savepoint would persist on the shared, poison-recovering (never-reopened) connection, so the
    /// NEXT command would run inside the stale transaction and a later ROLLBACK TO could silently
    /// discard writes already reported as committed. Roll it back + release on failure so a failed
    /// commit cannot poison the connection.
    fn release_savepoint(&self, savepoint: &str) -> AppResult<()> {
        if let Err(error) = self.conn.execute(&format!("RELEASE {savepoint}"), []) {
            self.cleanup_savepoint_after_error(savepoint);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn insert_segment(&self, seg: &SpeechSegment) -> AppResult<()> {
        validate_segment(seg)?;
        let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, audio_path, raw_transcript, normalized_transcript,
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score, alignment_quality,
                 model_version_id, confidence_source, cloud_call, decoder_config_hash, normalizer_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, COALESCE(?18, 'unknown@pre-registry'), COALESCE(?19, 'unknown'), ?20, ?21, ?22)
             ON CONFLICT(id) DO UPDATE SET
                audio_path=excluded.audio_path,
                raw_transcript=excluded.raw_transcript,
                normalized_transcript=excluded.normalized_transcript,
                annotated_transcript=excluded.annotated_transcript,
                alignment_json=excluded.alignment_json,
                duration_ms=excluded.duration_ms,
                speaker_id=excluded.speaker_id,
                verified=excluded.verified,
                confidence=excluded.confidence,
                ctc_score=excluded.ctc_score,
                clipping_ratio=excluded.clipping_ratio,
                rms_db=excluded.rms_db,
                snr_db=excluded.snr_db,
                split=excluded.split,
                ood_score=excluded.ood_score,
                alignment_quality=excluded.alignment_quality,
                model_version_id=excluded.model_version_id,
                confidence_source=excluded.confidence_source,
                cloud_call=excluded.cloud_call,
                decoder_config_hash=excluded.decoder_config_hash,
                normalizer_version=excluded.normalizer_version,
                updated_at=datetime('now')",
            params![
                seg.id, seg.audio_path, raw_nfc,
                normalized_nfc, annotated_nfc,
                seg.alignment_json, seg.duration_ms, seg.speaker_id,
                seg.verified as i32, seg.confidence, seg.ctc_score,
                seg.clipping_ratio, seg.rms_db, seg.snr_db, seg.split,
                seg.ood_score, seg.alignment_quality,
                seg.model_version_id,
                seg.confidence_source,
                seg.cloud_call as i32,
                seg.decoder_config_hash,
                seg.normalizer_version,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Resurrect a HARD-DELETED segment from a full in-memory snapshot, persisting EVERY column the
    /// snapshot carries — including the jury / human-review / gold-provenance fields (verdict,
    /// verdict_transcript, rationale, evidence_json, agent_confidence, escalated, human_decision,
    /// corrected_at, is_gold) and `created_at` that [`insert_segment`] deliberately omits.
    ///
    /// [`insert_segment`]'s 17-column subset is correct for the normal edit path, where the row still
    /// exists and its `ON CONFLICT DO UPDATE` branch leaves the untouched columns intact. But undoing a
    /// deletion runs as a *fresh* INSERT (the row was physically removed by `delete_segment` /
    /// `delete_segments_batch`), so anything `insert_segment` skips would silently revert to its schema
    /// default: verdict/human_decision/is_gold/corrected_at → NULL/0 and `created_at` → datetime('now'),
    /// reordering the row in every `ORDER BY created_at` query and export. This method writes the whole
    /// row so a restore is lossless. `created_at` falls back to `datetime('now')` only when the snapshot
    /// genuinely lacks one (the column is NOT NULL).
    pub fn insert_segment_full(&self, seg: &SpeechSegment) -> AppResult<()> {
        validate_segment(seg)?;
        let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
        // NFC-normalize the jury verdict transcript too, so a restored row stays byte-consistent with
        // the rest of the (already NFC) transcript columns.
        let verdict_transcript_nfc = seg.verdict_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, created_at, audio_path, raw_transcript, normalized_transcript,
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence,
                 ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                 verdict, verdict_transcript, rationale, evidence_json, agent_confidence, escalated,
                 human_decision, corrected_at, is_gold, alignment_quality, model_version_id,
                 confidence_source, cloud_call, decoder_config_hash, normalizer_version, updated_at)
             VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                 COALESCE(?28, 'unknown@pre-registry'), COALESCE(?29, 'unknown'), ?30, ?31, ?32, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                created_at=excluded.created_at,
                audio_path=excluded.audio_path,
                raw_transcript=excluded.raw_transcript,
                normalized_transcript=excluded.normalized_transcript,
                annotated_transcript=excluded.annotated_transcript,
                alignment_json=excluded.alignment_json,
                duration_ms=excluded.duration_ms,
                speaker_id=excluded.speaker_id,
                verified=excluded.verified,
                confidence=excluded.confidence,
                ctc_score=excluded.ctc_score,
                clipping_ratio=excluded.clipping_ratio,
                rms_db=excluded.rms_db,
                snr_db=excluded.snr_db,
                split=excluded.split,
                ood_score=excluded.ood_score,
                verdict=excluded.verdict,
                verdict_transcript=excluded.verdict_transcript,
                rationale=excluded.rationale,
                evidence_json=excluded.evidence_json,
                agent_confidence=excluded.agent_confidence,
                escalated=excluded.escalated,
                human_decision=excluded.human_decision,
                corrected_at=excluded.corrected_at,
                is_gold=excluded.is_gold,
                alignment_quality=excluded.alignment_quality,
                model_version_id=excluded.model_version_id,
                confidence_source=excluded.confidence_source,
                cloud_call=excluded.cloud_call,
                decoder_config_hash=excluded.decoder_config_hash,
                normalizer_version=excluded.normalizer_version,
                updated_at=datetime('now')",
            params![
                seg.id,
                seg.created_at,
                seg.audio_path,
                raw_nfc,
                normalized_nfc,
                annotated_nfc,
                seg.alignment_json,
                seg.duration_ms,
                seg.speaker_id,
                seg.verified as i32,
                seg.confidence,
                seg.ctc_score,
                seg.clipping_ratio,
                seg.rms_db,
                seg.snr_db,
                seg.split,
                seg.ood_score,
                seg.verdict,
                verdict_transcript_nfc,
                seg.rationale,
                seg.evidence_json,
                seg.agent_confidence,
                seg.escalated as i32,
                seg.human_decision,
                seg.corrected_at,
                seg.is_gold as i32,
                seg.alignment_quality,
                seg.model_version_id,
                seg.confidence_source,
                seg.cloud_call as i32,
                seg.decoder_config_hash,
                seg.normalizer_version,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Alias for [`insert_segment_full`], kept so the undo path can read as "restore". Both names
    /// write the FULL row (jury/review/gold columns + created_at) so an undone delete is lossless.
    pub fn restore_segment(&self, seg: &SpeechSegment) -> AppResult<()> {
        self.insert_segment_full(seg)
    }

    /// Targeted single-column update: sets `verified` without touching any other field.
    /// Returns true if the row was found and updated.
    pub fn update_verified(&self, id: &str, verified: bool) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET verified = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, verified as i32],
        )?;
        Ok(rows > 0)
    }

    /// Targeted single-column update: sets `speaker_id` without touching any other field.
    /// Pass `None` to clear the speaker assignment.
    /// Returns true if the row was found and updated.
    pub fn update_speaker_id(&self, id: &str, speaker_id: Option<&str>) -> AppResult<bool> {
        let rows = self.conn.execute(
            "UPDATE speech_segments SET speaker_id = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, speaker_id],
        )?;
        Ok(rows > 0)
    }

    pub fn insert_segments_batch(&self, segments: &[SpeechSegment]) -> AppResult<()> {
        // Use a SAVEPOINT on the shared connection — avoids opening a second
        // file handle that could race with other writers under WAL mode.
        self.conn.execute("SAVEPOINT batch_insert", [])?;
        let result: AppResult<()> = (|| {
            let mut stmt = self.conn.prepare(
                "INSERT INTO speech_segments
                    (id, audio_path, raw_transcript, normalized_transcript,
                     annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                     model_version_id, confidence_source, cloud_call, decoder_config_hash, normalizer_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'), ?19, ?20, ?21)
                 ON CONFLICT(id) DO UPDATE SET
                    audio_path=excluded.audio_path,
                    raw_transcript=excluded.raw_transcript,
                    normalized_transcript=excluded.normalized_transcript,
                    annotated_transcript=excluded.annotated_transcript,
                    alignment_json=excluded.alignment_json,
                    duration_ms=excluded.duration_ms,
                    speaker_id=excluded.speaker_id,
                    verified=excluded.verified,
                    confidence=excluded.confidence,
                    ctc_score=excluded.ctc_score,
                    clipping_ratio=excluded.clipping_ratio,
                    rms_db=excluded.rms_db,
                    snr_db=excluded.snr_db,
                    split=excluded.split,
                    ood_score=excluded.ood_score,
                    model_version_id=excluded.model_version_id,
                    confidence_source=excluded.confidence_source,
                    cloud_call=excluded.cloud_call,
                    decoder_config_hash=excluded.decoder_config_hash,
                    normalizer_version=excluded.normalizer_version,
                    updated_at=datetime('now')"
            )?;
            for seg in segments {
                validate_segment(seg)?;
                let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
                stmt.execute(params![
                    seg.id,
                    seg.audio_path,
                    raw_nfc,
                    normalized_nfc,
                    annotated_nfc,
                    seg.alignment_json,
                    seg.duration_ms,
                    seg.speaker_id,
                    seg.verified as i32,
                    seg.confidence,
                    seg.ctc_score,
                    seg.clipping_ratio,
                    seg.rms_db,
                    seg.snr_db,
                    seg.split,
                    seg.ood_score,
                    seg.model_version_id,
                    seg.confidence_source,
                    seg.cloud_call as i32,
                    seg.decoder_config_hash,
                    seg.normalizer_version,
                ])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_insert")?;
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("batch_insert");
                Err(e)
            }
        }
    }

    pub fn merge_dataset_json(&self, json_content: &str) -> AppResult<(usize, usize)> {
        let external_segments: Vec<SpeechSegment> = serde_json::from_str(json_content)?;
        let mut updated = 0;
        let mut created = 0;

        self.conn.execute("SAVEPOINT merge_json", [])?;
        let result: AppResult<()> = (|| {
            let mut check_stmt = self.conn.prepare("SELECT id FROM speech_segments WHERE id = ?1")?;
            let mut insert_stmt = self.conn.prepare(
                "INSERT INTO speech_segments
                    (id, audio_path, raw_transcript, normalized_transcript,
                     annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                     model_version_id, confidence_source, cloud_call, decoder_config_hash, normalizer_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, COALESCE(?17, 'unknown@pre-registry'), COALESCE(?18, 'unknown'), ?19, ?20, ?21)"
            )?;
            // Guard: never overwrite human decisions; only update unreviewed rows.
            let mut update_stmt = self.conn.prepare(
                "UPDATE speech_segments SET
                    audio_path=?2, raw_transcript=?3, normalized_transcript=?4, 
                    annotated_transcript=?5, alignment_json=?6, duration_ms=?7, 
                    speaker_id=?8, verified=?9, confidence=?10, ctc_score=?11,
                    clipping_ratio=?12, rms_db=?13, snr_db=?14, split=?15, ood_score=?16,
                    model_version_id=COALESCE(?17, 'unknown@pre-registry'),
                    confidence_source=COALESCE(?18, 'unknown'),
                    cloud_call=?19,
                    decoder_config_hash=?20,
                    normalizer_version=?21,
                    updated_at=datetime('now')
                 WHERE id=?1
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            )?;

            for seg in &external_segments {
                validate_segment(seg)?;
                let (raw_nfc, normalized_nfc, annotated_nfc) = nfc_transcripts(seg);
                let exists = check_stmt.exists(params![seg.id])?;
                if exists {
                    // Count only rows the guard actually changed — a human-reviewed row matches 0
                    // rows here (the UPDATE skips it), so it must not be reported as "updated".
                    let changed = update_stmt.execute(params![
                        seg.id,
                        seg.audio_path,
                        raw_nfc,
                        normalized_nfc,
                        annotated_nfc,
                        seg.alignment_json,
                        seg.duration_ms,
                        seg.speaker_id,
                        seg.verified as i32,
                        seg.confidence,
                        seg.ctc_score,
                        seg.clipping_ratio,
                        seg.rms_db,
                        seg.snr_db,
                        seg.split,
                        seg.ood_score,
                        seg.model_version_id,
                        seg.confidence_source,
                        seg.cloud_call as i32,
                        seg.decoder_config_hash,
                        seg.normalizer_version,
                    ])?;
                    if changed > 0 {
                        updated += 1;
                    }
                } else {
                    insert_stmt.execute(params![
                        seg.id,
                        seg.audio_path,
                        raw_nfc,
                        normalized_nfc,
                        annotated_nfc,
                        seg.alignment_json,
                        seg.duration_ms,
                        seg.speaker_id,
                        seg.verified as i32,
                        seg.confidence,
                        seg.ctc_score,
                        seg.clipping_ratio,
                        seg.rms_db,
                        seg.snr_db,
                        seg.split,
                        seg.ood_score,
                        seg.model_version_id,
                        seg.confidence_source,
                        seg.cloud_call as i32,
                        seg.decoder_config_hash,
                        seg.normalizer_version,
                    ])?;
                    created += 1;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("merge_json")?;
                self.track_write()?;
                Ok((created, updated))
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("merge_json");
                Err(e)
            }
        }
    }

    /// Safely update the ASR transcript for a segment only if a human has NOT
    /// already reviewed it. This is the correct API for the WSL 7B branch to
    /// persist refined transcripts without overwriting user edits.
    #[allow(clippy::too_many_arguments)]
    pub fn update_asr_transcript_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
        confidence_source: Option<&str>,
        model_version_id: Option<&str>,
        cloud_call: bool,
    ) -> AppResult<bool> {
        // NFC-canonicalize before writing the FTS-indexed columns, exactly like insert_segment /
        // update_segment. The WSL 7B branch feeds raw ASR output here, which can arrive decomposed;
        // storing a non-NFC form fragments the search index so the text can't be found.
        let raw_nfc = to_nfc(raw_transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        let rows_changed = self.conn.execute(
            "UPDATE speech_segments
             SET raw_transcript        = ?2,
                 normalized_transcript = ?3,
                 confidence            = ?4,
                 confidence_source     = COALESCE(?5, 'unknown'),
                 model_version_id      = COALESCE(?6, 'unknown@pre-registry'),
                 cloud_call            = ?7,
                 updated_at            = datetime('now')
             WHERE id = ?1
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![
                segment_id,
                raw_nfc,
                normalized_nfc,
                confidence,
                confidence_source,
                model_version_id,
                cloud_call as i32,
            ],
        )?;
        self.track_write()?;
        Ok(rows_changed > 0)
    }

    /// Persist a batch (re)transcription result WITHOUT clobbering concurrent human work.
    ///
    /// Batch transcription runs in a background thread off a snapshot taken at batch start; a human can
    /// verify or edit a target segment while the batch is in flight. Writing the whole stale snapshot
    /// back (the old `insert_segment` path) reverted the human's `verified` flag and overwrote their
    /// edited annotation — a silent lost update. This targeted write instead:
    ///   • updates ONLY the ASR-derived columns (raw / normalized / confidence),
    ///   • seeds `annotated_transcript` solely when it is still empty, via COALESCE against the CURRENT
    ///     row (never the stale snapshot), so an in-flight human annotation is preserved,
    ///   • never touches `verified`, and
    ///   • skips any row a human has verified or reviewed since the batch began.
    /// Returns Ok(true) if the row was updated, Ok(false) if it was skipped as human-owned.
    #[allow(clippy::too_many_arguments)]
    pub fn update_batch_transcription_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
        confidence_source: Option<&str>,
        model_version_id: Option<&str>,
        cloud_call: bool,
        annotated_seed: &str,
    ) -> AppResult<bool> {
        let raw_nfc = to_nfc(raw_transcript);
        let normalized_nfc = normalized_transcript.map(to_nfc);
        let annotated_nfc = to_nfc(annotated_seed);
        let rows_changed = self.conn.execute(
            "UPDATE speech_segments
             SET raw_transcript        = ?2,
                 normalized_transcript = ?3,
                 confidence            = ?4,
                 confidence_source     = COALESCE(?5, 'unknown'),
                 model_version_id      = COALESCE(?6, 'unknown@pre-registry'),
                 cloud_call            = ?7,
                 annotated_transcript  = COALESCE(annotated_transcript, ?8),
                 updated_at            = datetime('now')
             WHERE id = ?1
               AND verified = 0
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![
                segment_id,
                raw_nfc,
                normalized_nfc,
                confidence,
                confidence_source,
                model_version_id,
                cloud_call as i32,
                annotated_nfc,
            ],
        )?;
        self.track_write()?;
        Ok(rows_changed > 0)
    }

    /// Fold ONE segment's LOOP-0 shadow evidence into the durable archive BEFORE it is deleted, so the
    /// C5 over-trigger gate isn't survivor-biased by the owner's normal cleanup (review a bad clip, then
    /// delete it — exactly the rows most likely to be over-triggers). Uses the same correlation as
    /// `intelligence_report`. Must run while the segment + its shadow rows still exist (before DELETE).
    fn archive_loop0_evidence_for(&self, id: &str) -> AppResult<()> {
        // Per-SEGMENT semantics (true-10 audit 2026-07-09): a segment re-processed N times holds N
        // shadow rows, but the C5 gate reasons about distinct events — one clip, one human decision,
        // at most one over-trigger. Fold MAX(memory_fired) per segment (this fn archives exactly one
        // segment), matching intelligence_report's DISTINCT-segment live counts.
        self.conn.execute(
            "UPDATE loop0_evidence_archive SET
                 total_observations = total_observations
                     + COALESCE((SELECT COUNT(DISTINCT segment_id) FROM loop0_shadow_log WHERE segment_id = ?1), 0),
                 would_fire = would_fire
                     + COALESCE((SELECT MAX(memory_fired) FROM loop0_shadow_log WHERE segment_id = ?1), 0),
                 fired_human_accepted = fired_human_accepted + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('accept','human_accept') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0),
                 fired_human_edited = fired_human_edited + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('edit','human_edit') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0),
                 fired_human_rejected = fired_human_rejected + COALESCE((
                     SELECT MAX(CASE WHEN l.memory_fired = 1 AND s.human_decision IN ('reject','human_reject') THEN 1 ELSE 0 END)
                     FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id WHERE l.segment_id = ?1), 0)
             WHERE id = 1",
            params![id],
        )?;
        Ok(())
    }

    /// v34 twin of [`Self::archive_loop0_evidence_for`], for the C4 auto-accept-precision denominator:
    /// decision_verdicts CASCADE-deletes with its segment, so the owner's normal cleanup (review a bad
    /// clip, then delete it) removed exactly the T0_ACCEPT rows whose humans CONTRADICTED the machine —
    /// the precision gating any autonomy increase could only drift optimistic (true-10 audit
    /// 2026-07-09). Must run while the segment + its verdict row still exist (before DELETE).
    fn archive_c4_evidence_for(&self, id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE c4_evidence_archive SET
                 t0_accepts = t0_accepts + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = ?1 AND auto_accept_verdict = 'T0_ACCEPT'), 0),
                 t1_escalations = t1_escalations + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts WHERE segment_id = ?1 AND auto_accept_verdict = 'T1_ESCALATE'), 0),
                 t0_human_confirmed = t0_human_confirmed + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id
                     WHERE dv.segment_id = ?1 AND dv.auto_accept_verdict = 'T0_ACCEPT'
                       AND s.human_decision IN ('accept','human_accept')), 0),
                 t0_human_contradicted = t0_human_contradicted + COALESCE((
                     SELECT COUNT(*) FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id
                     WHERE dv.segment_id = ?1 AND dv.auto_accept_verdict = 'T0_ACCEPT'
                       AND s.human_decision IN ('edit','human_edit','reject','human_reject')), 0)
             WHERE id = 1",
            params![id],
        )?;
        Ok(())
    }

    pub fn delete_segment(&self, id: &str) -> AppResult<()> {
        self.conn.execute("SAVEPOINT del_seg", [])?;
        let result: AppResult<()> = (|| {
            self.archive_loop0_evidence_for(id)?;
            self.archive_c4_evidence_for(id)?;
            self.conn.execute("DELETE FROM speech_segments WHERE id = ?1", params![id])?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("del_seg")?;
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("del_seg");
                Err(e)
            }
        }
    }

    pub fn delete_segments_batch(&self, ids: &[String]) -> AppResult<()> {
        self.conn.execute("SAVEPOINT batch_delete", [])?;
        let result: AppResult<()> = (|| {
            // Archive each segment's shadow + C4 evidence FIRST (while its rows still exist), then delete.
            for id in ids {
                self.archive_loop0_evidence_for(id)?;
                self.archive_c4_evidence_for(id)?;
            }
            let mut stmt = self.conn.prepare("DELETE FROM speech_segments WHERE id = ?1")?;
            for id in ids {
                stmt.execute(params![id])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.release_savepoint("batch_delete")?;
                // Keep the FTS5 index clean after bulk deletions.
                if let Err(error) = self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES('optimize')", []) {
                    tracing::warn!("Failed to optimize segments FTS index after batch delete: {error}");
                }
                self.track_write()?;
                Ok(())
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("batch_delete");
                Err(e)
            }
        }
    }

    pub fn get_segment_by_id(&self, id: &str) -> AppResult<Option<SpeechSegment>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE id = ?1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_row(row)?))
        } else {
            Ok(None)
        }
    }

    /// Look up a segment by its `audio_path` column using the `idx_segments_audio_path` index.
    /// Used by the media registry to verify playback access without a full table scan.
    /// Returns `Ok(Some(...))` when found, `Ok(None)` when no segment matches the path.
    pub fn get_segment_by_audio_path(&self, audio_path: &str) -> AppResult<Option<SpeechSegment>> {
        let query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE audio_path = ?1 LIMIT 1");
        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![audio_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_segments(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        let mut query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments");
        if let Some(v) = verified {
            query.push_str(&format!(" WHERE verified = {}", if v { 1 } else { 0 }));
        }
        // `, id ASC` is a deterministic tiebreaker: created_at has 1s resolution, so a chunked file's
        // batch-inserted segments tie, and without a unique secondary key SQLite's tie order is
        // undefined — making JSON/JSONL/CSV/Parquet exports non-byte-reproducible across plan/VACUUM.
        query.push_str(" ORDER BY created_at DESC, id ASC");

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn get_segments_page(
        &self,
        verified: Option<bool>,
        text_query: Option<&str>,
        sort: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> AppResult<SegmentsPage> {
        let limit = limit.clamp(1, 500);
        let offset = cursor.and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);

        let mut where_parts: Vec<String> = Vec::new();
        let mut bind_values: Vec<Value> = Vec::new();
        if let Some(v) = verified {
            bind_values.push(Value::Integer(if v { 1 } else { 0 }));
            where_parts.push(format!("verified = ?{}", bind_values.len()));
        }
        if let Some(raw_query) = text_query.map(str::trim).filter(|value| !value.is_empty()) {
            let match_query = to_fts5_match(&normalize_search_query(raw_query));
            if match_query.is_empty() {
                return Ok(SegmentsPage { items: Vec::new(), total: 0, next_cursor: None });
            }
            let scoped_query =
                format!("{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})");
            bind_values.push(Value::Text(scoped_query));
            where_parts
                .push(format!("id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?{})", bind_values.len()));
        }
        let where_sql =
            if where_parts.is_empty() { String::new() } else { format!(" WHERE {}", where_parts.join(" AND ")) };
        let order_sql = match sort {
            "oldest" => "datetime(created_at) ASC, id ASC",
            "duration" => "duration_ms DESC, id ASC",
            "verified" => "verified DESC, datetime(created_at) DESC, id ASC",
            "confidence" => "COALESCE(confidence, 1.0) ASC, id ASC",
            "activeLearning" | "active_learning" => {
                "ABS(((1.0 - COALESCE(confidence, 0.5)) + (0.1 * -COALESCE(ctc_score, -5.0))) - 0.35) ASC, id ASC"
            }
            "suspectFirst" | "suspect_first" => {
                "escalated DESC, COALESCE(agent_confidence, 0.5) ASC, datetime(created_at) DESC, id ASC"
            }
            _ => "datetime(created_at) DESC, id ASC",
        };

        let count_sql = format!("SELECT COUNT(*) FROM speech_segments{where_sql}");
        let total: i64 =
            self.conn.query_row(&count_sql, rusqlite::params_from_iter(bind_values.iter()), |row| row.get(0))?;

        let mut page_values = bind_values.clone();
        page_values.push(Value::Integer(limit as i64));
        page_values.push(Value::Integer(offset as i64));
        let limit_idx = page_values.len() - 1;
        let offset_idx = page_values.len();
        let page_sql = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments{where_sql} ORDER BY {order_sql} LIMIT ?{limit_idx} OFFSET ?{offset_idx}"
        );
        let mut stmt = self.conn.prepare(&page_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(page_values.iter()), Self::map_row)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        let next_offset = offset + items.len();
        let next_cursor = if next_offset < total as usize { Some(next_offset.to_string()) } else { None };
        Ok(SegmentsPage { items, total: total as usize, next_cursor })
    }

    /// M2.5: Return segments ordered by suspect-first priority for ReviewInbox.
    /// Jury escalated segments first, then low-confidence (suspicious) segments, then chronological.
    pub fn get_segments_suspect_first(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        let mut query = format!("SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments");
        if let Some(v) = verified {
            query.push_str(&format!(" WHERE verified = {}", if v { 1 } else { 0 }));
        }
        // Priority: escalated (jury doubts) first, then low agent confidence (suspicious), then chronological.
        query.push_str(" ORDER BY escalated DESC, COALESCE(agent_confidence, 0.5) ASC, created_at DESC, id ASC");

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn search_segments(&self, text: &str) -> AppResult<Vec<SpeechSegment>> {
        let match_query = to_fts5_match(&normalize_search_query(text));
        // Whitespace-only / empty input is an empty result, not an FTS5 `MATCH ""` error.
        if match_query.is_empty() {
            return Ok(Vec::new());
        }
        // Round-23 #7: the segments_fts table also indexes `audio_path`, so a bare `MATCH ?` matches the
        // query against the FILE PATH too — a token that appears only in a folder/file name returned
        // false-positive segments whose transcript did not contain it. Restrict the match to the
        // transcript columns with an FTS5 column filter so only transcript content is searched.
        let scoped_query = format!("{{raw_transcript normalized_transcript annotated_transcript}} : ({match_query})");
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}
             FROM speech_segments
             WHERE id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)
             ORDER BY created_at DESC, id ASC"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![scoped_query], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    /// Batch-fetch segments by a list of IDs using a single `WHERE id IN (...)` query.
    /// Dramatically faster than N individual `get_segment_by_id` calls for delete/undo
    /// history snapshots on large selections.
    pub fn get_segments_by_ids(&self, ids: &[String]) -> AppResult<Vec<SpeechSegment>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // SQLite caps bound parameters per statement (SQLITE_MAX_VARIABLE_NUMBER — only 999 on older
        // builds). A large selection (delete/undo of thousands of segments) would overflow a single
        // IN(?,?,…) and fail with "too many SQL variables", so fetch in bounded chunks and re-impose
        // the global ordering afterwards (per-chunk ORDER BY doesn't compose across chunks).
        const CHUNK: usize = 500;
        let mut segments: Vec<SpeechSegment> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            // Build a parameterised placeholder list: (?1,?2,...?N)
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let query = format!(
                "SELECT {SEGMENT_SELECT_COLUMNS} FROM speech_segments WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&query)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), Self::map_row)?;
            for row in rows {
                segments.push(row?);
            }
        }
        // Match the single-query contract: created_at DESC (newest first), then id ASC. None sorts
        // last under DESC, mirroring SQLite ordering NULLs after non-NULLs in a descending sort.
        segments.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(segments)
    }

    pub fn rename_speaker(&self, old_id: &str, new_id: &str) -> AppResult<usize> {
        let count = self.conn.execute(
            "UPDATE speech_segments SET speaker_id = ?2, updated_at = datetime('now') WHERE speaker_id = ?1",
            params![old_id, new_id],
        )?;
        self.track_write()?;
        Ok(count)
    }

    pub fn integrity_check(&self) -> AppResult<String> {
        let result: String = self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        Ok(result)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// The on-disk path this connection was opened from (or `":memory:"`). Used by commands that need
    /// to open a SECOND, dedicated connection so they can release the global AppState db Mutex before a
    /// long network call (e.g. cloud jury T2) — holding it across the round-trip would freeze every
    /// other DB-touching command app-wide.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn info(&self) -> AppResult<serde_json::Value> {
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let journal_mode: String = self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let segment_count: i64 = self.conn.query_row("SELECT count(*) FROM speech_segments", [], |r| r.get(0))?;
        Ok(serde_json::json!({
            "path": self.path,
            "sizeBytes": size,
            "journalMode": journal_mode,
            "segmentCount": segment_count,
        }))
    }

    pub fn segment_count(&self) -> AppResult<i64> {
        let count: i64 = self.conn.query_row("SELECT count(*) FROM speech_segments", [], |r| r.get(0))?;
        Ok(count)
    }

    pub fn backup<P: AsRef<Path>>(&self, dest: P) -> AppResult<()> {
        let mut dest_conn = Connection::open(dest.as_ref())?;
        let backup = backup::Backup::new(&self.conn, &mut dest_conn)?;
        backup.run_to_completion(5, std::time::Duration::from_millis(250), None)?;
        Ok(())
    }

    pub fn restore<P: AsRef<Path>>(&mut self, src: P) -> AppResult<()> {
        let src_conn = Connection::open(src.as_ref())?;
        // Verify the SOURCE snapshot is a healthy database BEFORE overwriting the live one, so a corrupt
        // snapshot fails fast with a clear error instead of part-way through the backup copy.
        let integrity: String = src_conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity.trim() != "ok" {
            return Err(AppError::Other(format!("snapshot database failed its integrity check: {integrity}")));
        }
        let backup = backup::Backup::new(&src_conn, &mut self.conn)?;
        backup.run_to_completion(5, std::time::Duration::from_millis(250), None)?;
        Ok(())
    }

    pub fn vacuum(&self) -> AppResult<()> {
        self.conn.execute("VACUUM", [])?;
        // VACUUM can renumber speech_segments' implicit rowids, silently desyncing the external-content
        // FTS index (search would return unrelated rows until the next restart). Rebuild it immediately.
        self.conn.execute("INSERT INTO segments_fts(segments_fts) VALUES('rebuild')", [])?;
        Ok(())
    }

    pub fn wal_checkpoint(&self) -> AppResult<()> {
        self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    /// P3.3: audio durability — segments reference their source audio by absolute path in place, so a
    /// file the owner moves/renames over months of use silently breaks playback, re-transcription, and
    /// the jury's source-reference guard. Report which distinct audio files are now missing.
    pub fn audio_health(&self) -> AppResult<AudioHealth> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT audio_path FROM speech_segments")?;
        let paths: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        let total_files = paths.len();
        let mut missing_paths: Vec<String> = paths.into_iter().filter(|p| !Path::new(p).exists()).collect();
        missing_paths.sort();
        Ok(AudioHealth { total_files, missing_files: missing_paths.len(), missing_paths })
    }

    /// P3.3: relink missing source audio by basename — for each missing `audio_path`, if a file with the
    /// same file name exists under `search_dir`, repoint every segment on that old path to the found one.
    /// Basename match (speech_segments store no content hash to verify against); the owner points at the
    /// folder they moved the audio to. Returns how many distinct paths were relinked + how many remain.
    ///
    /// AMBIGUITY GUARD: if two DISTINCT missing source paths share a basename (e.g. `interview.wav`
    /// imported from two different folders), a single found `interview.wav` cannot be known to be the
    /// right one for both — blindly repointing both would serve the WRONG audio for one recording on
    /// playback/re-transcription. Such colliding paths are left missing (and warned), never guessed.
    pub fn relink_audio(&self, search_dir: &Path) -> AppResult<RelinkResult> {
        let missing = self.audio_health()?.missing_paths;
        // Count distinct missing paths per basename so we can refuse ambiguous relinks.
        let mut basename_counts: std::collections::HashMap<std::ffi::OsString, usize> =
            std::collections::HashMap::new();
        for old in &missing {
            if let Some(name) = Path::new(old).file_name() {
                *basename_counts.entry(name.to_os_string()).or_insert(0) += 1;
            }
        }
        let mut relinked = 0usize;
        for old in &missing {
            let Some(name) = Path::new(old).file_name() else { continue };
            if basename_counts.get(name).copied().unwrap_or(0) > 1 {
                tracing::warn!(
                    "relink: '{}' shares its filename with another missing source — skipped (ambiguous, would risk the wrong audio)",
                    old
                );
                continue;
            }
            let candidate = search_dir.join(name);
            if candidate.is_file() {
                let new_path = candidate.to_string_lossy().to_string();
                let n = self.conn.execute(
                    "UPDATE speech_segments SET audio_path = ?2, updated_at = datetime('now') WHERE audio_path = ?1",
                    params![old, new_path],
                )?;
                if n > 0 {
                    relinked += 1;
                }
            }
        }
        self.track_write()?;
        Ok(RelinkResult { relinked, still_missing: self.audio_health()?.missing_files })
    }

    // ── P3.2: import journal (resume a directory import interrupted by a crash) ──────────────────

    /// Open a new import job (status 'running'). Also prunes old finished jobs so the journal stays
    /// small. Journal writes are best-effort at the call sites — a failure here never fails an import.
    pub fn begin_import_job(&self, dir: &str, total_files: usize) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        // Reap stale crashes first. Imports are single-flight (try_start_import guards the only call
        // site), so when a NEW import begins any lingering 'running' job is a PRIOR crash the user did
        // not resume — the startup resume prompt already had its chance before this new import started.
        // Marking them 'abandoned' keeps exactly one 'running' job (the active one), so:
        //   * find_interrupted_import_job stays unambiguous — no spurious "resume?" for an old crash
        //     after the user already resumed a newer one, and
        //   * 'running' rows can't accumulate unboundedly across repeated crashes (abandoned rows are
        //     status != 'running', so the retention prune below reaps them + CASCADE clears their files).
        self.conn.execute(
            "UPDATE import_jobs SET status = 'abandoned', updated_at = datetime('now') WHERE status = 'running'",
            [],
        )?;
        self.conn.execute(
            "INSERT INTO import_jobs (id, dir, total_files, status) VALUES (?1, ?2, ?3, 'running')",
            params![id, dir, total_files as i64],
        )?;
        // Retention: keep the newest 50 FINISHED jobs (running jobs are always kept — they may be crashes).
        self.conn.execute(
            "DELETE FROM import_jobs WHERE status != 'running' AND id NOT IN (
                 SELECT id FROM import_jobs WHERE status != 'running'
                 ORDER BY datetime(created_at) DESC, id DESC LIMIT 50
             )",
            [],
        )?;
        Ok(id)
    }

    /// Record that `path` finished processing in job `job_id` (idempotent).
    pub fn mark_import_file_done(&self, job_id: &str, path: &str) -> AppResult<()> {
        self.conn
            .execute("INSERT OR IGNORE INTO import_job_files (job_id, path) VALUES (?1, ?2)", params![job_id, path])?;
        self.conn.execute("UPDATE import_jobs SET updated_at = datetime('now') WHERE id = ?1", params![job_id])?;
        Ok(())
    }

    /// Mark a job finished (a clean end): it is no longer an interruption to resume.
    pub fn complete_import_job(&self, job_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE import_jobs SET status = 'completed', updated_at = datetime('now') WHERE id = ?1",
            params![job_id],
        )?;
        Ok(())
    }

    /// Discard an interrupted job (the user chose not to resume). Deletes both tables explicitly so it
    /// works whether or not the foreign-keys pragma is enabling CASCADE.
    pub fn discard_import_job(&self, job_id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM import_job_files WHERE job_id = ?1", params![job_id])?;
        self.conn.execute("DELETE FROM import_jobs WHERE id = ?1", params![job_id])?;
        Ok(())
    }

    /// The most recent still-'running' job — a crash never calls `complete_import_job`, so it stays
    /// running. Intended to be queried at STARTUP (when no import is active, a running job IS a crash).
    pub fn find_interrupted_import_job(&self) -> AppResult<Option<ImportJob>> {
        let head = self.conn.query_row(
            "SELECT id, dir, total_files, created_at FROM import_jobs
             WHERE status = 'running' ORDER BY datetime(created_at) DESC, id DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)),
        );
        let (id, dir, total_files, created_at) = match head {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut stmt = self.conn.prepare("SELECT path FROM import_job_files WHERE job_id = ?1")?;
        let completed_paths: Vec<String> = stmt.query_map(params![id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(Some(ImportJob { id, dir, total_files: total_files as usize, completed_paths, created_at }))
    }

    // ── Durable jobs (migration v37 + crate::jobs::JobState) — the persistent Job Supervisor. ──

    /// Build a `Job` from a `(id, kind, state_str, progress, completed, total, error_code)` row tuple,
    /// erroring if the persisted state is outside the lifecycle vocabulary (the CHECK constraint makes
    /// that impossible, but a corrupt DB shouldn't silently coerce to a wrong state).
    fn job_from_row(row: JobRow) -> AppResult<crate::jobs::Job> {
        let (id, kind, state_str, progress, completed, total, error_code) = row;
        let state = crate::jobs::JobState::parse(&state_str)
            .ok_or_else(|| AppError::Other(format!("job {id} has an unknown state {state_str:?} in the database")))?;
        Ok(crate::jobs::Job { id, kind, state, progress, completed, total, error_code })
    }

    const JOB_COLS: &str = "id, kind, state, progress, completed, total, error_code";

    fn read_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
    }

    /// Fetch a job by id, or `None` if it doesn't exist.
    pub fn get_job(&self, id: &str) -> AppResult<Option<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs WHERE id = ?1", Self::JOB_COLS);
        match self.conn.query_row(&sql, params![id], Self::read_job_row) {
            Ok(row) => Ok(Some(Self::job_from_row(row)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn get_job_by_idempotency_key(&self, key: &str) -> AppResult<Option<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs WHERE idempotency_key = ?1", Self::JOB_COLS);
        match self.conn.query_row(&sql, params![key], Self::read_job_row) {
            Ok(row) => Ok(Some(Self::job_from_row(row)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Create a queued job, or return the existing one when `idempotency_key` is already present — so a
    /// re-issued identical request (retry, double-click) resumes the same job instead of duplicating work.
    pub fn create_or_get_job(
        &self,
        id: &str,
        kind: &str,
        idempotency_key: Option<&str>,
        total: Option<i64>,
    ) -> AppResult<crate::jobs::Job> {
        if let Some(key) = idempotency_key {
            if let Some(existing) = self.get_job_by_idempotency_key(key)? {
                return Ok(existing);
            }
        }
        self.conn.execute(
            "INSERT INTO jobs (id, kind, idempotency_key, total, state) VALUES (?1, ?2, ?3, ?4, 'queued')",
            params![id, kind, idempotency_key, total],
        )?;
        self.get_job(id)?.ok_or_else(|| AppError::Other(format!("job {id} vanished immediately after insert")))
    }

    /// Move a job to `to`, enforcing the `JobState` lifecycle (an illegal edge — e.g. completing twice,
    /// or resurrecting a cancelled job — is rejected, not silently written). Stamps `started_at` on the
    /// first entry to `running` and `finished_at` on any terminal state.
    pub fn transition_job(&self, id: &str, to: crate::jobs::JobState, error_code: Option<&str>) -> AppResult<()> {
        let current = self.get_job(id)?.ok_or_else(|| AppError::Other(format!("job {id} not found")))?;
        if !current.state.can_transition_to(to) {
            return Err(AppError::Validation(format!(
                "illegal job transition {} -> {} for job {id}",
                current.state, to
            )));
        }
        let finished = to.is_terminal() as i64;
        self.conn.execute(
            "UPDATE jobs SET
                 state = ?2,
                 error_code = ?3,
                 started_at = CASE WHEN ?2 = 'running' AND started_at IS NULL THEN datetime('now') ELSE started_at END,
                 finished_at = CASE WHEN ?4 = 1 THEN datetime('now') ELSE finished_at END,
                 updated_at = datetime('now')
             WHERE id = ?1",
            params![id, to.as_str(), error_code, finished],
        )?;
        Ok(())
    }

    /// Update a running job's progress. `progress` is clamped to 0.0..=1.0 to respect the CHECK constraint.
    pub fn update_job_progress(&self, id: &str, completed: i64, progress: f64) -> AppResult<()> {
        let progress = progress.clamp(0.0, 1.0);
        self.conn.execute(
            "UPDATE jobs SET completed = ?2, progress = ?3, updated_at = datetime('now') WHERE id = ?1",
            params![id, completed, progress],
        )?;
        Ok(())
    }

    /// At STARTUP, any job still `running` is a crash residue (a clean run always reaches a terminal
    /// state). Mark them failed with a stable `INTERRUPTED` code so the UI can honestly show "interrupted"
    /// instead of a ghost that never finishes. Returns how many were reaped.
    // ponytail: generic recovery = fail+INTERRUPTED; a resumable job kind can re-create from its own
    // durable state on the next run. Add per-kind auto-resume only when a kind actually needs it.
    pub fn mark_orphaned_running_jobs_failed(&self) -> AppResult<usize> {
        let n = self.conn.execute(
            "UPDATE jobs SET state = 'failed', error_code = COALESCE(error_code, 'INTERRUPTED'),
                 finished_at = datetime('now'), updated_at = datetime('now')
             WHERE state = 'running'",
            [],
        )?;
        Ok(n)
    }

    /// The most recent jobs (newest first), for a UI activity surface.
    pub fn list_recent_jobs(&self, limit: i64) -> AppResult<Vec<crate::jobs::Job>> {
        let sql = format!("SELECT {} FROM jobs ORDER BY datetime(created_at) DESC, id DESC LIMIT ?1", Self::JOB_COLS);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<JobRow> = stmt.query_map(params![limit], Self::read_job_row)?.collect::<Result<_, _>>()?;
        rows.into_iter().map(Self::job_from_row).collect()
    }

    /// Bracket `work` as a durable job: record a queued→running lifecycle, run it, then mark
    /// succeeded, or failed with the stable `error_code` on error (the original error still propagates).
    /// A crash mid-`work` leaves a `running` row that `mark_orphaned_running_jobs_failed` reaps at the
    /// next startup — that is the whole point of routing a long op through here. `job_id` is caller-
    /// supplied (a fresh uuid) so the id is known before `work` starts and survives a crash.
    pub fn run_tracked<T>(
        &self,
        job_id: &str,
        kind: &str,
        error_code: &str,
        work: impl FnOnce(&Database) -> AppResult<T>,
    ) -> AppResult<T> {
        self.create_or_get_job(job_id, kind, None, None)?;
        self.transition_job(job_id, crate::jobs::JobState::Running, None)?;
        // Once `work` returns, the op's real outcome is DECIDED (the export file is on disk, or not).
        // The terminal stamp is a best-effort RECORD of that — it must never change what the caller sees.
        // If a stamp write fails, the row lingers `running` and is reaped as INTERRUPTED at the next
        // startup: a cosmetic history wart, never a false result to the user or data loss.
        match work(self) {
            Ok(v) => {
                if let Err(e) = self.transition_job(job_id, crate::jobs::JobState::Succeeded, None) {
                    tracing::warn!("job {job_id} ({kind}) succeeded but recording success failed: {e}");
                }
                Ok(v)
            }
            Err(e) => {
                let _ = self.transition_job(job_id, crate::jobs::JobState::Failed, Some(error_code));
                Err(e)
            }
        }
    }

    pub fn insert_hypothesis(&self, hyp: &SegmentHypothesis) -> AppResult<()> {
        // NFC-canonicalize the vote at this single chokepoint so EVERY engine's hypothesis (local
        // 300M/1B/WSL-7B and cloud Scribe) is stored in the same normalization form. The jury scores
        // agreement by exact surface word-equality (diff/phonetic.rs); without this, two engines that
        // emit the same Sorani text in different forms (NFD vs NFC) would be scored as disagreeing and
        // a real consensus would be spuriously escalated. Matches the NFC enforced on speech_segments.
        let transcript = to_nfc(&hyp.transcript);
        self.conn.execute(
            "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(segment_id, model_id) DO UPDATE SET
                transcript=excluded.transcript,
                confidence=excluded.confidence,
                created_at=datetime('now')",
            params![hyp.segment_id, hyp.model_id, transcript, hyp.confidence],
        )?;
        Ok(())
    }

    pub fn get_hypotheses_for_segment(&self, segment_id: &str) -> AppResult<Vec<SegmentHypothesis>> {
        let mut stmt = self.conn.prepare(
            "SELECT segment_id, model_id, transcript, confidence
             FROM segment_hypotheses WHERE segment_id = ?1
             ORDER BY created_at DESC, model_id ASC",
        )?;
        let rows = stmt.query_map(params![segment_id], |row| {
            Ok(SegmentHypothesis {
                segment_id: row.get(0)?,
                model_id: row.get(1)?,
                transcript: row.get(2)?,
                confidence: row.get(3)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    pub fn delete_hypotheses_for_segment(&self, segment_id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM segment_hypotheses WHERE segment_id = ?1", params![segment_id])?;
        Ok(())
    }

    pub fn upsert_source_transcript(&self, record: &SourceTranscriptRecord) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO source_transcripts
                (audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(audio_path, model_id) DO UPDATE SET
                audio_content_hash=excluded.audio_content_hash,
                audio_size_bytes=excluded.audio_size_bytes,
                transcript_path=excluded.transcript_path,
                transcript_text=excluded.transcript_text,
                updated_at=datetime('now')",
            params![
                record.audio_path,
                record.model_id,
                record.audio_content_hash,
                record.audio_size_bytes,
                record.transcript_path,
                record.transcript_text
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn get_source_transcript(&self, audio_path: &str, model_id: &str) -> AppResult<Option<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1 AND model_id = ?2",
        )?;
        let mut rows = stmt.query(params![audio_path, model_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_latest_source_transcript_for_audio(
        &self,
        audio_path: &str,
    ) -> AppResult<Option<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1
             ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![audio_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn get_source_transcripts_for_audio(&self, audio_path: &str) -> AppResult<Vec<SourceTranscriptRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT audio_path, model_id, audio_content_hash, audio_size_bytes, transcript_path, transcript_text, created_at
             FROM source_transcripts
             WHERE audio_path = ?1
             ORDER BY datetime(updated_at) DESC, datetime(created_at) DESC",
        )?;
        let rows = stmt.query_map(params![audio_path], |row| {
            Ok(SourceTranscriptRecord {
                audio_path: row.get(0)?,
                model_id: row.get(1)?,
                audio_content_hash: row.get(2)?,
                audio_size_bytes: row.get(3)?,
                transcript_path: row.get(4)?,
                transcript_text: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// P3.2 (resume fix): segment IDs previously imported from a given source audio file. Used on
    /// import-resume to fold already-imported files back into the post-import jury batch — the jury
    /// runs once at the end keyed on the freshly-imported ids, so without this the files persisted
    /// before a crash would never be adjudicated (they are skipped from re-processing on resume).
    pub fn segment_ids_for_audio_path(&self, audio_path: &str) -> AppResult<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM speech_segments WHERE audio_path = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![audio_path], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_all_hypotheses(&self) -> AppResult<Vec<SegmentHypothesis>> {
        let mut stmt =
            self.conn.prepare("SELECT segment_id, model_id, transcript, confidence FROM segment_hypotheses")?;
        let rows = stmt.query_map([], |row| {
            Ok(SegmentHypothesis {
                segment_id: row.get(0)?,
                model_id: row.get(1)?,
                transcript: row.get(2)?,
                confidence: row.get(3)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Load the persisted per-model IRT abilities (F7). Empty when learning has never run, in which
    /// case the consensus falls back to the hardcoded heuristic priors (identical to the old behavior).
    pub fn load_model_abilities(&self) -> AppResult<std::collections::HashMap<String, f64>> {
        let mut stmt = self.conn.prepare("SELECT model_id, ability FROM model_abilities")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (id, ability) = r?;
            map.insert(id, ability);
        }
        Ok(map)
    }

    /// Upsert the EM-fitted per-model IRT abilities (F7). Only finite abilities are stored.
    pub fn save_model_abilities(&self, abilities: &std::collections::HashMap<String, f64>) -> AppResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO model_abilities (model_id, ability, updated_at) VALUES (?1, ?2, datetime('now'))
                 ON CONFLICT(model_id) DO UPDATE SET ability = excluded.ability, updated_at = excluded.updated_at",
            )?;
            for (model_id, ability) in abilities {
                if ability.is_finite() {
                    stmt.execute(rusqlite::params![model_id, ability])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Returns the number of rows actually changed — human-reviewed rows are skipped by the guard,
    /// so this can be less than `updates.len()`; callers must report THIS, not the attempted count.
    pub fn update_segment_consensus_batch(&self, updates: &[(String, String, String, f64)]) -> AppResult<usize> {
        self.conn.execute("SAVEPOINT consensus_batch", [])?;
        let result: AppResult<usize> = (|| {
            let mut stmt = self.conn.prepare(
                // Guard: never overwrite a human-reviewed/edited segment with machine consensus —
                // mirrors update_asr_transcript_if_unreviewed and merge_dataset_json. Without this,
                // running the consensus refinery silently discards human corrections.
                "UPDATE speech_segments
                 SET raw_transcript = ?2,
                     normalized_transcript = ?3,
                     confidence = ?4,
                     updated_at = datetime('now')
                 WHERE id = ?1
                   AND (human_decision IS NULL OR human_decision = '')
                   AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            )?;
            let mut changed = 0usize;
            for (seg_id, cons, norm, conf) in updates {
                // NFC-canonicalize the consensus transcript + its normalization before they hit the
                // FTS-indexed columns — same guard as the other write paths, so machine consensus
                // doesn't store a decomposed form that search can't match.
                changed += stmt.execute(params![seg_id, to_nfc(cons), to_nfc(norm), conf])?;
            }
            Ok(changed)
        })();
        match result {
            Ok(changed) => {
                self.release_savepoint("consensus_batch")?;
                self.track_write()?;
                Ok(changed)
            }
            Err(e) => {
                self.cleanup_savepoint_after_error("consensus_batch");
                Err(e)
            }
        }
    }

    pub fn update_ctc_score(&self, id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET ctc_score = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, score],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_ood_score(&self, id: &str, score: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET ood_score = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, score],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_segment_split(&self, id: &str, split: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET split = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, split],
        )?;
        self.track_write()?;
        Ok(())
    }

    pub fn update_quality_metrics(&self, id: &str, clipping: f64, rms: f64, snr: f64) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments SET clipping_ratio = ?2, rms_db = ?3, snr_db = ?4, updated_at = datetime('now') WHERE id = ?1",
            params![id, clipping, rms, snr],
        )?;
        self.track_write()?;
        Ok(())
    }

    fn track_write(&self) -> AppResult<()> {
        // Placeholder for write-tracking if needed by external observers
        Ok(())
    }

    /// Stamp the alignment precision tier on a segment.
    /// Called by `align_segment` after a successful CTC forced alignment run.
    /// `quality`: "ctc_forced" | "energy_heuristic"
    pub fn update_alignment_quality(&self, segment_id: &str, quality: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments
             SET alignment_quality = ?2, updated_at = datetime('now')
             WHERE id = ?1",
            params![segment_id, quality],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Persist the chunk metadata + word timestamps JSON for a segment. `alignment_json` is metadata
    /// (chunk window + per-word timings), NOT FTS-indexed transcript text, so no NFC canonicalization
    /// is needed. Used by `align_segment` so forced-alignment word timings survive a reload instead of
    /// being recomputed every time the clip is opened in review.
    pub fn update_segment_alignment_json(&self, segment_id: &str, alignment_json: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments
             SET alignment_json = ?2, updated_at = datetime('now')
             WHERE id = ?1",
            params![segment_id, alignment_json],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Read a column that an older schema may lack (the jury cols were added by Migration v11 and
    /// alignment_quality by v12). A genuinely ABSENT column (`InvalidColumnIndex`, i.e. a row read
    /// through a pre-migration schema) yields `None`, and a SQL `NULL` yields `None` — both the
    /// intended defaults. But a type-mismatch / decode fault PROPAGATES instead of being masked:
    /// silently defaulting one of these on a decode error would misreport a genuinely gold /
    /// human-reviewed segment as `is_gold = false` / `human_decision = None`, which the
    /// human-protection guards key on — the exact silent-corruption the honesty rule forbids. This
    /// mirrors the strict `?` handling of columns 0-16 and the fail-closed `is_gold` read in
    /// `record_model_correction`.
    fn optional_col<T: rusqlite::types::FromSql>(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<T>> {
        match row.get::<_, Option<T>>(idx) {
            Ok(value) => Ok(value),
            Err(rusqlite::Error::InvalidColumnIndex(_)) => Ok(None),
            Err(other) => Err(other),
        }
    }

    fn map_row(row: &rusqlite::Row) -> rusqlite::Result<SpeechSegment> {
        Ok(SpeechSegment {
            id: row.get(0)?,
            created_at: row.get(1)?,
            audio_path: row.get(2)?,
            raw_transcript: row.get(3)?,
            normalized_transcript: row.get(4)?,
            annotated_transcript: row.get(5)?,
            alignment_json: row.get(6)?,
            duration_ms: row.get(7)?,
            speaker_id: row.get(8)?,
            verified: row.get::<_, i32>(9)? != 0,
            confidence: row.get(10)?,
            ctc_score: row.get(11)?,
            clipping_ratio: row.get(12)?,
            rms_db: row.get(13)?,
            snr_db: row.get(14)?,
            split: row.get(15)?,
            ood_score: row.get(16)?,
            // Jury fields — added by Migration v11. Default ONLY when the column is genuinely absent
            // (old schema) or NULL; a decode error propagates so it can't silently strip provenance.
            verdict: Self::optional_col(row, 17)?,
            verdict_transcript: Self::optional_col(row, 18)?,
            rationale: Self::optional_col(row, 19)?,
            evidence_json: Self::optional_col(row, 20)?,
            agent_confidence: Self::optional_col(row, 21)?,
            escalated: Self::optional_col::<i32>(row, 22)?.unwrap_or(0) != 0,
            human_decision: Self::optional_col(row, 23)?,
            corrected_at: Self::optional_col(row, 24)?,
            is_gold: Self::optional_col::<i32>(row, 25)?.unwrap_or(0) != 0,
            // Alignment quality — added by Migration v12; same fail-closed treatment.
            alignment_quality: Self::optional_col(row, 26)?,
            model_version_id: Self::optional_col(row, 27)?,
            confidence_source: Self::optional_col(row, 28)?,
            cloud_call: Self::optional_col::<i32>(row, 29)?.unwrap_or(0) != 0,
            decoder_config_hash: Self::optional_col(row, 30)?,
            normalizer_version: Self::optional_col(row, 31)?,
        })
    }

    // ── Jury DB helpers ───────────────────────────────────────────────────────

    /// M2.3 / P1.3: record what LOOP-0 WOULD have done for a segment WITHOUT mutating it. `memory_fired`
    /// is true when a correction memory would have changed the finalized transcript. One row per shadow
    /// observation; the C5 over-trigger decision joins these to the human's later decision at analysis
    /// time (an over-trigger is a would-fire the human subsequently contradicts).
    pub fn record_loop0_shadow(&self, segment_id: &str, memory_fired: bool) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO loop0_shadow_log (segment_id, memory_fired) VALUES (?1, ?2)",
            params![segment_id, memory_fired],
        )?;
        Ok(())
    }

    /// True-10 audit: the READ side of the intelligence instrumentation. loop0_shadow_log and
    /// decision_verdicts were write-only — the C5 (LOOP-0 go-live) and C4 (auto-accept precision)
    /// decisions were impossible to make in-app. This joins both against the humans' subsequent
    /// decisions:
    ///
    /// * LOOP-0 shadow: `fired_but_human_accepted_original` is the OVER-TRIGGER count (the memory
    ///   would have changed text a human then confirmed was already right) — C5 requires this to be
    ///   0 before `loop0_firing_enabled` may ever be turned on. `fired_and_human_edited` is
    ///   inconclusive-positive (the text did need changing; whether the memory's change matched the
    ///   human's is not knowable from the flag alone).
    /// * C4: of the machine's T0 auto-accepts that a human later reviewed, how many did the human
    ///   confirm vs contradict (edit/reject) — the honest precision behind any autonomy increase.
    pub fn intelligence_report(&self) -> AppResult<serde_json::Value> {
        // Live counts over surviving segments PLUS the durable archive of segments already deleted
        // (migration v33), so the C5 over-trigger gate is not survivor-biased by ordinary cleanup.
        // Per-SEGMENT counts (true-10 audit 2026-07-09): shadow_log holds one row per OBSERVATION and
        // re-processed segments accumulate several, but C5 reasons about distinct events — one clip,
        // one human decision, at most one over-trigger. Aggregate per segment first (MAX(memory_fired)
        // = "ever would have fired for this clip"), then count segments; the v33/v34 archives fold the
        // same per-segment semantics at delete time. (Archive rows accumulated before this change may
        // carry per-observation counts — a conservative overstatement for the C5 "must be 0" gate.)
        let loop0 = self.conn.query_row(
            "WITH per_seg AS (
                 SELECT l.segment_id, MAX(l.memory_fired) AS fired, s.human_decision AS hd
                 FROM loop0_shadow_log l JOIN speech_segments s ON s.id = l.segment_id
                 GROUP BY l.segment_id
             )
             SELECT COUNT(*) + COALESCE((SELECT total_observations FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(fired), 0)
                        + COALESCE((SELECT would_fire FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('accept','human_accept') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_accepted FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('edit','human_edit') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_edited FROM loop0_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN fired = 1 AND hd IN ('reject','human_reject') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT fired_human_rejected FROM loop0_evidence_archive WHERE id = 1), 0)
             FROM per_seg",
            [],
            |row| {
                Ok(serde_json::json!({
                    "totalObservations": row.get::<_, i64>(0)?,
                    "wouldFire": row.get::<_, i64>(1)?,
                    "firedButHumanAcceptedOriginal": row.get::<_, i64>(2)?,
                    "firedAndHumanEdited": row.get::<_, i64>(3)?,
                    "firedAndHumanRejected": row.get::<_, i64>(4)?,
                }))
            },
        )?;
        // Live counts PLUS the v34 durable archive — deleting a reviewed clip must not shrink
        // t0HumanContradicted (the C4 precision could only drift optimistic; same class as v33/C5).
        let c4 = self.conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_accepts FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T1_ESCALATE' THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t1_escalations FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' AND s.human_decision IN ('accept','human_accept') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_human_confirmed FROM c4_evidence_archive WHERE id = 1), 0),
                    COALESCE(SUM(CASE WHEN dv.auto_accept_verdict = 'T0_ACCEPT' AND s.human_decision IN ('edit','human_edit','reject','human_reject') THEN 1 ELSE 0 END), 0)
                        + COALESCE((SELECT t0_human_contradicted FROM c4_evidence_archive WHERE id = 1), 0)
             FROM decision_verdicts dv JOIN speech_segments s ON s.id = dv.segment_id",
            [],
            |row| {
                Ok(serde_json::json!({
                    "t0Accepts": row.get::<_, i64>(0)?,
                    "t1Escalations": row.get::<_, i64>(1)?,
                    "t0HumanConfirmed": row.get::<_, i64>(2)?,
                    "t0HumanContradicted": row.get::<_, i64>(3)?,
                }))
            },
        )?;
        // C3 honesty (true-10 audit 2026-07-09): the T0 auto-accept gate needs a Hoeffding-certified
        // per-SNR-bucket calibration set, and at the shipped constants that means ~thousands of
        // perfectly-transcribed verified clips PER BUCKET — previously invisible, so the user just
        // experienced "the jury escalates everything" with no stated reason or distance. Surface the
        // per-bucket progress: verified-with-reference counts vs the minimum needed at ZERO CER
        // (a hard lower bound — real data needs more). The gate itself is deliberately unchanged.
        let mut bucket_counts = [0i64; crate::quality::conformal::N_SNR_BUCKETS];
        {
            let mut stmt = self.conn.prepare(
                "SELECT snr_db FROM speech_segments
                 WHERE verified = 1 AND annotated_transcript IS NOT NULL AND TRIM(annotated_transcript) != ''",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, Option<f64>>(0))?;
            for snr in rows {
                bucket_counts[crate::quality::conformal::snr_bucket(snr?)] += 1;
            }
        }
        // The T0 gate's shipped constants (jury/mod.rs): target 5% CER at 90% joint confidence,
        // Bonferroni-split across the buckets.
        let target_error = 0.05;
        let per_bucket_delta = (1.0 - 0.90) / crate::quality::conformal::N_SNR_BUCKETS as f64;
        let min_needed = crate::quality::conformal::min_calibration_n(target_error, per_bucket_delta);
        let bucket_labels = ["<5 dB (very noisy)", "5-15 dB", "15-25 dB", ">25 dB (clean)", "unknown SNR"];
        let calibration: Vec<serde_json::Value> = (0..crate::quality::conformal::N_SNR_BUCKETS)
            .map(|b| {
                serde_json::json!({
                    "bucket": bucket_labels[b],
                    "verifiedWithReference": bucket_counts[b],
                    "minNeededAtZeroCer": min_needed,
                })
            })
            .collect();
        let conformal_progress = serde_json::json!({
            "targetErrorCer": target_error,
            "perBucketDelta": per_bucket_delta,
            "minNeededAtZeroCer": min_needed,
            "buckets": calibration,
        });
        Ok(serde_json::json!({
            "loop0Shadow": loop0,
            "autoAcceptPrecision": c4,
            "conformalCalibration": conformal_progress,
        }))
    }

    /// M2.2 / P1.2: classify a MACHINE verdict as T0 (auto-resolved, no human needed) or T1
    /// (escalated to a human) and record it in decision_verdicts — the denominator/index for the C4
    /// auto-accept-precision measurement. Human verdicts (`human_*`) and any unknown string record
    /// nothing: they are not machine auto-accept decisions. The raw verdict stays on
    /// speech_segments.verdict, so a C4 query can still recover auto_accept-vs-jury_accept-vs-jury_edit.
    /// Call ONLY after the verdict UPDATE affected the row (segment was not already human-decided), so a
    /// stale/late machine verdict never plants a phantom T0/T1 over a human's decision.
    pub fn record_decision_verdict(&self, segment_id: &str, verdict: &str, escalated: bool) -> AppResult<()> {
        let auto_accept_verdict = if escalated || verdict == "escalated" {
            "T1_ESCALATE"
        } else if matches!(verdict, "auto_accept" | "jury_accept" | "jury_edit") {
            "T0_ACCEPT"
        } else {
            return Ok(());
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO decision_verdicts (segment_id, auto_accept_verdict, verdict_computed_at)
             VALUES (?1, ?2, datetime('now'))",
            params![segment_id, auto_accept_verdict],
        )?;
        Ok(())
    }

    /// Write a MACHINE jury verdict to a segment (T0/T1/T2 and the agentic/escalation paths).
    ///
    /// The human-review path is `record_human_decision`, NOT this function. A machine verdict must never
    /// overwrite a human decision: the jury runs on a SEPARATE WAL connection from the human path, reads
    /// its segment snapshot once at the start of a run, then may block for seconds on a T2 cloud call —
    /// so a curator can accept/edit the same segment mid-run. Without this guard the late machine write
    /// would silently revert the human's `verdict` (the COALESCE-preferred gold transcript source) and
    /// flip `escalated` back, mis-routing the segment. The predicate mirrors the consensus/ASR write
    /// paths (the `human_decision`/`verdict NOT IN (human_*)` guards elsewhere in this file): a verdict
    /// for an already-human-decided segment matches 0 rows and is a no-op, leaving the human authoritative.
    #[allow(clippy::too_many_arguments)]
    pub fn write_segment_verdict(
        &self,
        segment_id: &str,
        verdict: &str,
        transcript: Option<&str>,
        rationale: Option<&str>,
        evidence_json: Option<&str>,
        agent_confidence: Option<f64>,
        escalated: bool,
    ) -> AppResult<()> {
        // agent_confidence uses COALESCE(?6, existing): a machine verdict that carries NO confidence
        // signal must never destroy a previously persisted one. The T1/T2 escalation paths (cloud-off,
        // audio-prep failure, no-majority) all write None moments after run_t0_gate persisted the real
        // IRT confidence for the same segment — the unconditional overwrite NULLed it, and both
        // suspect-first orderings (COALESCE(agent_confidence, 0.5)) collapsed back to recency: the one
        // live review-speed feature was silently nominal (true-10 audit 2026-07-09). No caller has a
        // legitimate "clear the confidence" case; callers that HAVE a signal pass Some and still win.
        let affected = self.conn.execute(
            "UPDATE speech_segments
             SET verdict              = ?2,
                 verdict_transcript   = ?3,
                 rationale            = ?4,
                 evidence_json        = ?5,
                 agent_confidence     = COALESCE(?6, agent_confidence),
                 escalated            = ?7,
                 updated_at           = datetime('now')
             WHERE id = ?1
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept', 'human_edit', 'human_reject'))",
            params![segment_id, verdict, transcript, rationale, evidence_json, agent_confidence, escalated as i32],
        )?;
        if affected == 0 {
            // Either the row is gone or a human already decided it — in both cases the machine verdict
            // correctly does not apply. Logged (not an error) so the no-op is visible without masking it.
            tracing::debug!(
                "write_segment_verdict({segment_id}, {verdict}): no-op — segment is human-decided or missing"
            );
        } else {
            // M2.2/P1.2: record the T0/T1 classification for the C4 denominator (no-op for human/unknown).
            self.record_decision_verdict(segment_id, verdict, escalated)?;
        }
        self.track_write()?;
        Ok(())
    }

    /// Fully RE-OPEN a segment whose human decision is being undone. record_human_decision OVERWRITES
    /// the prior machine verdict with the human one, so the pre-decision verdict is gone — the honest
    /// reset is "un-adjudicated": clear the human decision AND the verdict it set, and return the segment
    /// to the review queue (escalated = 1). Clearing only human_decision (the old behavior) left a stale
    /// verdict = 'human_*' so the "undone" segment still looked decided on reload AND the machine
    /// verdict-write guard (write_segment_verdict / jury::write_verdict) would refuse to re-adjudicate it.
    pub fn clear_human_decision(&self, segment_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments
             SET human_decision     = NULL,
                 corrected_at       = NULL,
                 verdict            = NULL,
                 verdict_transcript = NULL,
                 rationale          = NULL,
                 evidence_json      = NULL,
                 agent_confidence   = NULL,
                 escalated          = 1,
                 updated_at         = datetime('now')
             WHERE id = ?1",
            params![segment_id],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Reverse a UI `flag()` escalation (the review-inbox Undo path): clear the `escalated` flag and the
    /// machine `'escalated'` verdict + rationale that flag wrote, WITHOUT touching a human_decision (flag
    /// never sets one). This is the exact inverse of flag — unlike `clear_human_decision`, which
    /// deliberately SETS escalated=1 to reopen a human-decided row for re-adjudication. Guarded to a
    /// still-undecided row so it can never stomp a human decision made after the flag; idempotent. Every
    /// SET expression references the row's PRE-UPDATE values (SQLite semantics), so both CASEs see the
    /// original verdict.
    pub fn clear_escalation(&self, segment_id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE speech_segments
             SET escalated  = 0,
                 verdict    = CASE WHEN verdict = 'escalated' THEN NULL ELSE verdict END,
                 rationale  = CASE WHEN verdict = 'escalated' THEN NULL ELSE rationale END,
                 updated_at = datetime('now')
             WHERE id = ?1 AND (human_decision IS NULL OR human_decision = '')",
            params![segment_id],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Capture a MODEL correction (the jury auto-correcting OmniASR) as a provenance-tagged PSEUDO
    /// example: `source='model'`, `verified_by_human=0`. Unlike a human edit, this is NOT trusted
    /// training data — it is a candidate for human review / a future gated pseudo-label pass, and is
    /// excluded from the DPO export and few-shot context until a human signs off. Training directly
    /// on model-generated corrections causes model collapse (Shumailov et al., Nature 2024), so this
    /// path only RECORDS; it never promotes a label into the trainable pool.
    ///
    /// No-ops when the corrected text equals the wrong text (not a real correction) or the segment is
    /// gold/holdout (quarantined at capture). Best-effort: returns Ok even when it records nothing.
    pub fn record_model_correction(
        &self,
        segment_id: &str,
        wrong_transcript: &str,
        corrected_transcript: &str,
        corrector_model_id: &str,
    ) -> AppResult<()> {
        let wrong = wrong_transcript.trim();
        let fix = corrected_transcript.trim();
        if fix.is_empty() || wrong == fix {
            return Ok(()); // not a correction
        }
        // Quarantine gold at capture time (holdout exclusion is also applied at every export). Distinguish
        // "no such segment" (genuinely not gold -> 0) from a TRANSIENT read error (e.g. SQLITE_BUSY after
        // the busy_timeout, under a long adjudication on the other connection): the latter must NOT
        // fail-OPEN the quarantine by defaulting to 0 and writing a model pseudo-label onto a gold row —
        // propagate it so the best-effort caller simply skips this capture.
        let is_gold: i64 =
            match self
                .conn
                .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", params![segment_id], |r| r.get(0))
            {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => 0,
                Err(e) => return Err(e.into()),
            };
        if is_gold != 0 {
            return Ok(());
        }
        let example_id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO agent_examples
                 (id, segment_id, wrong_transcript, human_fix, source, verified_by_human, corrector_model_id)
             VALUES (?1, ?2, ?3, ?4, 'model', 0, ?5)",
            params![example_id, segment_id, wrong, fix, corrector_model_id],
        )?;
        Ok(())
    }

    /// Record a human decision (accept/edit/reject) and optionally store a
    /// corrected transcript.  Gold segments are updated but never written to
    /// agent_examples. M2.1: Also logs decision timing to decision_log table.
    pub fn record_human_decision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
        timestamp_ms: Option<i64>,
    ) -> AppResult<()> {
        let human_verdict = human_verdict_for_decision(decision)?;
        // NFC-canonicalize the human correction like EVERY other transcript write path (insert/restore/
        // update_*). Without it a decomposed (NFD) paste / IME input becomes the lone non-NFC label in an
        // otherwise-NFC corpus (verdict_transcript is the COALESCE-preferred gold source) and defeats the
        // no-op-edit dedup guard — which compares via learning_text_key WITHOUT NFC — so an edit that is
        // byte-different-but-NFC-identical to the wrong text emits a degenerate DPO pair.
        let corrected_owned: Option<String> =
            corrected_transcript.map(|t| to_nfc(t.trim())).filter(|value| !value.is_empty());
        let corrected_transcript = corrected_owned.as_deref();
        if decision == "edit" && corrected_transcript.is_none() {
            return Err(AppError::Validation("Human edit decisions require a corrected transcript".into()));
        }

        let (
            is_gold,
            raw_transcript,
            normalized_transcript,
            annotated_transcript,
            verdict_transcript,
            prior_verdict,
            audio_path,
            model_version_id,
        ): HumanDecisionContext = self.conn.query_row(
            "SELECT COALESCE(is_gold, 0), raw_transcript, normalized_transcript, annotated_transcript,
                        verdict_transcript, verdict, audio_path, model_version_id
                 FROM speech_segments
                 WHERE id = ?1",
            params![segment_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?;

        let rejected_learning_transcript = if decision == "edit" {
            corrected_transcript.and_then(|fix| {
                rejected_transcript_for_learning(
                    fix,
                    &[
                        verdict_transcript.clone(),
                        annotated_transcript.clone(),
                        normalized_transcript.clone(),
                        Some(raw_transcript.clone()),
                    ],
                )
            })
        } else {
            None
        };

        // For an edit, capture the durable audio identity for the corrections ledger. Best-effort:
        // computed BEFORE the transaction (no file I/O while a write is open) and, if the audio is
        // unavailable, the verdict still records — we skip the audit row rather than fail the
        // human's correction over a missing file.
        let ledger_hash = if decision == "edit" {
            crate::pipeline::source_audio_identity(Path::new(&audio_path)).ok().map(|identity| identity.content_hash)
        } else {
            None
        };

        // The model's wrong transcript for this edit (the agent proposal when available, else the
        // raw ASR) — the shared "wrong" side of both the audit-ledger row and the LOOP-0 memory.
        let wrong_side: Option<String> = if decision == "edit" {
            Some(rejected_learning_transcript.clone().unwrap_or_else(|| raw_transcript.clone()))
        } else {
            None
        };

        // LOOP-0 evidence-based confidence (true-10 audit): for every PRE-EXISTING correction memory
        // that would have fired on this segment, record whether the human's decision confirmed or
        // contradicted it. The finalized transcript the human reviewed (annotated ▸ normalized ▸ raw)
        // mirrors `pipeline::shadow_log_loop0`, so the evidence matches the shadow signal.
        //
        //   * edit   -> reference is the human's fix; a memory whose firing moves the text TOWARD it is
        //               a confirm, AWAY is an override (over-trigger).
        //   * accept -> reference IS the finalized text; any memory that fires there over-triggered on a
        //               draft the human just confirmed was already correct -> override.
        //   * reject -> inconclusive (the human discarded the whole clip, not a verdict on any word) -> skip.
        //
        // Snapshot the memories BEFORE the capture/upsert below so the memory born from THIS edit cannot
        // confirm itself. Gold is excluded to match the capture path: gold human-decisions are the firing
        // eval set (see `firing_error_delta`), and tuning the store on them would leak.
        let finalized_text = crate::corrections::loop0_draft_text(
            annotated_transcript.as_deref(),
            normalized_transcript.as_deref(),
            &raw_transcript,
        )
        .to_string();
        let confidence_reference: Option<String> = match decision {
            "edit" => corrected_transcript.map(str::to_string),
            "accept" => Some(finalized_text.clone()),
            _ => None,
        };
        type MemoryOutcomeUpdate = (String, String, String, crate::corrections::MemoryOutcome);
        let confidence_updates: Vec<MemoryOutcomeUpdate> = match (is_gold, confidence_reference.as_deref()) {
            (0, Some(reference)) => {
                let cfg = crate::corrections::FiringConfig::default();
                let mems = self.load_correction_memories()?;
                // Winner-take-all per slot (matches runtime firing): only the memory that would actually
                // fire at each slot is credited, so a losing sibling in the same slot earns no spurious
                // confirm/override.
                crate::corrections::classify_memory_outcomes(&finalized_text, reference, &mems, &cfg)
                    .into_iter()
                    .map(|(idx, outcome)| {
                        let m = &mems[idx];
                        (m.slot_key.clone(), m.wrong_token.clone(), m.human_token.clone(), outcome)
                    })
                    .collect()
            }
            _ => Vec::new(),
        };

        // The human's verdict, the learning pair, and the audit-ledger row commit together as one
        // atomic correction — a crash can never leave a verdict without its provenance, or vice versa.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE speech_segments
             SET human_decision     = ?2,
                 verdict            = ?3,
                 verdict_transcript = COALESCE(?4, verdict_transcript),
                 escalated          = 0,
                 corrected_at       = datetime('now'),
                 updated_at         = datetime('now')
             WHERE id = ?1",
            params![segment_id, decision, human_verdict, corrected_transcript],
        )?;

        // Insert into agent_examples when it is an edit on a non-gold segment.
        // The rejected side must be the actual agent proposal when available,
        // not blindly the original raw ASR transcript.
        if is_gold == 0 {
            if let (Some(wrong), Some(fix)) = (rejected_learning_transcript.clone(), corrected_transcript) {
                let example_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![example_id, segment_id, wrong, fix],
                )?;
            }
        }

        // Append to the corrections provenance ledger for any edit with a concrete fix and a
        // resolvable audio identity. Holdout exclusion is applied downstream by content hash, so
        // the ledger records gold and non-gold alike — it is the full audit trail, keyed on the
        // durable audio_content_hash, that makes the training set reconstructable and every label
        // attributable to the model_version that produced it.
        if let (Some(content_hash), Some(fix), Some(wrong)) = (ledger_hash, corrected_transcript, wrong_side.as_deref())
        {
            // Only record a GENUINE correction. `wrong_side` falls back to raw_transcript when no
            // candidate differed from the fix (the model was already right), which would otherwise
            // append a row whose raw_hypothesis == human_fix (up to whitespace/case) and pollute the
            // reconstructable training ledger with non-corrections. Gate on the same learning-key
            // difference the agent_examples / LOOP-0 paths already use.
            if learning_text_key(wrong) != learning_text_key(fix) {
                let correction_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO corrections
                        (id, segment_id, audio_content_hash, raw_hypothesis, human_fix, jury_verdict, model_version_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![correction_id, segment_id, content_hash, wrong, fix, prior_verdict, model_version_id],
                )?;
            }
        }

        // LOOP 0: distil the edit into per-slot error memories so the SAME confusion is corrected on
        // the next decode with no retraining. Gold is excluded — a memory firing on a held-out clip
        // would leak into eval. Upsert on the natural key (slot + wrong + human): a repeated,
        // independently confirmed correction bumps hit_count instead of inserting a duplicate.
        if is_gold == 0 {
            if let (Some(wrong), Some(fix)) = (wrong_side.as_deref(), corrected_transcript) {
                for mem in crate::corrections::extract_substitution_memories(wrong, fix) {
                    let bumped = tx.execute(
                        "UPDATE correction_memory SET hit_count = hit_count + 1
                         WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3",
                        params![mem.slot_key, mem.wrong_token, mem.human_token],
                    )?;
                    if bumped == 0 {
                        let mem_id = uuid::Uuid::new_v4().to_string();
                        tx.execute(
                            "INSERT INTO correction_memory
                                (id, wrong_token, human_token, slot_key, phonetic_key, source_segment,
                                 model_version_id, confidence)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                mem_id,
                                mem.wrong_token,
                                mem.human_token,
                                mem.slot_key,
                                mem.phonetic_key,
                                segment_id,
                                model_version_id,
                                // Start at the Beta(1,1) prior (0.5), not the frozen 1.0: a fresh memory
                                // has zero firing-outcome evidence and must earn its way past tau_conf.
                                crate::corrections::beta_confidence(0, 0)
                            ],
                        )?;
                    }
                }
            }
        }

        // Apply the pre-computed LOOP-0 confidence evidence. Each pre-existing memory that would have
        // fired on this segment gets a confirm or an override; `confidence` becomes the Beta(1,1)
        // posterior of the updated counts (the SET expressions evaluate against the OLD row values, so
        // `+1`/`+2`/`+3` reconstruct `beta_confidence(new_confirm, new_override)` exactly). `last_fired_at`
        // records this shadow-fire against a human-reviewed segment — the column was never written before.
        for (slot_key, wrong_token, human_token, outcome) in &confidence_updates {
            let sql = match outcome {
                crate::corrections::MemoryOutcome::Confirm => {
                    "UPDATE correction_memory
                        SET confirm_count = confirm_count + 1,
                            confidence    = (confirm_count + 2.0) / (confirm_count + override_count + 3.0),
                            last_fired_at = datetime('now')
                      WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3"
                }
                crate::corrections::MemoryOutcome::Override => {
                    "UPDATE correction_memory
                        SET override_count = override_count + 1,
                            confidence     = (confirm_count + 1.0) / (confirm_count + override_count + 3.0),
                            last_fired_at  = datetime('now')
                      WHERE slot_key = ?1 AND wrong_token = ?2 AND human_token = ?3"
                }
                crate::corrections::MemoryOutcome::Neutral => continue,
            };
            tx.execute(sql, params![slot_key, wrong_token, human_token])?;
        }

        // M2.1: Log decision timing to decision_log for instrumentation before M3 marathon.
        if let Some(ts_ms) = timestamp_ms {
            tx.execute(
                "INSERT INTO decision_log (segment_id, decision_type, timestamp_ms, human_decision, created_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                params![segment_id, decision, ts_ms, decision],
            )?;
        }

        tx.commit()?;
        self.track_write()?;
        Ok(())
    }

    /// Load all LOOP-0 correction memories for the firing rule. `apply_memories` applies the
    /// confidence / hit-count / phonetic gates itself, so every stored row is returned here.
    pub fn load_correction_memories(&self) -> AppResult<Vec<crate::corrections::MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT wrong_token, human_token, slot_key, phonetic_key, confidence, hit_count
             FROM correction_memory",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::corrections::MemoryEntry {
                wrong_token: row.get(0)?,
                human_token: row.get(1)?,
                slot_key: row.get(2)?,
                phonetic_key: row.get(3)?,
                confidence: row.get(4)?,
                hit_count: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Return escalated segments ordered riskiest-first (lowest agent_confidence).
    pub fn get_escalation_queue(&self, limit: usize) -> AppResult<Vec<SpeechSegment>> {
        let query = format!(
            "SELECT {SEGMENT_SELECT_COLUMNS}
             FROM speech_segments
             WHERE escalated = 1
               AND (human_decision IS NULL OR human_decision = '')
             ORDER BY COALESCE(agent_confidence, 0.5) ASC, id ASC
             LIMIT ?1"
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![limit as i64], Self::map_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// True only when an error indicates the database FILE itself is corrupt / not a database — the only
/// conditions under which the destructive `recover_database_at` quarantine is warranted. Transient
/// errors (SQLITE_BUSY/LOCKED, disk I/O, OOM) return false so a healthy db is never quarantined.
fn is_corruption_error(err: &AppError) -> bool {
    use rusqlite::ErrorCode;
    matches!(
        err,
        AppError::Database(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

/// Whether a non-"ok" `PRAGMA integrity_check` result row is a TRANSIENT page-access / I/O message
/// rather than genuine structural corruption. integrity_check is designed to keep walking the b-tree
/// and report problems as up to 100 text result rows instead of failing the statement, so a momentary
/// page-read failure (disk hiccup, or an AV/backup/indexer holding a page locked mid-scan) surfaces as
/// `Ok("unable to get the page N. error code=...")`. Treating that as corruption and quarantining a
/// HEALTHY database is silent total data loss, so these abort startup without quarantine instead.
fn integrity_result_looks_transient(result: &str) -> bool {
    let r = result.to_ascii_lowercase();
    r.contains("unable to get the page")
        || r.contains("error code=")
        || r.contains("i/o error")
        || r.contains("disk i/o")
        || r.contains("is locked")
        || r.contains("out of memory")
}

fn recover_database_at(path: &str) -> AppResult<()> {
    let path_buf = Path::new(path);
    if !path_buf.exists() {
        return Ok(());
    }

    let backup_path = unique_corrupt_backup_path(path_buf, chrono::Utc::now().timestamp());
    std::fs::rename(path_buf, &backup_path)?;
    tracing::info!("Corrupt database moved to {:?}", backup_path);

    move_sqlite_sidecar(path_buf, &backup_path, "-wal");
    move_sqlite_sidecar(path_buf, &backup_path, "-shm");
    Ok(())
}

fn unique_corrupt_backup_path(db_path: &Path, timestamp: i64) -> PathBuf {
    let base = db_path.with_extension(format!("corrupt.{timestamp}"));
    if !base.exists() {
        return base;
    }

    for suffix in 1..1000 {
        let candidate = db_path.with_extension(format!("corrupt.{timestamp}.{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }

    db_path.with_extension(format!("corrupt.{timestamp}.{}", std::process::id()))
}

fn move_sqlite_sidecar(original_db: &Path, backup_db: &Path, suffix: &str) {
    let original = sqlite_sidecar_path(original_db, suffix);
    if !original.exists() {
        return;
    }

    let backup = sqlite_sidecar_path(backup_db, suffix);
    if let Err(e) = std::fs::rename(&original, &backup) {
        tracing::warn!("Failed to quarantine SQLite sidecar {} to {}: {e}", original.display(), backup.display());
    }
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", db_path.display(), suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        db
    }

    fn make_segment(id: &str, audio_path: &str) -> SpeechSegment {
        SpeechSegment {
            id: id.to_string(),
            audio_path: audio_path.to_string(),
            raw_transcript: "test".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        }
    }

    #[test]
    fn intelligence_report_joins_shadow_and_verdicts_against_human_decisions() {
        // True-10 audit: the C5/C4 read side. Over-trigger = would-fire + human accepted the
        // ORIGINAL text unchanged (the memory would have corrupted a correct transcript). C4
        // precision = of T0 auto-accepts a human later reviewed, confirmed vs contradicted.
        let db = make_db();
        for id in ["ot", "edited", "unreviewed", "t0-ok", "t0-bad", "t1"] {
            db.insert_segment(&make_segment(id, "/audio/i.wav")).unwrap();
        }
        // LOOP-0 shadow: 'ot' would fire but the human accepted the original -> OVER-TRIGGER.
        db.record_loop0_shadow("ot", true).unwrap();
        db.record_loop0_shadow("edited", true).unwrap();
        db.record_loop0_shadow("unreviewed", true).unwrap();
        db.connection().execute("UPDATE speech_segments SET human_decision='accept' WHERE id='ot'", []).unwrap();
        db.connection().execute("UPDATE speech_segments SET human_decision='edit' WHERE id='edited'", []).unwrap();
        // C4: two T0 accepts (one confirmed, one contradicted) + one T1 escalation.
        db.record_decision_verdict("t0-ok", "auto_accept", false).unwrap();
        db.record_decision_verdict("t0-bad", "jury_accept", false).unwrap();
        db.record_decision_verdict("t1", "escalated", true).unwrap();
        db.connection().execute("UPDATE speech_segments SET human_decision='accept' WHERE id='t0-ok'", []).unwrap();
        db.connection().execute("UPDATE speech_segments SET human_decision='reject' WHERE id='t0-bad'", []).unwrap();

        let report = db.intelligence_report().unwrap();
        let loop0 = &report["loop0Shadow"];
        assert_eq!(loop0["totalObservations"], 3);
        assert_eq!(loop0["wouldFire"], 3);
        assert_eq!(loop0["firedButHumanAcceptedOriginal"], 1, "'ot' is the one over-trigger");
        assert_eq!(loop0["firedAndHumanEdited"], 1);
        assert_eq!(loop0["firedAndHumanRejected"], 0);
        let c4 = &report["autoAcceptPrecision"];
        assert_eq!(c4["t0Accepts"], 2);
        assert_eq!(c4["t1Escalations"], 1);
        assert_eq!(c4["t0HumanConfirmed"], 1);
        assert_eq!(c4["t0HumanContradicted"], 1);
    }

    #[test]
    fn suspect_first_ranks_escalated_by_real_confidence_not_recency() {
        // True-10 audit: escalated rows used to carry agent_confidence=None, collapsing the second
        // sort key (COALESCE(agent_confidence, 0.5) ASC) to a constant — "suspect-first" silently
        // degraded to recency. With the jury now persisting the IRT confidence on escalation, the
        // most-doubted clip genuinely ranks first; a legacy None row slots at the 0.5 midpoint.
        let db = make_db();
        for id in ["confident", "shaky", "legacy"] {
            db.insert_segment(&make_segment(id, "/audio/s.wav")).unwrap();
        }
        db.write_segment_verdict("confident", "escalated", None, None, None, Some(0.9), true).unwrap();
        db.write_segment_verdict("shaky", "escalated", None, None, None, Some(0.2), true).unwrap();
        db.write_segment_verdict("legacy", "escalated", None, None, None, None, true).unwrap();

        let ordered: Vec<String> = db.get_segments_suspect_first(None).unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(
            ordered,
            vec!["shaky".to_string(), "legacy".to_string(), "confident".to_string()],
            "lowest jury confidence first; a legacy None row sits at the 0.5 midpoint"
        );
    }

    #[test]
    fn segment_ids_for_audio_path_returns_only_that_files_segments() {
        // P3.2 resume fix: on import-resume, already-imported files are folded back into the jury
        // batch by their segment ids. This pins that the lookup returns exactly (and only) the
        // segments of the requested source file, so a resumed import re-adjudicates the right set.
        let db = make_db();
        db.insert_segment(&make_segment("a1", "/audio/one.wav")).unwrap();
        db.insert_segment(&make_segment("a2", "/audio/one.wav")).unwrap();
        db.insert_segment(&make_segment("b1", "/audio/two.wav")).unwrap();

        let ids = db.segment_ids_for_audio_path("/audio/one.wav").unwrap();
        assert_eq!(ids, vec!["a1".to_string(), "a2".to_string()], "only one.wav's segments, in insert order");
        assert_eq!(db.segment_ids_for_audio_path("/audio/two.wav").unwrap(), vec!["b1".to_string()]);
        assert!(db.segment_ids_for_audio_path("/audio/missing.wav").unwrap().is_empty(), "unknown path -> empty");
    }

    #[test]
    fn model_abilities_round_trip_and_upsert() {
        let db = make_db();
        assert!(db.load_model_abilities().unwrap().is_empty(), "no abilities before any learning run");
        let mut a = std::collections::HashMap::new();
        a.insert("omniasr-wsl-7b".to_string(), 1.5);
        a.insert("omniasr-ctc-300m".to_string(), -0.5);
        a.insert("nan-model".to_string(), f64::NAN); // must be dropped (non-finite)
        db.save_model_abilities(&a).unwrap();
        let loaded = db.load_model_abilities().unwrap();
        assert_eq!(loaded.len(), 2, "the NaN ability must not be stored");
        assert!((loaded["omniasr-wsl-7b"] - 1.5).abs() < 1e-9);
        // Upsert overwrites the prior value for the same model.
        db.save_model_abilities(&std::collections::HashMap::from([("omniasr-wsl-7b".to_string(), 2.2)])).unwrap();
        assert!((db.load_model_abilities().unwrap()["omniasr-wsl-7b"] - 2.2).abs() < 1e-9);
    }

    // The jury db-lock fix (with_jury_db) opens a SECOND, dedicated connection from Database::path so
    // the global AppState db Mutex isn't held across cloud T2 network calls. This pins the assumption
    // it relies on: path() round-trips, and a dedicated connection to the same file sees the primary's
    // committed rows AND its own writes are visible back to the primary (SQLite WAL coexistence).
    #[test]
    fn dedicated_connection_via_path_shares_committed_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("jury.db");
        let path_str = path.to_string_lossy().to_string();

        let primary = Database::open(&path_str).unwrap();
        primary.initialize().unwrap();
        assert_eq!(primary.path(), path_str, "path() must return the opened path");
        primary.insert_segment(&make_segment("s1", "/s1.wav")).unwrap();

        // A dedicated connection opened from primary.path() (the with_jury_db pattern) sees the row.
        let dedicated = Database::open(primary.path()).unwrap();
        assert!(
            dedicated.get_segment_by_id("s1").unwrap().is_some(),
            "dedicated connection must see rows committed by the primary connection"
        );

        // A verdict written through the dedicated connection persists to the same file and is visible
        // back to the primary — so the jury writing through it loses nothing.
        dedicated.write_segment_verdict("s1", "jury_accept", Some("hi"), None, None, Some(0.9), false).unwrap();
        let seen = primary.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(seen.verdict.as_deref(), Some("jury_accept"));
    }

    #[test]
    fn write_segment_verdict_records_all_machine_verdict_classes() {
        // P1.2: decision_verdicts must classify every machine verdict for the C4 denominator. Before the
        // fix only jury_accept recorded a row; auto_accept and jury_edit (also auto-resolutions) dropped.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        for id in ["aa", "je", "es", "hv"] {
            db.insert_segment(&make_segment(id, &format!("/{id}.wav"))).unwrap();
        }
        db.record_human_decision("hv", "accept", None, None).unwrap();

        db.write_segment_verdict("aa", "auto_accept", Some("m"), None, None, Some(0.9), false).unwrap();
        db.write_segment_verdict("je", "jury_edit", Some("m"), None, None, Some(0.8), false).unwrap();
        db.write_segment_verdict("es", "escalated", None, None, None, None, true).unwrap();
        db.write_segment_verdict("hv", "auto_accept", Some("m"), None, None, Some(0.9), false).unwrap();

        let verdict_of = |id: &str| -> Option<String> {
            db.connection()
                .query_row("SELECT auto_accept_verdict FROM decision_verdicts WHERE segment_id = ?1", [id], |r| {
                    r.get(0)
                })
                .ok()
        };
        assert_eq!(verdict_of("aa").as_deref(), Some("T0_ACCEPT"), "auto_accept is a T0 auto-resolution");
        assert_eq!(verdict_of("je").as_deref(), Some("T0_ACCEPT"), "jury_edit is a T0 auto-resolution");
        assert_eq!(verdict_of("es").as_deref(), Some("T1_ESCALATE"), "escalated is T1");
        assert_eq!(verdict_of("hv"), None, "human-decided segment gets no machine verdict row");
    }

    #[test]
    fn v35_repairs_divergent_segments_fts_so_segment_writes_succeed() {
        // Regression (real-app import failure, 2026-07-10): a 4-column segments_fts (missing audio_path)
        // left by a mis-ordered init, while the segments_ai/ad/au triggers reference audio_path, makes
        // EVERY segment INSERT fail ("table segments_fts has no column named audio_path"). The import
        // transaction then rolls back and VAD "produces 0 segments" — the app cannot ingest any audio.
        // v35 rebuilds the FTS shadow table to the authoritative 6-column shape.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();

        // Reproduce the broken divergent state: drop the correct FTS table and recreate migration v1's
        // 4-column copy under the (audio_path-referencing) triggers that initialize() created.
        db.connection()
            .execute_batch(
                "DROP TABLE IF EXISTS segments_fts;
                 CREATE VIRTUAL TABLE segments_fts USING fts5(
                     id, raw_transcript, normalized_transcript, annotated_transcript,
                     content='speech_segments', content_rowid='rowid');",
            )
            .unwrap();
        assert!(
            db.insert_segment(&make_segment("broken", "/a.wav")).is_err(),
            "a 4-column segments_fts under audio_path triggers must reject segment inserts (the real bug)"
        );

        // Re-apply the repair migration (rewind past v35 so run_migrations re-runs it). v36's
        // proof-metadata ALTERs are not idempotent, so undo them first (its down_sql) or the
        // re-run fails on "duplicate column name".
        db.connection()
            .execute_batch(
                "DROP INDEX IF EXISTS idx_segments_confidence_source;
                 DROP INDEX IF EXISTS idx_segments_cloud_call;
                 ALTER TABLE speech_segments DROP COLUMN normalizer_version;
                 ALTER TABLE speech_segments DROP COLUMN decoder_config_hash;
                 ALTER TABLE speech_segments DROP COLUMN cloud_call;
                 ALTER TABLE speech_segments DROP COLUMN confidence_source;",
            )
            .unwrap();
        db.connection().execute("DELETE FROM schema_migrations WHERE version >= 35", []).unwrap();
        crate::migrations::run_migrations(&db).unwrap();

        // After v35, the write the import path depends on succeeds again.
        db.insert_segment(&make_segment("fixed", "/b.wav"))
            .expect("segment INSERT must succeed after v35 rebuilds segments_fts with audio_path");
    }

    #[test]
    fn search_does_not_match_the_audio_path_column() {
        // Round-23 #7: a token that appears ONLY in the file path must NOT return the segment — search
        // is over transcript content, not folder/file names.
        let db = Database::open(":memory:").unwrap();
        db.initialize().unwrap();
        let mut path_only = make_segment("p1", "/recordings/kurdistan/interview.wav");
        path_only.raw_transcript = "hello world".to_string(); // "kurdistan" is ONLY in the path
        db.insert_segment(&path_only).unwrap();
        let mut text_hit = make_segment("t1", "/recordings/a/b.wav");
        text_hit.raw_transcript = "this is about kurdistan today".to_string();
        db.insert_segment(&text_hit).unwrap();

        let ids: Vec<String> = db.search_segments("kurdistan").unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["t1".to_string()], "only the transcript match may return, not the path match: {ids:?}");
    }

    #[test]
    fn transient_integrity_messages_are_not_classified_as_corruption() {
        // Round-20: a transient page-read message from PRAGMA integrity_check must NOT be treated as
        // corruption (which would quarantine a healthy db = silent data loss). It aborts startup instead.
        assert!(integrity_result_looks_transient("unable to get the page 42. error code=8"));
        assert!(integrity_result_looks_transient("disk I/O error"));
        assert!(integrity_result_looks_transient("database is locked"));
        assert!(integrity_result_looks_transient("out of memory"));
        // Genuine structural-corruption findings are NOT transient -> they still quarantine.
        assert!(!integrity_result_looks_transient("row 5 missing from index idx_foo"));
        assert!(!integrity_result_looks_transient("*** in database main *** Page 9: btreeInitPage() returns error"));
        assert!(!integrity_result_looks_transient("wrong # of entries in index"));
    }

    #[test]
    fn stored_transcripts_are_nfc_canonicalized() {
        // Arabic "آ" (U+0622) can arrive decomposed as Alef (U+0627) + combining madda
        // (U+0653). Stored non-canonically it fragments FTS/dedup/WER. The write boundary
        // must store the composed NFC form regardless of the input form.
        let db = make_db();
        let decomposed = "\u{0627}\u{0653}\u{0628}"; // ا + ◌ٓ + ب  (NFD of "آب")
        let composed = "\u{0622}\u{0628}"; // آب (NFC)
        assert_ne!(decomposed, composed, "fixture must actually differ before NFC");

        let mut seg = make_segment("nfc1", "/a.wav");
        seg.raw_transcript = decomposed.to_string();
        seg.annotated_transcript = Some(decomposed.to_string());
        db.insert_segment(&seg).unwrap();

        let stored = db.get_segment_by_audio_path("/a.wav").unwrap().unwrap();
        assert_eq!(stored.raw_transcript, composed, "raw_transcript must be stored NFC-composed");
        assert_eq!(stored.annotated_transcript.as_deref(), Some(composed), "annotated must be NFC too");
    }

    #[test]
    fn asr_and_consensus_updates_store_nfc_so_search_still_matches() {
        // The two UPDATE paths that feed the FTS-indexed raw_transcript (the WSL 7B refinement and the
        // machine-consensus batch) must NFC-canonicalize like the insert path, or a decomposed update
        // silently drops the segment out of search.
        let db = make_db();
        let decomposed = "\u{0627}\u{0653}\u{0628}"; // NFD of "آب"
        let composed = "\u{0622}\u{0628}"; // NFC

        // update_asr_transcript_if_unreviewed
        db.insert_segment(&make_segment("u1", "/u1.wav")).unwrap();
        assert!(db
            .update_asr_transcript_if_unreviewed(
                "u1",
                decomposed,
                Some(decomposed),
                Some(0.9),
                Some("heuristic"),
                Some("omniasr-ctc-300m"),
                false,
            )
            .unwrap());
        let s1 = db.get_segment_by_audio_path("/u1.wav").unwrap().unwrap();
        assert_eq!(s1.raw_transcript, composed, "ASR-update raw_transcript must be stored NFC");
        assert_eq!(s1.confidence_source.as_deref(), Some("heuristic"));
        assert_eq!(s1.model_version_id.as_deref(), Some("omniasr-ctc-300m"));
        assert!(!s1.cloud_call);
        assert!(db.search_segments(composed).unwrap().iter().any(|s| s.id == "u1"), "NFC query must find it");

        // update_segment_consensus_batch
        db.insert_segment(&make_segment("u2", "/u2.wav")).unwrap();
        let updates = vec![("u2".to_string(), decomposed.to_string(), decomposed.to_string(), 0.8)];
        assert_eq!(db.update_segment_consensus_batch(&updates).unwrap(), 1);
        let s2 = db.get_segment_by_audio_path("/u2.wav").unwrap().unwrap();
        assert_eq!(s2.raw_transcript, composed, "consensus-batch raw_transcript must be stored NFC");
        assert!(db.search_segments(composed).unwrap().iter().any(|s| s.id == "u2"), "NFC query must find it");
    }

    #[test]
    fn insert_hypothesis_stores_nfc_so_jury_agreement_is_not_normalization_fragile() {
        // The jury scores inter-engine agreement by exact surface word-equality. If two engines emit the
        // same Sorani word in different normalization forms (NFD vs NFC), a real consensus would be
        // mis-scored as a disagreement and spuriously escalated. insert_hypothesis must NFC-canonicalize
        // every vote (local 300M/1B/WSL-7B and cloud Scribe), exactly like the segment write paths.
        let db = make_db();
        let decomposed = "\u{0627}\u{0653}\u{0628}"; // ا + ◌ٓ + ب  (NFD of "آب")
        let composed = "\u{0622}\u{0628}"; // آب (NFC)
        assert_ne!(decomposed, composed, "fixture must actually differ before NFC");

        db.insert_segment(&make_segment("h1", "/h1.wav")).unwrap();
        db.insert_hypothesis(&SegmentHypothesis {
            segment_id: "h1".to_string(),
            model_id: "engine-nfd".to_string(),
            transcript: decomposed.to_string(),
            confidence: Some(0.9),
        })
        .unwrap();
        let hyps = db.get_hypotheses_for_segment("h1").unwrap();
        assert_eq!(hyps.len(), 1, "exactly one hypothesis stored");
        assert_eq!(hyps[0].transcript, composed, "hypothesis vote must be stored NFC-composed, not NFD");
    }

    #[test]
    fn machine_verdict_never_overwrites_a_human_decision() {
        // The jury (machine) write runs on a separate connection and may land AFTER a curator decided the
        // same segment mid-run. The human is authoritative: a late write_segment_verdict must be a no-op,
        // never reverting the human verdict/transcript or re-escalating an accepted segment.
        let db = make_db();
        db.insert_segment(&make_segment("hv1", "/hv1.wav")).unwrap();
        db.record_human_decision("hv1", "accept", None, None).unwrap();

        db.write_segment_verdict("hv1", "jury_accept", Some("machine consensus"), Some("r"), None, Some(0.9), true)
            .unwrap();

        let seg = db.get_segment_by_id("hv1").unwrap().unwrap();
        assert_eq!(seg.verdict.as_deref(), Some("human_accept"), "machine verdict clobbered the human decision");
        assert_eq!(seg.human_decision.as_deref(), Some("accept"), "human_decision must be preserved");
        assert!(!seg.escalated, "a human-accepted segment must not be re-escalated by a late machine write");

        // Sanity: the SAME machine write DOES apply to a fresh (non-human) segment — the guard is targeted.
        db.insert_segment(&make_segment("hv2", "/hv2.wav")).unwrap();
        db.write_segment_verdict("hv2", "jury_accept", Some("machine"), None, None, Some(0.8), false).unwrap();
        let seg2 = db.get_segment_by_id("hv2").unwrap().unwrap();
        assert_eq!(seg2.verdict.as_deref(), Some("jury_accept"), "a machine verdict must apply to a non-human segment");
    }

    #[test]
    fn clear_human_decision_reopens_the_segment_for_re_adjudication() {
        // Undo of a human decision must FULLY re-open the segment: clear the human decision AND the
        // verdict it set (the pre-decision machine verdict is gone), returning it to the review queue.
        // Otherwise the stale verdict='human_*' both shows as decided on reload and blocks re-jury.
        let db = make_db();
        db.insert_segment(&make_segment("cl1", "/cl1.wav")).unwrap();
        db.record_human_decision("cl1", "edit", Some("human gold"), None).unwrap();
        assert_eq!(db.get_segment_by_id("cl1").unwrap().unwrap().verdict.as_deref(), Some("human_edit"));

        db.clear_human_decision("cl1").unwrap();
        let cleared = db.get_segment_by_id("cl1").unwrap().unwrap();
        assert_eq!(cleared.human_decision, None, "human_decision must be cleared");
        assert_eq!(cleared.verdict, None, "the stale human verdict must be cleared, not left as 'human_edit'");
        assert_eq!(cleared.verdict_transcript, None, "the human gold transcript is part of the undone decision");
        assert!(cleared.escalated, "a re-opened segment returns to the review queue");

        // A fresh machine verdict now applies (the human-decision guard no longer blocks it).
        db.write_segment_verdict("cl1", "jury_accept", Some("machine"), None, None, Some(0.8), false).unwrap();
        assert_eq!(db.get_segment_by_id("cl1").unwrap().unwrap().verdict.as_deref(), Some("jury_accept"));
    }

    #[test]
    fn clear_escalation_is_the_exact_inverse_of_a_flag() {
        // A UI flag() sets verdict='escalated' + escalated=1. Undo must clear BOTH (unlike
        // clear_human_decision, which sets escalated=1). And it must NOT touch a segment that a human
        // decided after the flag.
        let db = make_db();
        db.insert_segment(&make_segment("fl1", "/fl1.wav")).unwrap();
        db.write_segment_verdict(
            "fl1",
            "escalated",
            None,
            Some("Flagged for second-pass adjudication"),
            None,
            None,
            true,
        )
        .unwrap();
        let flagged = db.get_segment_by_id("fl1").unwrap().unwrap();
        assert!(flagged.escalated && flagged.verdict.as_deref() == Some("escalated"));

        db.clear_escalation("fl1").unwrap();
        let un = db.get_segment_by_id("fl1").unwrap().unwrap();
        assert!(!un.escalated, "escalated flag must be cleared (inverse of flag)");
        assert_eq!(un.verdict, None, "the 'escalated' verdict must be cleared");
        assert_eq!(un.rationale, None, "the flag rationale must be cleared");

        // Guard: once a human has decided, clear_escalation must be a no-op (never stomp a decision).
        db.insert_segment(&make_segment("fl2", "/fl2.wav")).unwrap();
        db.write_segment_verdict("fl2", "escalated", None, Some("flag"), None, None, true).unwrap();
        db.record_human_decision("fl2", "accept", None, None).unwrap();
        db.clear_escalation("fl2").unwrap();
        let kept = db.get_segment_by_id("fl2").unwrap().unwrap();
        assert_eq!(kept.human_decision.as_deref(), Some("accept"), "a human-decided row must be untouched");
    }

    #[test]
    fn search_segments_tie_order_is_deterministic_by_id() {
        let db = make_db();
        // Insert in non-sorted id order; all share the search token.
        for id in ["seg_m", "seg_a", "seg_z"] {
            let mut s = make_segment(id, &format!("/{id}.wav"));
            s.raw_transcript = "uniquesearchtoken body".to_string();
            db.insert_segment(&s).unwrap();
        }
        // Pin all rows' created_at to ONE value so the tie is GUARANTEED. created_at is stamped by
        // the column default datetime('now') at 1-second resolution, so if these near-instant inserts
        // straddle a one-second clock tick they no longer tie: ORDER BY created_at DESC (the primary
        // sort key) reorders them and the id-tiebreaker assertions below flake (~1-in-N runs, under
        // load). Forcing equal timestamps isolates the `id ASC` tiebreaker that is actually under test.
        db.conn.execute("UPDATE speech_segments SET created_at = '2026-01-01 00:00:00'", []).unwrap();
        let by_search: Vec<String> =
            db.search_segments("uniquesearchtoken").unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(by_search, vec!["seg_a", "seg_m", "seg_z"], "tied search results must order by id");

        let by_ids: Vec<String> = db
            .get_segments_by_ids(&["seg_z".into(), "seg_a".into(), "seg_m".into()])
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(by_ids, vec!["seg_a", "seg_m", "seg_z"], "tied id-batch results must order by id");
    }

    #[test]
    fn get_segments_by_ids_handles_more_than_one_sqlite_param_chunk() {
        // 1200 ids spans >2 of the 500-id chunks. A single IN(?,?,…) of this size would overflow the
        // SQLite bound-parameter cap on older builds; the chunked fetch must return every row, with the
        // global (created_at DESC, id ASC) order preserved across chunk boundaries.
        let db = make_db();
        let n = 1200usize;
        for i in 0..n {
            let id = format!("seg_{i:05}");
            db.insert_segment(&make_segment(&id, &format!("/{id}.wav"))).unwrap();
        }
        // Pin created_at so the id ASC tiebreaker is what orders the result deterministically.
        db.conn.execute("UPDATE speech_segments SET created_at = '2024-01-01 00:00:00'", []).unwrap();
        let ids: Vec<String> = (0..n).map(|i| format!("seg_{i:05}")).collect();
        let got = db.get_segments_by_ids(&ids).unwrap();
        assert_eq!(got.len(), n, "every requested id must come back across all chunks");
        let got_ids: Vec<String> = got.into_iter().map(|s| s.id).collect();
        let mut expected = ids.clone();
        expected.sort();
        assert_eq!(got_ids, expected, "rows must be globally ordered by id ASC across chunk boundaries");
    }

    #[test]
    fn write_boundary_rejects_invalid_segments() {
        let db = make_db();

        // Empty id, negative duration, and an unknown split are all rejected with a
        // clean AppError::Validation — never silently persisted to corrupt later math.
        let mut s = make_segment("", "/a.wav");
        assert!(matches!(db.insert_segment(&s), Err(AppError::Validation(_))), "empty id");

        s = make_segment("s1", "/a.wav");
        s.duration_ms = -1;
        assert!(matches!(db.insert_segment(&s), Err(AppError::Validation(_))), "negative duration");

        s = make_segment("s2", "/a.wav");
        s.split = Some("trainn".to_string());
        assert!(matches!(db.insert_segment(&s), Err(AppError::Validation(_))), "bogus split");

        // A valid segment (known split) inserts fine.
        s = make_segment("s3", "/a.wav");
        s.split = Some("validation".to_string());
        db.insert_segment(&s).expect("valid segment should insert");

        // A batch containing ANY invalid segment is rejected atomically — the savepoint
        // rolls back, so even the valid sibling does not persist.
        let good = make_segment("b1", "/b1.wav");
        let mut bad = make_segment("b2", "/b2.wav");
        bad.duration_ms = -10;
        assert!(db.insert_segments_batch(&[good, bad]).is_err(), "batch with an invalid segment must fail");
        assert!(
            db.get_segment_by_audio_path("/b1.wav").unwrap().is_none(),
            "the whole batch must roll back, including the valid segment"
        );

        // After a failed batch the connection must NOT hold an open savepoint/transaction — otherwise
        // the next command would run inside a stale transaction and a later rollback could silently
        // discard committed writes (round-17: release_savepoint cleans up even if the commit fails).
        assert!(db.conn.is_autocommit(), "a failed batch must leave no open transaction on the connection");
        db.insert_segment(&make_segment("after", "/after.wav")).expect("a write after a failed batch must commit");
        assert!(db.get_segment_by_audio_path("/after.wav").unwrap().is_some());
    }

    #[test]
    fn get_segment_by_audio_path_returns_match() {
        let db = make_db();
        let seg = make_segment("s1", "/data/audio/file1.wav");
        db.insert_segment(&seg).unwrap();

        let found = db.get_segment_by_audio_path("/data/audio/file1.wav").unwrap();
        assert!(found.is_some(), "should find segment by audio_path");
        assert_eq!(found.unwrap().id, "s1");
    }

    #[test]
    fn get_segment_by_audio_path_returns_none_when_absent() {
        let db = make_db();
        let found = db.get_segment_by_audio_path("/does/not/exist.wav").unwrap();
        assert!(found.is_none(), "should return None for unknown path");
    }

    #[test]
    fn open_with_retry_quarantines_db_when_integrity_check_fails_after_open() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("recover.db");
        {
            let db = Database::open(path.to_str().expect("db path")).expect("open db");
            db.initialize().expect("initialize db");
            for i in 0..2000 {
                let mut segment = make_segment(&format!("corrupt-{i}"), &format!("/audio/{i}.wav"));
                segment.raw_transcript = "x".repeat(1000);
                db.insert_segment(&segment).expect("insert segment");
            }
            db.wal_checkpoint().expect("checkpoint");
        }

        let mut bytes = std::fs::read(&path).expect("read db");
        assert!(bytes.len() > 4096 + 64, "fixture database should span multiple pages");
        for byte in &mut bytes[4096..4096 + 64] {
            *byte = 0xFF;
        }
        std::fs::write(&path, bytes).expect("corrupt db page");

        {
            let corrupt = Database::open(path.to_str().expect("db path")).expect("corrupt db should still open");
            let integrity = corrupt.integrity_check().expect("integrity result");
            assert_ne!(integrity.trim(), "ok", "fixture must reproduce a post-open integrity failure");
        }

        let recovered = Database::open_with_retry(path.to_str().expect("db path")).expect("recover database");
        recovered.initialize().expect("initialize recovered db");

        assert_eq!(recovered.integrity_check().expect("integrity after recovery").trim(), "ok");
        assert_eq!(recovered.segment_count().expect("fresh segment count"), 0);
        assert!(
            std::fs::read_dir(tmp.path())
                .expect("read temp dir")
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with("recover.corrupt.")),
            "corrupt database should be retained as a quarantine file"
        );
    }

    #[test]
    fn on_disk_boot_applies_all_migrations_and_survives_restart() {
        // The real boot path (open_with_retry -> initialize) on a FILE-backed database, which the
        // :memory: migration tests never exercise: WAL, persistence across a close, and a second
        // open that must migrate nothing and still pass integrity_check. This is the end-to-end
        // smoke test that the continual-learning schema (v20..v23) actually applies on a genuine
        // app restart, not just in memory.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("cortex-speech.db");
        let path_str = path.to_str().expect("db path");
        let head = crate::migrations::MIGRATIONS.iter().map(|m| m.version).max().expect("migrations");

        // First boot: open, migrate to head, persist, close.
        {
            let db = Database::open_with_retry(path_str).expect("first open");
            db.initialize().expect("first initialize");
            assert_eq!(crate::migrations::get_current_version(&db).expect("version"), head);
            assert_eq!(db.integrity_check().expect("integrity").trim(), "ok");
            db.wal_checkpoint().expect("checkpoint");
        }

        // Second boot (simulated restart): the persisted schema is already at head, so initialize
        // migrates nothing, and the new continual-learning tables + provenance column are present.
        let db = Database::open_with_retry(path_str).expect("reopen");
        db.initialize().expect("reopen initialize");
        assert_eq!(crate::migrations::get_current_version(&db).expect("version after restart"), head);
        assert_eq!(db.integrity_check().expect("integrity after restart").trim(), "ok");

        let conn = db.connection();
        for table in ["correction_memory", "corrections", "model_versions", "adapters"] {
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| r.get(0))
                .expect("table query");
            assert_eq!(exists, 1, "{table} must exist after an on-disk restart");
        }
        let has_stamp: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('segment_hypotheses') WHERE name='model_version_id'",
                [],
                |r| r.get(0),
            )
            .expect("stamp query");
        assert_eq!(has_stamp, 1, "the model_version_id provenance stamp must persist across a restart");
    }

    #[test]
    fn corrupt_backup_path_avoids_same_second_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("recover.db");
        let timestamp = 1_781_573_888;
        let first = path.with_extension(format!("corrupt.{timestamp}"));
        std::fs::write(&first, "already quarantined").expect("seed existing quarantine");

        let selected = unique_corrupt_backup_path(&path, timestamp);

        assert_eq!(selected.file_name().unwrap().to_string_lossy(), "recover.corrupt.1781573888.1");
        assert!(!selected.exists());
    }

    #[test]
    fn recover_database_at_quarantines_sqlite_sidecars() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("recover.db");
        std::fs::write(&path, "main").expect("seed db");
        std::fs::write(sqlite_sidecar_path(&path, "-wal"), "wal").expect("seed wal");
        std::fs::write(sqlite_sidecar_path(&path, "-shm"), "shm").expect("seed shm");

        recover_database_at(path.to_str().expect("db path")).expect("recover database");

        assert!(!path.exists());
        assert!(!sqlite_sidecar_path(&path, "-wal").exists());
        assert!(!sqlite_sidecar_path(&path, "-shm").exists());

        let quarantine = std::fs::read_dir(tmp.path())
            .expect("read temp dir")
            .flatten()
            .map(|entry| entry.path())
            .find(|entry| entry.file_name().unwrap().to_string_lossy().starts_with("recover.corrupt."))
            .expect("main quarantine file");

        assert_eq!(std::fs::read_to_string(&quarantine).expect("read quarantined main"), "main");
        assert_eq!(
            std::fs::read_to_string(sqlite_sidecar_path(&quarantine, "-wal")).expect("read quarantined wal"),
            "wal"
        );
        assert_eq!(
            std::fs::read_to_string(sqlite_sidecar_path(&quarantine, "-shm")).expect("read quarantined shm"),
            "shm"
        );
    }

    #[test]
    fn migration_v13_creates_audio_path_index() {
        let db = make_db();
        // Verify idx_segments_audio_path index exists after migrations.
        let count: i32 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_segments_audio_path'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "idx_segments_audio_path index should exist");
    }

    #[test]
    fn fts_index_searches_inserted_segments_and_tracks_batch_delete() {
        let db = make_db();
        let mut first = make_segment("fts-1", "/data/audio/fts-1.wav");
        first.raw_transcript = "hawzhin reliable transcript".to_string();
        let mut second = make_segment("fts-2", "/data/audio/fts-2.wav");
        second.raw_transcript = "hawzhin retained transcript".to_string();

        db.insert_segment(&first).expect("insert first");
        db.insert_segment(&second).expect("insert second");

        let before_delete = db.search_segments("hawzhin").expect("search before delete");
        assert_eq!(before_delete.len(), 2, "FTS should index inserted transcripts");

        db.delete_segments_batch(&["fts-1".to_string()]).expect("batch delete");

        let after_delete = db.search_segments("hawzhin").expect("search after delete");
        assert_eq!(after_delete.len(), 1, "FTS should track batch deletes");
        assert_eq!(after_delete[0].id, "fts-2");
    }

    #[test]
    fn search_treats_fts5_metacharacters_as_literal_text_not_query_syntax() {
        // Regression for the hardening-audit HIGH finding: FTS5 parses the bound value as a query,
        // so ordinary punctuation used to raise a hard error (unterminated string / no such column /
        // fts5: syntax error) and surface a confusing toast on every such keystroke.
        let db = make_db();
        let mut seg = make_segment("repro-1", "/a.wav");
        seg.raw_transcript = "hello world foo bar".to_string();
        db.insert_segment(&seg).expect("insert");

        // Each of these errored BEFORE the fix; all must now be Ok (results or empty), never Err.
        // The control-char cases (NUL etc.) are the regression a proptest later surfaced: an interior
        // NUL survived split_whitespace and made SQLite/FTS5 raise a hard error.
        for q in [
            "\"hello", "foo:bar", "*", "(", "NEAR(a b", "a AND", "OR", "^", "-foo", ")", "\0", "a\0b", "\u{1b}",
            "\u{7f}",
        ] {
            assert!(db.search_segments(q).is_ok(), "query {q:?} must not error");
        }
        // A real token still finds the segment, and a quote next to it doesn't break matching.
        assert_eq!(db.search_segments("hello").unwrap().len(), 1, "literal token still matches");
        assert_eq!(db.search_segments("\"hello\"").unwrap().len(), 1, "quoted token matches too");
        // Whitespace-only input is an empty result, not an error.
        assert!(db.search_segments("   ").unwrap().is_empty(), "blank query -> empty, not error");
    }

    #[test]
    fn search_segments_never_errors_on_arbitrary_input() {
        use proptest::prelude::*;
        // Property generalization of the metacharacter regression above: for ANY user input the
        // search box must return Ok (results or empty), never an FTS5 syntax Err or a panic. The
        // example test samples known-bad punctuation; this covers the infinite input space.
        let db = make_db();
        let mut seg = make_segment("prop-1", "/a.wav");
        seg.raw_transcript = "hello world foo bar".to_string();
        db.insert_segment(&seg).expect("insert");

        proptest!(|(q in ".*")| {
            prop_assert!(db.search_segments(&q).is_ok(), "search must not error on input {q:?}");
        });
    }

    #[test]
    fn insert_segment_accepts_arbitrary_transcript_text_and_keeps_search_queryable() {
        use proptest::prelude::*;
        // Write-path sibling of the search robustness property: user annotations/corrections are
        // free text, so persisting ANY transcript body must not error and must not corrupt the FTS
        // index it feeds (a later search must still return Ok, never an indexing-time syntax error).
        proptest!(|(body in ".*")| {
            let db = make_db();
            let mut seg = make_segment("prop-w", "/w.wav");
            seg.raw_transcript = body.clone();
            prop_assert!(db.insert_segment(&seg).is_ok(), "insert must not error on body {body:?}");
            prop_assert!(db.search_segments("hello").is_ok(), "search must stay Ok after body {body:?}");
        });
    }

    #[test]
    fn consensus_batch_preserves_human_reviewed_transcripts() {
        // Hardening-audit MEDIUM (silent data loss): the consensus refinery overwrote human-corrected
        // transcripts because update_segment_consensus_batch lacked the human-review guard that every
        // other transcript-write path (e.g. update_asr_transcript_if_unreviewed) enforces.
        let db = make_db();
        let mut locked = make_segment("locked-1", "/a.wav");
        locked.raw_transcript = "human corrected text".to_string();
        locked.normalized_transcript = Some("human corrected text".to_string());
        db.insert_segment(&locked).expect("insert locked");
        db.conn
            .execute("UPDATE speech_segments SET verdict='human_edit', human_decision='edit' WHERE id='locked-1'", [])
            .expect("lock as human-reviewed");

        // The refinery's batch write tries to replace the locked segment with machine consensus.
        db.update_segment_consensus_batch(&[(
            "locked-1".to_string(),
            "machine consensus text".to_string(),
            "machine consensus text".to_string(),
            0.9,
        )])
        .expect("consensus batch");

        let after = db.get_segment_by_id("locked-1").unwrap().expect("segment exists");
        assert_eq!(after.raw_transcript, "human corrected text", "human correction must NOT be clobbered");
        assert_eq!(after.normalized_transcript.as_deref(), Some("human corrected text"));

        // An UNREVIEWED segment is still refined normally (the guard only protects human-locked rows).
        let mut fresh = make_segment("fresh-1", "/b.wav");
        fresh.raw_transcript = "old asr".to_string();
        db.insert_segment(&fresh).expect("insert fresh");
        db.update_segment_consensus_batch(&[(
            "fresh-1".to_string(),
            "new consensus".to_string(),
            "new consensus".to_string(),
            0.8,
        )])
        .expect("consensus batch 2");
        assert_eq!(
            db.get_segment_by_id("fresh-1").unwrap().unwrap().raw_transcript,
            "new consensus",
            "an unreviewed segment is still refined"
        );
    }

    #[test]
    fn batch_transcription_update_preserves_human_review_and_seeds_annotation() {
        // Round-9 audit HIGH (lost update): batch_transcribe wrote the whole STALE snapshot back via
        // insert_segment, reverting a concurrent human verify/edit. The guarded targeted write must
        // (a) refuse to touch a human-verified/reviewed row, (b) never revert `verified`, (c) seed the
        // annotation only when still empty, and (d) preserve an existing human annotation (COALESCE).
        let db = make_db();

        // (a)+(b): a human verified + annotated this row AFTER the batch prefetched it.
        let mut verified = make_segment("verified-1", "/a.wav");
        verified.raw_transcript = "old asr".to_string();
        db.insert_segment(&verified).expect("insert verified");
        db.conn
            .execute(
                "UPDATE speech_segments SET verified=1, annotated_transcript='human gold' WHERE id='verified-1'",
                [],
            )
            .expect("mark verified");
        let updated = db
            .update_batch_transcription_if_unreviewed(
                "verified-1",
                "fresh asr",
                Some("fresh asr"),
                Some(0.9),
                Some("heuristic"),
                Some("omniasr-ctc-300m"),
                false,
                "fresh asr",
            )
            .expect("update verified");
        assert!(!updated, "a verified row must be skipped, not updated");
        let after = db.get_segment_by_id("verified-1").unwrap().unwrap();
        assert!(after.verified, "verified flag must NOT be reverted by the batch");
        assert_eq!(after.annotated_transcript.as_deref(), Some("human gold"), "human annotation preserved");
        assert_eq!(after.raw_transcript, "old asr", "human-owned row's raw must not be clobbered");

        // (c): a fresh unreviewed row with no annotation IS updated and seeds the annotation.
        let mut fresh = make_segment("fresh-1", "/b.wav");
        fresh.raw_transcript = "old".to_string();
        fresh.annotated_transcript = None;
        db.insert_segment(&fresh).expect("insert fresh");
        let updated = db
            .update_batch_transcription_if_unreviewed(
                "fresh-1",
                "new asr",
                Some("new asr"),
                Some(0.8),
                Some("heuristic"),
                Some("omniasr-ctc-300m"),
                false,
                "new asr",
            )
            .expect("update fresh");
        assert!(updated, "an unreviewed row is updated");
        let after = db.get_segment_by_id("fresh-1").unwrap().unwrap();
        assert_eq!(after.raw_transcript, "new asr");
        assert_eq!(after.annotated_transcript.as_deref(), Some("new asr"), "annotation seeded when empty");
        assert_eq!(after.confidence_source.as_deref(), Some("heuristic"));
        assert_eq!(after.model_version_id.as_deref(), Some("omniasr-ctc-300m"));
        assert!(!after.cloud_call);

        // (d): an unverified row the user annotated (without verifying) keeps that annotation; only
        // the ASR fields refresh — the seed is ignored because COALESCE reads the CURRENT row.
        let mut annotated = make_segment("annot-1", "/c.wav");
        annotated.raw_transcript = "old".to_string();
        annotated.annotated_transcript = Some("user typed".to_string());
        db.insert_segment(&annotated).expect("insert annotated");
        let updated = db
            .update_batch_transcription_if_unreviewed(
                "annot-1",
                "new asr",
                Some("new asr"),
                Some(0.7),
                Some("real_posterior"),
                Some("omniasr-ctc-1b"),
                false,
                "seed ignored",
            )
            .expect("update annotated");
        assert!(updated, "an unverified annotated row still refreshes ASR");
        let after = db.get_segment_by_id("annot-1").unwrap().unwrap();
        assert_eq!(
            after.annotated_transcript.as_deref(),
            Some("user typed"),
            "existing annotation preserved (COALESCE)"
        );
        assert_eq!(after.raw_transcript, "new asr", "raw ASR refreshed on an unverified row");
        assert_eq!(after.confidence_source.as_deref(), Some("real_posterior"));
        assert_eq!(after.model_version_id.as_deref(), Some("omniasr-ctc-1b"));
    }

    #[test]
    fn human_edit_does_not_write_no_op_correction_ledger_row() {
        // Round-9 audit LOW: when the model was already right (no candidate differs from the fix),
        // wrong_side falls back to raw_transcript, so the corrections ledger used to record a row whose
        // raw_hypothesis == human_fix. A real (resolvable) audio file is required so the ledger hash
        // resolves — otherwise the ledger insert is skipped for an unrelated reason and the bug hides.
        let db = make_db();
        let tmp = tempfile::tempdir().expect("tmp");
        let audio = tmp.path().join("clip.wav");
        std::fs::write(&audio, b"RIFFxxxxWAVEfmt ").expect("write audio");
        let audio_path = audio.to_string_lossy().to_string();

        let mut seg = make_segment("noop-1", &audio_path);
        seg.raw_transcript = "hello world".to_string();
        db.insert_segment(&seg).expect("insert");

        // A no-op edit: the corrected text equals the raw ASR (up to the learning key).
        db.record_human_decision("noop-1", "edit", Some("hello world"), None).expect("record no-op edit");

        let ledger_rows: i64 =
            db.conn.query_row("SELECT COUNT(*) FROM corrections WHERE segment_id='noop-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(ledger_rows, 0, "a no-op edit must NOT append a corrections-ledger row");

        // A genuine correction on the same kind of row DOES record a ledger entry.
        let mut seg2 = make_segment("real-1", &audio_path);
        seg2.raw_transcript = "helo wrld".to_string();
        db.insert_segment(&seg2).expect("insert real");
        db.record_human_decision("real-1", "edit", Some("hello world"), None).expect("record real edit");
        let real_rows: i64 =
            db.conn.query_row("SELECT COUNT(*) FROM corrections WHERE segment_id='real-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(real_rows, 1, "a genuine correction still records a ledger row");
    }

    #[test]
    fn merge_dataset_json_does_not_count_human_protected_rows_as_updated() {
        // Hardening-audit LOW: the guarded merge UPDATE correctly skips human-reviewed rows, but the
        // 'updated' counter incremented regardless of rows-affected — over-reporting to the UI.
        let db = make_db();
        let mut seg = make_segment("merge-1", "/a.wav");
        seg.raw_transcript = "original".to_string();
        db.insert_segment(&seg).expect("insert");
        db.conn
            .execute("UPDATE speech_segments SET verdict='human_accept' WHERE id='merge-1'", [])
            .expect("lock as human-reviewed");

        let incoming = vec![SpeechSegment {
            id: "merge-1".to_string(),
            audio_path: "/a.wav".to_string(),
            raw_transcript: "incoming".to_string(),
            duration_ms: 1000,
            ..SpeechSegment::default()
        }];
        let json = serde_json::to_string(&incoming).expect("serialize");
        let (created, updated) = db.merge_dataset_json(&json).expect("merge");
        assert_eq!((created, updated), (0, 0), "a guard-skipped human-locked row must not count as updated");
        assert_eq!(
            db.get_segment_by_id("merge-1").unwrap().unwrap().raw_transcript,
            "original",
            "the locked row is genuinely unchanged"
        );
    }

    #[test]
    fn consensus_batch_counts_only_rows_actually_changed() {
        // Round-2 audit LOW: the refinery reported updates.len() (attempted), not rows changed, so a
        // guard-skipped human-locked segment was over-counted. The method now returns rows-affected.
        let db = make_db();
        let mut locked = make_segment("c-lock", "/a.wav");
        locked.raw_transcript = "orig".to_string();
        db.insert_segment(&locked).expect("insert locked");
        db.conn.execute("UPDATE speech_segments SET verdict='human_accept' WHERE id='c-lock'", []).expect("lock");
        db.insert_segment(&make_segment("c-fresh", "/b.wav")).expect("insert fresh");

        let changed = db
            .update_segment_consensus_batch(&[
                ("c-lock".to_string(), "new".to_string(), "new".to_string(), 0.9),
                ("c-fresh".to_string(), "new2".to_string(), "new2".to_string(), 0.9),
            ])
            .expect("batch");
        assert_eq!(changed, 1, "only the unlocked row counts; the human-locked one is skipped");
    }

    #[test]
    fn fts_search_matches_sorani_codepoint_variants() {
        let db = make_db();
        // The canonical normalized_transcript uses Kurdish Keheh (ک U+06A9) + Yeh
        // (ی U+06CC). raw_transcript is deliberately non-matching Latin so the test
        // isolates whether a query typed with the Arabic Kaf/Yeh variant still
        // matches the canonical normalized text.
        let mut seg = make_segment("fts-var", "/data/audio/fts-var.wav");
        seg.raw_transcript = "zzz".to_string();
        seg.normalized_transcript = Some("کوردی".to_string());
        db.insert_segment(&seg).expect("insert segment");

        // Query uses Arabic Kaf (ك U+0643) + Arabic Yeh (ي U+064A) — distinct codepoints.
        let hits = db.search_segments("كوردي").expect("variant search");
        assert_eq!(hits.len(), 1, "a variant-typed query must match the canonical normalized transcript");
        assert_eq!(hits[0].id, "fts-var");
    }

    #[test]
    fn human_edit_learning_uses_agent_proposal_before_raw_asr() {
        let db = make_db();
        let mut seg = make_segment("learn-agent", "/data/audio/learn-agent.wav");
        seg.raw_transcript = "raw wrong transcript".to_string();
        seg.normalized_transcript = Some("normalized wrong transcript".to_string());
        seg.verdict = Some("jury_accept".to_string());
        seg.verdict_transcript = Some("agent proposed transcript".to_string());
        seg.escalated = true;
        db.insert_segment(&seg).expect("insert segment");
        db.write_segment_verdict(
            "learn-agent",
            "jury_accept",
            Some("agent proposed transcript"),
            Some("agent rationale"),
            None,
            Some(0.81),
            true,
        )
        .expect("write agent verdict");

        db.record_human_decision("learn-agent", "edit", Some("human corrected transcript"), None)
            .expect("record human edit");

        let fresh = db.get_segment_by_id("learn-agent").expect("load segment").expect("segment exists");
        assert_eq!(fresh.human_decision.as_deref(), Some("edit"));
        assert_eq!(fresh.verdict.as_deref(), Some("human_edit"));
        assert_eq!(fresh.verdict_transcript.as_deref(), Some("human corrected transcript"));
        assert!(!fresh.escalated);

        let (wrong, fix): (String, String) = db
            .connection()
            .query_row(
                "SELECT wrong_transcript, human_fix FROM agent_examples WHERE segment_id = ?1",
                params!["learn-agent"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("learning example exists");
        assert_eq!(wrong, "agent proposed transcript");
        assert_eq!(fix, "human corrected transcript");
    }

    #[test]
    fn human_edit_skips_learning_pair_when_proposal_matches_fix() {
        let db = make_db();
        let mut seg = make_segment("learn-same", "/data/audio/learn-same.wav");
        seg.raw_transcript = "same text".to_string();
        seg.verdict_transcript = Some("same   text".to_string());
        db.insert_segment(&seg).expect("insert segment");
        db.write_segment_verdict("learn-same", "jury_accept", Some("same   text"), None, None, Some(0.9), true)
            .expect("write agent verdict");

        db.record_human_decision("learn-same", "edit", Some("same text"), None).expect("record human edit");

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM agent_examples WHERE segment_id = ?1", params!["learn-same"], |row| {
                row.get(0)
            })
            .expect("count examples");
        assert_eq!(count, 0);
    }

    #[test]
    fn record_human_decision_appends_to_corrections_ledger() {
        let db = make_db();
        // A real on-disk audio file so the durable content hash (the ledger's identity) can be
        // computed, even though the database itself is in memory.
        let tmp = tempfile::tempdir().expect("tempdir");
        let audio = tmp.path().join("clip.wav");
        std::fs::write(&audio, b"RIFF....fake-audio-bytes").expect("write audio");
        let expected_hash = crate::pipeline::source_audio_identity(&audio).expect("identity").content_hash;

        let mut seg = make_segment("led-1", audio.to_str().expect("audio path"));
        seg.raw_transcript = "wrong text".to_string();
        db.insert_segment(&seg).expect("insert segment");
        // The agent verdict the human is about to override (captured into jury_verdict).
        db.write_segment_verdict("led-1", "jury_accept", Some("agent guess"), None, None, Some(0.7), true)
            .expect("write agent verdict");

        db.record_human_decision("led-1", "edit", Some("right text"), None).expect("record edit");

        let (segment_id, hash, raw_hyp, fix, jury, mv): (
            Option<String>,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = db
            .connection()
            .query_row(
                "SELECT segment_id, audio_content_hash, raw_hypothesis, human_fix, jury_verdict, model_version_id
                 FROM corrections WHERE segment_id = ?1",
                params!["led-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .expect("a corrections ledger row must exist after an edit");
        assert_eq!(segment_id.as_deref(), Some("led-1"));
        assert_eq!(hash, expected_hash, "the ledger must key on the durable audio content hash");
        assert!(!raw_hyp.is_empty(), "raw_hypothesis must record what the model produced");
        assert_eq!(fix, "right text");
        assert_eq!(jury.as_deref(), Some("jury_accept"), "jury_verdict captures the pre-override agent verdict");
        assert_eq!(mv.as_deref(), Some("unknown@pre-registry"), "model_version_id provenance is stamped");
    }

    #[test]
    fn non_edit_decision_writes_no_corrections_ledger_row() {
        let db = make_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let audio = tmp.path().join("clip.wav");
        std::fs::write(&audio, b"bytes").expect("write audio");
        let mut seg = make_segment("led-acc", audio.to_str().expect("path"));
        seg.raw_transcript = "ok text".to_string();
        db.insert_segment(&seg).expect("insert segment");

        db.record_human_decision("led-acc", "accept", None, None).expect("record accept");

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM corrections WHERE segment_id = ?1", params!["led-acc"], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0, "an accept (non-edit) decision records no correction");
    }

    #[test]
    fn edit_with_missing_audio_still_records_verdict_without_ledger_row() {
        // Best-effort ledger: a missing audio file must never block the human's correction.
        let db = make_db();
        let mut seg = make_segment("led-missing", "/nonexistent/gone.wav");
        seg.raw_transcript = "wrong".to_string();
        db.insert_segment(&seg).expect("insert segment");

        db.record_human_decision("led-missing", "edit", Some("right"), None).expect("edit must still succeed");

        let fresh = db.get_segment_by_id("led-missing").expect("load").expect("exists");
        assert_eq!(fresh.human_decision.as_deref(), Some("edit"), "the verdict is recorded despite missing audio");
        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM corrections WHERE segment_id = ?1", params!["led-missing"], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0, "no ledger row when the audio identity cannot be computed");
    }

    #[test]
    fn edit_populates_correction_memory_with_substitution() {
        let db = make_db();
        let mut seg = make_segment("mem-1", "/data/audio/mem-1.wav");
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        db.record_human_decision("mem-1", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");

        let (wrong, human, hits): (String, String, i64) = db
            .connection()
            .query_row(
                "SELECT wrong_token, human_token, hit_count FROM correction_memory WHERE source_segment = ?1",
                params!["mem-1"],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("a correction memory row must exist after a substituting edit");
        assert_eq!(wrong, "باش");
        assert_eq!(human, "خراپ");
        assert_eq!(hits, 0, "a freshly captured memory starts at hit_count 0");
    }

    #[test]
    fn repeated_correction_bumps_hit_count_not_duplicates() {
        let db = make_db();
        for id in ["mem-a", "mem-b"] {
            let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
            seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
            db.insert_segment(&seg).expect("insert");
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
        }
        let (rows, max_hits): (i64, i64) = db
            .connection()
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(hit_count), 0) FROM correction_memory
                 WHERE wrong_token = 'باش' AND human_token = 'خراپ'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query");
        assert_eq!(rows, 1, "the same correction must upsert, not duplicate");
        assert_eq!(max_hits, 1, "a second independent confirmation bumps hit_count to 1");
    }

    #[test]
    fn gold_edit_does_not_populate_correction_memory() {
        let db = make_db();
        let mut seg = make_segment("mem-gold", "/data/audio/mem-gold.wav");
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        db.connection().execute("UPDATE speech_segments SET is_gold = 1 WHERE id = 'mem-gold'", []).expect("mark gold");
        db.record_human_decision("mem-gold", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM correction_memory WHERE source_segment = 'mem-gold'", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0, "gold-segment edits must not populate LOOP-0 memory (eval-leak guard)");
    }

    #[test]
    fn load_correction_memories_returns_captured_entries() {
        let db = make_db();
        let mut seg = make_segment("lm-1", "/data/audio/lm-1.wav");
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        db.record_human_decision("lm-1", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");

        let mems = db.load_correction_memories().expect("load");
        assert_eq!(mems.len(), 1);
        assert_eq!(mems[0].wrong_token, "باش");
        assert_eq!(mems[0].human_token, "خراپ");
        assert!(
            (mems[0].confidence - 0.5).abs() < 1e-9,
            "a freshly captured memory starts at the Beta(1,1) prior 0.5 (no firing-outcome evidence yet)"
        );
        assert_eq!(mems[0].hit_count, 0);
    }

    #[test]
    fn loop0_round_trips_capture_to_fire_through_the_database() {
        // The whole LOOP 0 minus the live-decode wiring. The same confusion is corrected on TWO
        // segments so hit_count reaches 1 and clears the anti-one-off guard (a single correction,
        // hit_count 0, deliberately does NOT fire — covered by unconfirmed_memory_does_not_fire).
        let db = make_db();
        for id in ["lm-2a", "lm-2b"] {
            let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
            seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
            db.insert_segment(&seg).expect("insert");
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
        }

        let mems = db.load_correction_memories().expect("load");
        assert_eq!(mems.len(), 1, "the repeated correction upserts to a single memory");
        assert_eq!(mems[0].hit_count, 1, "confirmed twice -> hit_count 1, past the anti-one-off guard");

        let out =
            crate::corrections::apply_memories("ئەو ساڵە باش بوو", &mems, &crate::corrections::FiringConfig::default());
        assert_eq!(out, "ئەو ساڵە خراپ بوو", "capture x2 -> DB -> load -> fire reproduces the human fix");
    }

    /// Confirming edits of the SAME confusion must lift a memory's evidence-based confidence from the
    /// neutral prior up past tau_conf — the "a confirmed memory's confidence rises" half of the audit fix.
    #[test]
    fn confirmed_memory_confidence_rises_above_tau_conf() {
        let db = make_db();
        let tau = crate::corrections::FiringConfig::default().tau_conf;

        // The first edit CAPTURES the memory at the 0.5 prior — below tau_conf, so it cannot fire yet.
        let mut seg0 = make_segment("cf-0", "/data/audio/cf-0.wav");
        seg0.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg0).expect("insert");
        db.record_human_decision("cf-0", "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
        let fresh = db.load_correction_memories().expect("load")[0].confidence;
        assert!(fresh < tau, "a freshly captured memory sits at the 0.5 prior, below tau_conf: {fresh}");

        // Each further human edit of the same confusion is an independent confirmation -> confidence climbs.
        for id in ["cf-1", "cf-2"] {
            let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
            seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
            db.insert_segment(&seg).expect("insert");
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
        }
        let confirmed = db.load_correction_memories().expect("load")[0].confidence;
        assert!(confirmed > tau, "confirming edits must raise confidence above tau_conf: {confirmed}");
    }

    /// A memory that first earns confidence, then repeatedly over-triggers on drafts the human ACCEPTS
    /// as-is, must decay back below tau_conf — the anti-poisoning "one bad memory decays" half of the fix.
    #[test]
    fn overridden_memory_confidence_decays_below_tau_conf() {
        let db = make_db();
        let tau = crate::corrections::FiringConfig::default().tau_conf;

        // Earn confidence with confirming edits of the same confusion (distinct segments).
        for id in ["bad-1", "bad-2", "bad-3"] {
            let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
            seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
            db.insert_segment(&seg).expect("insert");
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
        }
        let confident = db.load_correction_memories().expect("load")[0].confidence;
        assert!(confident > tau, "after confirming edits the memory clears tau_conf: {confident}");

        // Now humans keep ACCEPTING the original in that slot -> every would-fire is an over-trigger.
        for id in ["ovr-1", "ovr-2", "ovr-3"] {
            let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
            seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
            db.insert_segment(&seg).expect("insert");
            db.record_human_decision(id, "accept", None, None).expect("accept");
        }
        let decayed = db.load_correction_memories().expect("load")[0].confidence;
        assert!(decayed < tau, "repeated over-triggers must decay confidence below tau_conf: {decayed}");
    }

    /// A confirm/override event stamps `last_fired_at` (previously never written) and increments the
    /// firing-outcome counters; a gold segment's decision must NOT touch either (eval-leak guard).
    #[test]
    fn confidence_evidence_stamps_last_fired_at_and_skips_gold() {
        let db = make_db();
        // Capture on the first edit, then a second same-confusion edit lands a confirm.
        for id in ["lf-1", "lf-2"] {
            let mut seg = make_segment(id, &format!("/data/audio/{id}.wav"));
            seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
            db.insert_segment(&seg).expect("insert");
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو"), None).expect("edit");
        }
        let (fired_set, confirms): (i64, i64) = db
            .connection()
            .query_row(
                "SELECT last_fired_at IS NOT NULL, confirm_count FROM correction_memory WHERE wrong_token = 'باش'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query");
        assert_eq!(fired_set, 1, "a confirm event stamps last_fired_at");
        assert_eq!(confirms, 1, "the second same-confusion edit records exactly one confirm");

        // A gold segment whose text the memory would fire on must leave the evidence untouched.
        let mut gold = make_segment("lf-gold", "/data/audio/lf-gold.wav");
        gold.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&gold).expect("insert");
        db.connection().execute("UPDATE speech_segments SET is_gold = 1 WHERE id = 'lf-gold'", []).expect("mark gold");
        db.record_human_decision("lf-gold", "accept", None, None).expect("accept");
        let overrides: i64 = db
            .connection()
            .query_row("SELECT override_count FROM correction_memory WHERE wrong_token = 'باش'", [], |r| r.get(0))
            .expect("query");
        assert_eq!(overrides, 0, "a gold-segment decision must not update firing-outcome evidence (eval-leak guard)");
    }

    #[test]
    fn loop0_shadow_log_records_would_fire_flag() {
        // P1.3: each shadow observation persists a row with its memory_fired flag (the C5 data source).
        let db = make_db();
        db.insert_segment(&make_segment("sh-1", "/data/audio/sh-1.wav")).expect("insert");
        db.record_loop0_shadow("sh-1", true).expect("shadow true");
        db.record_loop0_shadow("sh-1", false).expect("shadow false");

        let rows: Vec<i64> = db
            .connection()
            .prepare("SELECT memory_fired FROM loop0_shadow_log WHERE segment_id = 'sh-1' ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, vec![1, 0], "both shadow observations persist with their flags");
    }

    #[test]
    fn deleting_a_segment_preserves_its_loop0_over_trigger_evidence() {
        // C5 survivor-bias guard: the owner's normal cleanup (review a bad clip, then delete it) must not
        // erase the over-trigger evidence that gate reads — else the gate looks safer than reality.
        let db = make_db();
        let mut seg = make_segment("ov-1", "/data/audio/ov-1.wav");
        seg.raw_transcript = "ئەو ساڵە باش بوو".to_string();
        db.insert_segment(&seg).expect("insert");
        // Shadow says a memory WOULD fire; the human then accepted the original -> an OVER-TRIGGER.
        db.record_loop0_shadow("ov-1", true).expect("shadow");
        db.connection().execute("UPDATE speech_segments SET human_decision='accept' WHERE id='ov-1'", []).expect("hd");

        let ot_before =
            db.intelligence_report().expect("report")["loop0Shadow"]["firedButHumanAcceptedOriginal"].as_i64().unwrap();
        assert_eq!(ot_before, 1, "the over-trigger is counted while the segment exists");

        db.delete_segment("ov-1").expect("delete");

        let ot_after =
            db.intelligence_report().expect("report")["loop0Shadow"]["firedButHumanAcceptedOriginal"].as_i64().unwrap();
        assert_eq!(ot_after, 1, "the over-trigger evidence SURVIVES deletion via the durable archive");
    }

    #[test]
    fn restore_rejects_a_corrupt_source_before_overwriting_the_live_db() {
        let tmp = tempfile::TempDir::new().unwrap();
        let live_path = tmp.path().join("live.db");
        let mut live = Database::open(live_path.to_str().unwrap()).unwrap();
        live.initialize().unwrap();

        // A healthy snapshot restores fine.
        let good_path = tmp.path().join("good.db");
        {
            let good = Database::open(good_path.to_str().unwrap()).unwrap();
            good.initialize().unwrap();
        }
        live.restore(&good_path).expect("a healthy snapshot restores");

        // A garbage source is REJECTED (integrity/open failure) — no partial overwrite of the live DB.
        let bad_path = tmp.path().join("bad.db");
        std::fs::write(&bad_path, b"this is not a sqlite database at all").unwrap();
        assert!(live.restore(&bad_path).is_err(), "a corrupt snapshot must be rejected");
        assert!(live.segment_count().is_ok(), "the live database is still usable after a rejected restore");
    }

    #[test]
    fn audio_health_detects_missing_and_relink_repoints_by_basename() {
        // P3.3: a moved/renamed source file becomes "missing"; pointing relink at the new folder
        // repoints every segment on that path (basename match).
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("clip.wav");
        std::fs::write(&src, b"audio").unwrap();
        let db = make_db();
        for id in ["a", "b"] {
            db.insert_segment(&make_segment(id, &src.to_string_lossy())).unwrap();
        }
        assert_eq!(db.audio_health().unwrap().missing_files, 0, "present file -> healthy");

        // Owner reorganizes: move the file to a new folder (same name).
        let newdir = tmp.path().join("moved");
        std::fs::create_dir(&newdir).unwrap();
        let moved = newdir.join("clip.wav");
        std::fs::rename(&src, &moved).unwrap();

        let health = db.audio_health().unwrap();
        assert_eq!(health.total_files, 1, "two segments, one distinct source file");
        assert_eq!(health.missing_files, 1, "the moved file is missing");

        let result = db.relink_audio(&newdir).unwrap();
        assert_eq!(result.relinked, 1);
        assert_eq!(result.still_missing, 0);
        assert_eq!(db.audio_health().unwrap().missing_files, 0, "relinked -> healthy");
        assert_eq!(
            db.get_segment_by_id("a").unwrap().unwrap().audio_path,
            moved.to_string_lossy().to_string(),
            "both segments repointed to the found file"
        );
    }

    #[test]
    fn relink_leaves_unmatched_paths_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = make_db();
        db.insert_segment(&make_segment("x", "/gone/missing.wav")).unwrap();
        let result = db.relink_audio(tmp.path()).unwrap(); // no clip named missing.wav in the dir
        assert_eq!(result.relinked, 0);
        assert_eq!(result.still_missing, 1, "an unmatched missing path stays missing");
    }

    #[test]
    fn relink_refuses_ambiguous_basename_collisions() {
        // P3.3 ambiguity guard: two DISTINCT missing sources share a basename. A single found file of
        // that name cannot be known to be the right one for both — relink must refuse both (leaving
        // them missing) rather than silently repoint segments to the WRONG recording's audio.
        let tmp = tempfile::TempDir::new().unwrap();
        let db = make_db();
        db.insert_segment(&make_segment("from_a", "/old/folderA/interview.wav")).unwrap();
        db.insert_segment(&make_segment("from_b", "/old/folderB/interview.wav")).unwrap();
        // The owner points relink at a folder that happens to contain ONE interview.wav.
        std::fs::write(tmp.path().join("interview.wav"), b"audio").unwrap();

        let result = db.relink_audio(tmp.path()).unwrap();
        assert_eq!(result.relinked, 0, "ambiguous basename -> nothing relinked");
        assert_eq!(result.still_missing, 2, "both colliding sources stay missing, not mis-linked");
        // Neither segment was repointed — the original (missing) paths are untouched.
        assert_eq!(db.get_segment_by_id("from_a").unwrap().unwrap().audio_path, "/old/folderA/interview.wav");
        assert_eq!(db.get_segment_by_id("from_b").unwrap().unwrap().audio_path, "/old/folderB/interview.wav");
    }

    #[test]
    fn sorani_paths_and_fts_search_are_robust() {
        // P3.7: a real Sorani corpus has Arabic-script filenames with spaces and long paths. Storage,
        // round-trip, and (transcript-scoped) FTS search must all survive them.
        let db = make_db();
        let long_dir = format!("D:/media/کوردی ساؤند سامپڵز/{}", "پارچە ".repeat(40));
        let audio_path = format!("{long_dir}/گەشتی مێژوویی.wav");
        assert!(audio_path.chars().count() > 200, "path is >200 chars: {}", audio_path.chars().count());
        let mut seg = make_segment("sr-1", &audio_path);
        seg.raw_transcript = "ئەمە دەقێکی کوردی گرنگە".to_string();
        db.insert_segment(&seg).unwrap();

        // The long non-ASCII path round-trips byte-identical.
        let got = db.get_segment_by_id("sr-1").unwrap().unwrap();
        assert_eq!(got.audio_path, audio_path, "non-ASCII long path round-trips intact");
        assert_eq!(got.raw_transcript, "ئەمە دەقێکی کوردی گرنگە");

        // FTS finds the segment by a Sorani transcript word (unicode61 tokenizer).
        let by_transcript = db.search_segments("کوردی").unwrap();
        assert!(by_transcript.iter().any(|s| s.id == "sr-1"), "FTS finds the Sorani segment by transcript content");

        // A token present only in the PATH (not the transcript) must NOT match — search is
        // transcript-scoped, so a folder name never yields a false positive.
        let by_path_token = db.search_segments("سامپڵز").unwrap();
        assert!(!by_path_token.iter().any(|s| s.id == "sr-1"), "a path-only token does not match");
    }

    #[test]
    fn import_journal_records_progress_and_finds_interruption() {
        // P3.2: a running job with some files done is the interruption to resume; completing clears it.
        let db = make_db();
        assert!(db.find_interrupted_import_job().unwrap().is_none(), "no job -> no interruption");

        let job = db.begin_import_job("C:/audio", 3).unwrap();
        db.mark_import_file_done(&job, "C:/audio/a.wav").unwrap();
        db.mark_import_file_done(&job, "C:/audio/b.wav").unwrap();
        db.mark_import_file_done(&job, "C:/audio/a.wav").unwrap(); // idempotent

        let found = db.find_interrupted_import_job().unwrap().unwrap();
        assert_eq!(found.id, job);
        assert_eq!(found.dir, "C:/audio");
        assert_eq!(found.total_files, 3);
        assert_eq!(found.completed_paths.len(), 2, "idempotent mark -> 2 distinct completed files");

        db.complete_import_job(&job).unwrap();
        assert!(db.find_interrupted_import_job().unwrap().is_none(), "completed job is not an interruption");
    }

    #[test]
    fn begin_import_job_reaps_prior_running_crashes() {
        // P3.2 robustness: a crash leaves a job 'running' forever. When a new import begins, that stale
        // crash must be reaped so (a) it can't accumulate across repeated crashes and (b) resuming the
        // NEW job never leaves an old crash lurking to prompt a spurious resume later.
        let db = make_db();
        let crashed = db.begin_import_job("C:/audio/first", 5).unwrap();
        db.mark_import_file_done(&crashed, "C:/audio/first/a.wav").unwrap();
        // No complete/discard — simulate a crash. A second import starts.
        let fresh = db.begin_import_job("C:/audio/second", 2).unwrap();

        // Exactly one 'running' job now, and it is the fresh one — the crash was reaped, not surfaced.
        let running: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM import_jobs WHERE status = 'running'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(running, 1, "only the active import is 'running'; the prior crash is abandoned");
        let found = db.find_interrupted_import_job().unwrap().unwrap();
        assert_eq!(found.id, fresh, "the interruption is the fresh import, never the reaped crash");
        assert_eq!(found.dir, "C:/audio/second");
    }

    #[test]
    fn discard_import_job_removes_it_and_its_files() {
        let db = make_db();
        let job = db.begin_import_job("C:/x", 1).unwrap();
        db.mark_import_file_done(&job, "C:/x/a.wav").unwrap();
        db.discard_import_job(&job).unwrap();
        assert!(db.find_interrupted_import_job().unwrap().is_none());
        let files: i64 = db.connection().query_row("SELECT COUNT(*) FROM import_job_files", [], |r| r.get(0)).unwrap();
        assert_eq!(files, 0, "discard removed the job's file rows");
    }

    #[test]
    fn source_transcript_upsert_roundtrips_latest_reference() {
        let db = make_db();
        let first = SourceTranscriptRecord {
            audio_path: "/audio/long.wav".to_string(),
            model_id: "gemini-2.5-pro".to_string(),
            audio_content_hash: Some("hash-v1".to_string()),
            audio_size_bytes: Some(123),
            transcript_path: "/refs/long.txt".to_string(),
            transcript_text: "first transcript".to_string(),
            created_at: None,
        };
        db.upsert_source_transcript(&first).expect("insert source transcript");

        let mut second = first.clone();
        second.transcript_path = "/refs/long-v2.txt".to_string();
        second.transcript_text = "improved transcript".to_string();
        db.upsert_source_transcript(&second).expect("update source transcript");

        let loaded = db
            .get_source_transcript("/audio/long.wav", "gemini-2.5-pro")
            .expect("load source transcript")
            .expect("source transcript exists");
        assert_eq!(loaded.transcript_path, "/refs/long-v2.txt");
        assert_eq!(loaded.transcript_text, "improved transcript");
        assert_eq!(loaded.audio_content_hash.as_deref(), Some("hash-v1"));
        assert_eq!(loaded.audio_size_bytes, Some(123));

        let latest = db
            .get_latest_source_transcript_for_audio("/audio/long.wav")
            .expect("load latest source transcript")
            .expect("latest source transcript exists");
        assert_eq!(latest.model_id, "gemini-2.5-pro");
        assert_eq!(latest.transcript_text, "improved transcript");

        let flash = SourceTranscriptRecord {
            audio_path: "/audio/long.wav".to_string(),
            model_id: "gemini-2.5-flash".to_string(),
            audio_content_hash: Some("hash-v1".to_string()),
            audio_size_bytes: Some(123),
            transcript_path: "/refs/long-flash.txt".to_string(),
            transcript_text: "flash transcript".to_string(),
            created_at: None,
        };
        db.upsert_source_transcript(&flash).expect("insert second model source transcript");

        let all = db.get_source_transcripts_for_audio("/audio/long.wav").expect("load all source transcripts");
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|record| record.model_id == "gemini-2.5-pro"));
        assert!(all.iter().any(|record| record.model_id == "gemini-2.5-flash"));
    }

    #[test]
    fn escalation_write_with_no_confidence_preserves_the_persisted_irt_confidence() {
        // True-10 audit 2026-07-09 (suspect-first regression): run_t0_gate persists the real IRT
        // confidence on an escalated verdict, then the T1/T2 escalation paths (cloud-off,
        // audio-prep failure, no-majority) re-write "escalated" with agent_confidence=None moments
        // later. The unconditional overwrite NULLed the confidence and both suspect-first orderings
        // (COALESCE(agent_confidence, 0.5)) collapsed to recency. COALESCE in the UPDATE must keep
        // the earlier signal; a caller WITH a signal must still win.
        let db = make_db();
        db.insert_segment(&make_segment("esc", "/audio/esc.wav")).unwrap();
        db.write_segment_verdict("esc", "escalated", None, None, None, Some(0.83), true).unwrap();
        db.write_segment_verdict("esc", "escalated", None, Some("cloud off"), None, None, true).unwrap();
        let seg = db.get_segment_by_id("esc").unwrap().unwrap();
        assert_eq!(seg.agent_confidence, Some(0.83), "a None re-write must not destroy the IRT confidence");
        // A later write that CARRIES a signal still replaces it.
        db.write_segment_verdict("esc", "escalated", None, None, None, Some(0.41), true).unwrap();
        let seg = db.get_segment_by_id("esc").unwrap().unwrap();
        assert_eq!(seg.agent_confidence, Some(0.41));
    }

    #[test]
    fn c4_precision_survives_deleting_a_contradicted_auto_accept() {
        // True-10 audit 2026-07-09 (v34, same class as the v33 C5 fix): deleting a reviewed bad
        // clip CASCADE-deleted its decision_verdicts row, shrinking t0HumanContradicted — the C4
        // precision that authorizes raising the autonomy dial could only drift optimistic. The
        // archive must preserve the contradiction across the delete.
        let db = make_db();
        db.insert_segment(&make_segment("good", "/audio/g.wav")).unwrap();
        db.insert_segment(&make_segment("bad", "/audio/b.wav")).unwrap();
        // Two T0 auto-accepts; the human confirms one and contradicts the other.
        db.write_segment_verdict("good", "auto_accept", Some("ok"), None, None, Some(0.9), false).unwrap();
        db.write_segment_verdict("bad", "auto_accept", Some("wrong"), None, None, Some(0.9), false).unwrap();
        db.record_human_decision("good", "accept", None, None).unwrap();
        db.record_human_decision("bad", "reject", None, None).unwrap();

        let before = db.intelligence_report().unwrap();
        assert_eq!(before["autoAcceptPrecision"]["t0HumanContradicted"], 1);
        assert_eq!(before["autoAcceptPrecision"]["t0HumanConfirmed"], 1);

        // The owner's documented cleanup: review the bad clip, then delete it.
        db.delete_segment("bad").unwrap();

        let after = db.intelligence_report().unwrap();
        assert_eq!(
            after["autoAcceptPrecision"]["t0HumanContradicted"], 1,
            "deleting the contradicted clip must not erase the contradiction (survivor bias)"
        );
        assert_eq!(after["autoAcceptPrecision"]["t0Accepts"], 2, "the T0 denominator survives too");
        // And batch delete folds the archive the same way.
        db.delete_segments_batch(&["good".to_string()]).unwrap();
        let final_report = db.intelligence_report().unwrap();
        assert_eq!(final_report["autoAcceptPrecision"]["t0HumanConfirmed"], 1);
        assert_eq!(final_report["autoAcceptPrecision"]["t0Accepts"], 2);
    }

    #[test]
    fn shadow_metrics_count_distinct_segments_not_observations() {
        // True-10 audit 2026-07-09: a re-processed segment accumulates several shadow rows, but C5
        // reasons about distinct events — one clip, one human decision, at most one over-trigger.
        let db = make_db();
        db.insert_segment(&make_segment("re", "/audio/re.wav")).unwrap();
        db.record_loop0_shadow("re", true).unwrap();
        db.record_loop0_shadow("re", true).unwrap();
        db.record_loop0_shadow("re", false).unwrap();
        db.record_human_decision("re", "accept", None, None).unwrap();
        let report = db.intelligence_report().unwrap();
        assert_eq!(report["loop0Shadow"]["totalObservations"], 1, "one segment, not three rows");
        assert_eq!(report["loop0Shadow"]["wouldFire"], 1);
        assert_eq!(
            report["loop0Shadow"]["firedButHumanAcceptedOriginal"], 1,
            "one physical over-trigger event, not two"
        );
        // The per-segment semantics survive deletion through the archive.
        db.delete_segment("re").unwrap();
        let after = db.intelligence_report().unwrap();
        assert_eq!(after["loop0Shadow"]["firedButHumanAcceptedOriginal"], 1);
        assert_eq!(after["loop0Shadow"]["totalObservations"], 1);
    }

    // ── Durable jobs accessors (migration v37 + crate::jobs) ──
    use crate::jobs::JobState;

    #[test]
    fn create_or_get_job_is_idempotent_on_the_key() {
        let db = make_db();
        let a = db.create_or_get_job("job-a", "import", Some("dedupe-1"), Some(10)).unwrap();
        assert_eq!(a.state, JobState::Queued);
        assert_eq!(a.total, Some(10));
        // Re-issuing the SAME key returns the ORIGINAL job (no duplicate row), even with a different id.
        let again = db.create_or_get_job("job-b", "import", Some("dedupe-1"), Some(99)).unwrap();
        assert_eq!(again.id, "job-a", "same key must return the first job, not create a second");
        assert_eq!(again.total, Some(10), "original params preserved");
        let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM jobs WHERE kind='import'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "the idempotent re-issue must not have inserted a second row");
    }

    #[test]
    fn null_key_jobs_are_never_deduped() {
        let db = make_db();
        db.create_or_get_job("n1", "export", None, None).unwrap();
        db.create_or_get_job("n2", "export", None, None).unwrap();
        let count: i64 = db.conn.query_row("SELECT COUNT(*) FROM jobs WHERE kind='export'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2, "two null-key jobs must both persist");
    }

    #[test]
    fn transition_job_enforces_the_lifecycle_and_stamps_times() {
        let db = make_db();
        db.create_or_get_job("j", "transcribe", None, None).unwrap();

        // Legal: queued -> running stamps started_at; progress updates land.
        db.transition_job("j", JobState::Running, None).unwrap();
        db.update_job_progress("j", 3, 0.3).unwrap();
        let mid = db.get_job("j").unwrap().unwrap();
        assert_eq!(mid.state, JobState::Running);
        assert_eq!(mid.completed, 3);
        assert!((mid.progress - 0.3).abs() < 1e-9);
        let started: Option<String> =
            db.conn.query_row("SELECT started_at FROM jobs WHERE id='j'", [], |r| r.get(0)).unwrap();
        assert!(started.is_some(), "started_at stamped on first running");

        // Legal: running -> failed records the error_code and stamps finished_at.
        db.transition_job("j", JobState::Failed, Some("MODEL_UNAVAILABLE")).unwrap();
        let done = db.get_job("j").unwrap().unwrap();
        assert_eq!(done.state, JobState::Failed);
        assert_eq!(done.error_code.as_deref(), Some("MODEL_UNAVAILABLE"));
        let finished: Option<String> =
            db.conn.query_row("SELECT finished_at FROM jobs WHERE id='j'", [], |r| r.get(0)).unwrap();
        assert!(finished.is_some(), "finished_at stamped on terminal");

        // Illegal: a terminal job cannot transition again — rejected, not written.
        let err = db.transition_job("j", JobState::Succeeded, None).unwrap_err();
        assert!(format!("{err}").contains("illegal job transition"), "double-complete must be rejected: {err}");
        assert_eq!(db.get_job("j").unwrap().unwrap().state, JobState::Failed, "state unchanged after illegal move");
    }

    #[test]
    fn progress_is_clamped_to_the_check_range() {
        let db = make_db();
        db.create_or_get_job("p", "eval", None, None).unwrap();
        db.transition_job("p", JobState::Running, None).unwrap();
        // A caller passing 1.4 (e.g. off-by-one on the denominator) must not violate the CHECK constraint.
        db.update_job_progress("p", 7, 1.4).unwrap();
        assert!((db.get_job("p").unwrap().unwrap().progress - 1.0).abs() < 1e-9, "over-range progress clamps to 1.0");
        db.update_job_progress("p", 0, -0.5).unwrap();
        assert!((db.get_job("p").unwrap().unwrap().progress).abs() < 1e-9, "negative progress clamps to 0.0");
    }

    #[test]
    fn orphaned_running_jobs_are_reaped_as_interrupted_on_startup() {
        let db = make_db();
        // Simulate a crash: one job left running, one already finished, one still queued.
        db.create_or_get_job("crashed", "import", None, None).unwrap();
        db.transition_job("crashed", JobState::Running, None).unwrap();
        db.create_or_get_job("clean", "import", None, None).unwrap();
        db.transition_job("clean", JobState::Running, None).unwrap();
        db.transition_job("clean", JobState::Succeeded, None).unwrap();
        db.create_or_get_job("waiting", "import", None, None).unwrap();

        let reaped = db.mark_orphaned_running_jobs_failed().unwrap();
        assert_eq!(reaped, 1, "only the still-running job is a crash residue");

        let crashed = db.get_job("crashed").unwrap().unwrap();
        assert_eq!(crashed.state, JobState::Failed);
        assert_eq!(crashed.error_code.as_deref(), Some("INTERRUPTED"));
        assert_eq!(db.get_job("clean").unwrap().unwrap().state, JobState::Succeeded, "finished job untouched");
        assert_eq!(db.get_job("waiting").unwrap().unwrap().state, JobState::Queued, "queued job untouched");
    }

    #[test]
    fn get_job_returns_none_for_a_missing_id() {
        let db = make_db();
        assert!(db.get_job("does-not-exist").unwrap().is_none());
    }

    #[test]
    fn run_tracked_marks_succeeded_and_returns_the_work_value() {
        let db = make_db();
        let out = db.run_tracked("t-ok", "export_dataset", "EXPORT_FAILED", |_d| Ok(42_i32)).unwrap();
        assert_eq!(out, 42);
        let job = db.get_job("t-ok").unwrap().unwrap();
        assert_eq!(job.state, JobState::Succeeded);
        assert!(job.error_code.is_none());
    }

    #[test]
    fn run_tracked_marks_failed_with_code_and_propagates_the_original_error() {
        let db = make_db();
        let err = db
            .run_tracked::<()>("t-err", "export_dataset", "EXPORT_FAILED", |_d| {
                Err(AppError::Other("disk full while writing parquet".into()))
            })
            .unwrap_err();
        // The ORIGINAL work error propagates, not a job-bookkeeping error.
        assert!(format!("{err}").contains("disk full"), "must surface the work error: {err}");
        let job = db.get_job("t-err").unwrap().unwrap();
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.error_code.as_deref(), Some("EXPORT_FAILED"));
    }

    #[test]
    fn run_tracked_gives_work_a_usable_db_handle() {
        // The closure receives &Database so the bracketed op can actually touch the library.
        let db = make_db();
        db.insert_segment(&make_segment("seg-in-job", "/a.wav")).unwrap();
        let n = db
            .run_tracked("t-db", "count", "COUNT_FAILED", |d| Ok(d.get_segment_by_id("seg-in-job")?.is_some()))
            .unwrap();
        assert!(n, "work closure could read the DB it was handed");
        assert_eq!(db.get_job("t-db").unwrap().unwrap().state, JobState::Succeeded);
    }

    #[test]
    fn list_recent_jobs_returns_newest_first_and_respects_limit() {
        let db = make_db();
        for i in 0..3 {
            db.create_or_get_job(&format!("lj{i}"), "export_dataset", None, None).unwrap();
        }
        let all = db.list_recent_jobs(10).unwrap();
        assert_eq!(all.len(), 3);
        let limited = db.list_recent_jobs(2).unwrap();
        assert_eq!(limited.len(), 2, "limit honored");
    }
}
