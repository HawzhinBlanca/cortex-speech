use crate::error::{AppError, AppResult};
use rusqlite::{backup, params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

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
    query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The only split labels the export/stats math understands.
const VALID_SPLITS: &[&str] = &["train", "validation", "test"];

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

fn learning_text_key(text: &str) -> String {
    text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

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
                    Ok(result) => {
                        // A non-"ok" string is SQLite reporting genuine page corruption: quarantine.
                        tracing::error!("Database integrity check failed on open; quarantining database: {result}");
                    }
                    Err(e) if is_corruption_error(&e) => {
                        tracing::error!("Database integrity check returned a corruption code on open; quarantining database: {e}");
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
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score, alignment_quality)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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
                updated_at=datetime('now')",
            params![
                seg.id, seg.audio_path, raw_nfc,
                normalized_nfc, annotated_nfc,
                seg.alignment_json, seg.duration_ms, seg.speaker_id,
                seg.verified as i32, seg.confidence, seg.ctc_score,
                seg.clipping_ratio, seg.rms_db, seg.snr_db, seg.split,
                seg.ood_score, seg.alignment_quality,
            ],
        )?;
        self.track_write()?;
        Ok(())
    }

    /// Resurrect a HARD-DELETED segment from a full in-memory snapshot, persisting EVERY column the
    /// snapshot carries — including the jury / human-review / gold-provenance fields and `created_at`
    /// that [`insert_segment`] deliberately omits.
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
        let verdict_transcript_nfc = seg.verdict_transcript.as_deref().map(to_nfc);
        self.conn.execute(
            "INSERT INTO speech_segments
                (id, created_at, audio_path, raw_transcript, normalized_transcript,
                 annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence,
                 ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                 verdict, verdict_transcript, rationale, evidence_json, agent_confidence, escalated,
                 human_decision, corrected_at, is_gold, alignment_quality, updated_at)
             VALUES (?1, COALESCE(?2, datetime('now')), ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, datetime('now'))
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
                updated_at=datetime('now')",
            params![
                seg.id, seg.created_at, seg.audio_path, raw_nfc,
                normalized_nfc, annotated_nfc,
                seg.alignment_json, seg.duration_ms, seg.speaker_id,
                seg.verified as i32, seg.confidence, seg.ctc_score,
                seg.clipping_ratio, seg.rms_db, seg.snr_db, seg.split,
                seg.ood_score, seg.verdict, verdict_transcript_nfc,
                seg.rationale, seg.evidence_json, seg.agent_confidence,
                seg.escalated as i32, seg.human_decision, seg.corrected_at,
                seg.is_gold as i32, seg.alignment_quality,
            ],
        )?;
        self.track_write()?;
        Ok(())
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
                     annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
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
                     annotated_transcript, alignment_json, duration_ms, speaker_id, verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
            )?;
            // Guard: never overwrite human decisions; only update unreviewed rows.
            let mut update_stmt = self.conn.prepare(
                "UPDATE speech_segments SET
                    audio_path=?2, raw_transcript=?3, normalized_transcript=?4, 
                    annotated_transcript=?5, alignment_json=?6, duration_ms=?7, 
                    speaker_id=?8, verified=?9, confidence=?10, ctc_score=?11, 
                    clipping_ratio=?12, rms_db=?13, snr_db=?14, split=?15, ood_score=?16, updated_at=datetime('now')
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
    pub fn update_asr_transcript_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
    ) -> AppResult<bool> {
        let rows_changed = self.conn.execute(
            "UPDATE speech_segments
             SET raw_transcript        = ?2,
                 normalized_transcript = ?3,
                 confidence            = ?4,
                 updated_at            = datetime('now')
             WHERE id = ?1
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![segment_id, raw_transcript, normalized_transcript, confidence],
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
    pub fn update_batch_transcription_if_unreviewed(
        &self,
        segment_id: &str,
        raw_transcript: &str,
        normalized_transcript: Option<&str>,
        confidence: Option<f64>,
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
                 annotated_transcript  = COALESCE(annotated_transcript, ?5),
                 updated_at            = datetime('now')
             WHERE id = ?1
               AND verified = 0
               AND (human_decision IS NULL OR human_decision = '')
               AND (verdict IS NULL OR verdict NOT IN ('human_accept','human_edit','human_reject'))",
            params![segment_id, raw_nfc, normalized_nfc, confidence, annotated_nfc],
        )?;
        self.track_write()?;
        Ok(rows_changed > 0)
    }

    pub fn delete_segment(&self, id: &str) -> AppResult<()> {
        self.conn.execute("DELETE FROM speech_segments WHERE id = ?1", params![id])?;
        self.track_write()?;
        Ok(())
    }

    pub fn delete_segments_batch(&self, ids: &[String]) -> AppResult<()> {
        self.conn.execute("SAVEPOINT batch_delete", [])?;
        let result: AppResult<()> = (|| {
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
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                    confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                    verdict, verdict_transcript, rationale, evidence_json,
                    agent_confidence, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality
             FROM speech_segments WHERE id = ?1",
        )?;
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
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                    confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                    verdict, verdict_transcript, rationale, evidence_json,
                    agent_confidence, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality
             FROM speech_segments WHERE audio_path = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![audio_path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(Self::map_row(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_segments(&self, verified: Option<bool>) -> AppResult<Vec<SpeechSegment>> {
        let col_list = "id, created_at, audio_path, raw_transcript, normalized_transcript,
                        annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                        confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                        verdict, verdict_transcript, rationale, evidence_json,
                        agent_confidence, escalated, human_decision, corrected_at, is_gold,
                        alignment_quality";
        let mut query = format!("SELECT {col_list} FROM speech_segments");
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

    pub fn search_segments(&self, text: &str) -> AppResult<Vec<SpeechSegment>> {
        let match_query = to_fts5_match(&normalize_search_query(text));
        // Whitespace-only / empty input is an empty result, not an FTS5 `MATCH ""` error.
        if match_query.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                    confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                    verdict, verdict_transcript, rationale, evidence_json,
                    agent_confidence, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality
             FROM speech_segments
             WHERE id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)
             ORDER BY created_at DESC, id ASC",
        )?;
        let rows = stmt.query_map(params![match_query], Self::map_row)?;
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
        let col_list = "id, created_at, audio_path, raw_transcript, normalized_transcript,
                        annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                        confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                        verdict, verdict_transcript, rationale, evidence_json,
                        agent_confidence, escalated, human_decision, corrected_at, is_gold,
                        alignment_quality";
        // Build a parameterised placeholder list: (?1,?2,...?N)
        let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{i}")).collect();
        let query = format!(
            "SELECT {col_list} FROM speech_segments WHERE id IN ({}) ORDER BY created_at DESC, id ASC",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), Self::map_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
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
        let backup = backup::Backup::new(&src_conn, &mut self.conn)?;
        backup.run_to_completion(5, std::time::Duration::from_millis(250), None)?;
        Ok(())
    }

    pub fn vacuum(&self) -> AppResult<()> {
        self.conn.execute("VACUUM", [])?;
        Ok(())
    }

    pub fn wal_checkpoint(&self) -> AppResult<()> {
        self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn insert_hypothesis(&self, hyp: &SegmentHypothesis) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO segment_hypotheses (segment_id, model_id, transcript, confidence)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(segment_id, model_id) DO UPDATE SET
                transcript=excluded.transcript,
                confidence=excluded.confidence,
                created_at=datetime('now')",
            params![hyp.segment_id, hyp.model_id, hyp.transcript, hyp.confidence],
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
                changed += stmt.execute(params![seg_id, cons, norm, conf])?;
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
            // Jury fields — added by Migration v11; default when column missing
            verdict: row.get(17).unwrap_or(None),
            verdict_transcript: row.get(18).unwrap_or(None),
            rationale: row.get(19).unwrap_or(None),
            evidence_json: row.get(20).unwrap_or(None),
            agent_confidence: row.get(21).unwrap_or(None),
            escalated: row.get::<_, i32>(22).unwrap_or(0) != 0,
            human_decision: row.get(23).unwrap_or(None),
            corrected_at: row.get(24).unwrap_or(None),
            is_gold: row.get::<_, i32>(25).unwrap_or(0) != 0,
            // Alignment quality — added by Migration v12; default to None when column missing
            alignment_quality: row.get(26).unwrap_or(None),
        })
    }

    // ── Jury DB helpers ───────────────────────────────────────────────────────

    /// Write a jury verdict to a segment (used by T0, T1, T2 and human review).
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
        self.conn.execute(
            "UPDATE speech_segments
             SET verdict            = ?2,
                 verdict_transcript = ?3,
                 rationale          = ?4,
                 evidence_json      = ?5,
                 agent_confidence   = ?6,
                 escalated          = ?7,
                 updated_at         = datetime('now')
             WHERE id = ?1",
            params![segment_id, verdict, transcript, rationale, evidence_json, agent_confidence, escalated as i32],
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
        // Quarantine gold at capture time (holdout exclusion is also applied at every export).
        let is_gold: i64 = self
            .conn
            .query_row("SELECT is_gold FROM speech_segments WHERE id = ?1", params![segment_id], |r| r.get(0))
            .unwrap_or(0);
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
    /// agent_examples.
    pub fn record_human_decision(
        &self,
        segment_id: &str,
        decision: &str,
        corrected_transcript: Option<&str>,
    ) -> AppResult<()> {
        let human_verdict = human_verdict_for_decision(decision)?;
        let corrected_transcript = corrected_transcript.map(str::trim).filter(|value| !value.is_empty());
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
        ): HumanDecisionContext =
            self.conn.query_row(
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
            crate::pipeline::source_audio_identity(Path::new(&audio_path))
                .ok()
                .map(|identity| identity.content_hash)
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
        if let (Some(content_hash), Some(fix), Some(wrong)) =
            (ledger_hash, corrected_transcript, wrong_side.as_deref())
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
                                (id, wrong_token, human_token, slot_key, phonetic_key, source_segment, model_version_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                mem_id,
                                mem.wrong_token,
                                mem.human_token,
                                mem.slot_key,
                                mem.phonetic_key,
                                segment_id,
                                model_version_id
                            ],
                        )?;
                    }
                }
            }
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
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, alignment_json, duration_ms, speaker_id,
                    verified, confidence, ctc_score, clipping_ratio, rms_db, snr_db,
                    split, ood_score,
                    verdict, verdict_transcript, rationale, evidence_json,
                    agent_confidence, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality
             FROM speech_segments
             WHERE escalated = 1
               AND (human_decision IS NULL OR human_decision = '')
             ORDER BY COALESCE(agent_confidence, 0.5) ASC, id ASC
             LIMIT ?1",
        )?;
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
        dedicated
            .write_segment_verdict("s1", "jury_accept", Some("hi"), None, None, Some(0.9), false)
            .unwrap();
        let seen = primary.get_segment_by_id("s1").unwrap().unwrap();
        assert_eq!(seen.verdict.as_deref(), Some("jury_accept"));
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
    fn search_segments_tie_order_is_deterministic_by_id() {
        let db = make_db();
        // Insert in non-sorted id order; all share the search token and (in this fast test) the same
        // 1s-resolution created_at, so the only stable order is the id tiebreaker.
        for id in ["seg_m", "seg_a", "seg_z"] {
            let mut s = make_segment(id, &format!("/{id}.wav"));
            s.raw_transcript = "uniquesearchtoken body".to_string();
            db.insert_segment(&s).unwrap();
        }
        // Pin all three created_at to ONE value so the tie is GUARANTEED. created_at defaults to
        // datetime('now') at 1s resolution, so if the three inserts straddle a one-second clock tick
        // they no longer tie, ORDER BY created_at DESC reorders them, and the id-tiebreaker assertions
        // below flake (~1-in-N runs, under load). Forcing equal timestamps isolates the id tiebreaker.
        db.conn
            .execute("UPDATE speech_segments SET created_at = '2026-01-01 00:00:00'", [])
            .unwrap();
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
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
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
        for q in ["\"hello", "foo:bar", "*", "(", "NEAR(a b", "a AND", "OR", "^", "-foo", ")"] {
            assert!(db.search_segments(q).is_ok(), "query {q:?} must not error");
        }
        // A real token still finds the segment, and a quote next to it doesn't break matching.
        assert_eq!(db.search_segments("hello").unwrap().len(), 1, "literal token still matches");
        assert_eq!(db.search_segments("\"hello\"").unwrap().len(), 1, "quoted token matches too");
        // Whitespace-only input is an empty result, not an error.
        assert!(db.search_segments("   ").unwrap().is_empty(), "blank query -> empty, not error");
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
            .execute(
                "UPDATE speech_segments SET verdict='human_edit', human_decision='edit' WHERE id='locked-1'",
                [],
            )
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
            .update_batch_transcription_if_unreviewed("verified-1", "fresh asr", Some("fresh asr"), Some(0.9), "fresh asr")
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
            .update_batch_transcription_if_unreviewed("fresh-1", "new asr", Some("new asr"), Some(0.8), "new asr")
            .expect("update fresh");
        assert!(updated, "an unreviewed row is updated");
        let after = db.get_segment_by_id("fresh-1").unwrap().unwrap();
        assert_eq!(after.raw_transcript, "new asr");
        assert_eq!(after.annotated_transcript.as_deref(), Some("new asr"), "annotation seeded when empty");

        // (d): an unverified row the user annotated (without verifying) keeps that annotation; only
        // the ASR fields refresh — the seed is ignored because COALESCE reads the CURRENT row.
        let mut annotated = make_segment("annot-1", "/c.wav");
        annotated.raw_transcript = "old".to_string();
        annotated.annotated_transcript = Some("user typed".to_string());
        db.insert_segment(&annotated).expect("insert annotated");
        let updated = db
            .update_batch_transcription_if_unreviewed("annot-1", "new asr", Some("new asr"), Some(0.7), "seed ignored")
            .expect("update annotated");
        assert!(updated, "an unverified annotated row still refreshes ASR");
        let after = db.get_segment_by_id("annot-1").unwrap().unwrap();
        assert_eq!(after.annotated_transcript.as_deref(), Some("user typed"), "existing annotation preserved (COALESCE)");
        assert_eq!(after.raw_transcript, "new asr", "raw ASR refreshed on an unverified row");
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
        db.record_human_decision("noop-1", "edit", Some("hello world")).expect("record no-op edit");

        let ledger_rows: i64 =
            db.conn.query_row("SELECT COUNT(*) FROM corrections WHERE segment_id='noop-1'", [], |r| r.get(0)).unwrap();
        assert_eq!(ledger_rows, 0, "a no-op edit must NOT append a corrections-ledger row");

        // A genuine correction on the same kind of row DOES record a ledger entry.
        let mut seg2 = make_segment("real-1", &audio_path);
        seg2.raw_transcript = "helo wrld".to_string();
        db.insert_segment(&seg2).expect("insert real");
        db.record_human_decision("real-1", "edit", Some("hello world")).expect("record real edit");
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
        db.conn
            .execute("UPDATE speech_segments SET verdict='human_accept' WHERE id='c-lock'", [])
            .expect("lock");
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

        db.record_human_decision("learn-agent", "edit", Some("human corrected transcript")).expect("record human edit");

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

        db.record_human_decision("learn-same", "edit", Some("same text")).expect("record human edit");

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

        db.record_human_decision("led-1", "edit", Some("right text")).expect("record edit");

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

        db.record_human_decision("led-acc", "accept", None).expect("record accept");

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

        db.record_human_decision("led-missing", "edit", Some("right")).expect("edit must still succeed");

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
        db.record_human_decision("mem-1", "edit", Some("ئەو ساڵە خراپ بوو")).expect("edit");

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
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو")).expect("edit");
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
        db.record_human_decision("mem-gold", "edit", Some("ئەو ساڵە خراپ بوو")).expect("edit");

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
        db.record_human_decision("lm-1", "edit", Some("ئەو ساڵە خراپ بوو")).expect("edit");

        let mems = db.load_correction_memories().expect("load");
        assert_eq!(mems.len(), 1);
        assert_eq!(mems[0].wrong_token, "باش");
        assert_eq!(mems[0].human_token, "خراپ");
        assert!(mems[0].confidence >= 1.0, "fresh memory confidence defaults to 1.0");
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
            db.record_human_decision(id, "edit", Some("ئەو ساڵە خراپ بوو")).expect("edit");
        }

        let mems = db.load_correction_memories().expect("load");
        assert_eq!(mems.len(), 1, "the repeated correction upserts to a single memory");
        assert_eq!(mems[0].hit_count, 1, "confirmed twice -> hit_count 1, past the anti-one-off guard");

        let out = crate::corrections::apply_memories(
            "ئەو ساڵە باش بوو",
            &mems,
            &crate::corrections::FiringConfig::default(),
        );
        assert_eq!(out, "ئەو ساڵە خراپ بوو", "capture x2 -> DB -> load -> fire reproduces the human fix");
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
}
