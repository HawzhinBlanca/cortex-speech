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
    pub fn open_with_retry(path: &str) -> AppResult<Self> {
        match Self::open(path) {
            Ok(db) => {
                match db.integrity_check() {
                    Ok(result) if result.trim() == "ok" => {
                        return Ok(db);
                    }
                    Ok(result) => {
                        tracing::error!("Database integrity check failed on open; quarantining database: {result}");
                    }
                    Err(e) => {
                        tracing::error!("Database integrity check errored on open; quarantining database: {e}");
                    }
                }
                drop(db);
                recover_database_at(path)?;
                Self::open(path)
            }
            Err(e) => {
                tracing::error!("Failed to open database: {e}. Attempting recovery...");
                recover_database_at(path)?;
                Self::open(path)
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
                self.conn.execute("RELEASE batch_insert", [])?;
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
                    update_stmt.execute(params![
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
                    updated += 1;
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
                self.conn.execute("RELEASE merge_json", [])?;
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
                self.conn.execute("RELEASE batch_delete", [])?;
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
        query.push_str(" ORDER BY created_at DESC");

        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map([], Self::map_row)?;
        let mut segments = Vec::new();
        for row in rows {
            segments.push(row?);
        }
        Ok(segments)
    }

    pub fn search_segments(&self, text: &str) -> AppResult<Vec<SpeechSegment>> {
        let query = normalize_search_query(text);
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, audio_path, raw_transcript, normalized_transcript,
                    annotated_transcript, alignment_json, duration_ms, speaker_id, verified,
                    confidence, ctc_score, clipping_ratio, rms_db, snr_db, split, ood_score,
                    verdict, verdict_transcript, rationale, evidence_json,
                    agent_confidence, escalated, human_decision, corrected_at, is_gold,
                    alignment_quality
             FROM speech_segments
             WHERE id IN (SELECT id FROM segments_fts WHERE segments_fts MATCH ?1)
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![query], Self::map_row)?;
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
            "SELECT {col_list} FROM speech_segments WHERE id IN ({}) ORDER BY created_at DESC",
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
             ORDER BY created_at DESC",
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

    pub fn update_segment_consensus_batch(&self, updates: &[(String, String, String, f64)]) -> AppResult<()> {
        self.conn.execute("SAVEPOINT consensus_batch", [])?;
        let result: AppResult<()> = (|| {
            let mut stmt = self.conn.prepare(
                "UPDATE speech_segments 
                 SET raw_transcript = ?2,
                     normalized_transcript = ?3,
                     confidence = ?4,
                     updated_at = datetime('now')
                 WHERE id = ?1",
            )?;
            for (seg_id, cons, norm, conf) in updates {
                stmt.execute(params![seg_id, cons, norm, conf])?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.conn.execute("RELEASE consensus_batch", [])?;
                self.track_write()?;
                Ok(())
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

        let (is_gold, raw_transcript, normalized_transcript, annotated_transcript, verdict_transcript): (
            i32,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = self.conn.query_row(
            "SELECT COALESCE(is_gold, 0), raw_transcript, normalized_transcript, annotated_transcript, verdict_transcript
             FROM speech_segments
             WHERE id = ?1",
            params![segment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
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

        self.conn.execute(
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
            if let (Some(wrong), Some(fix)) = (rejected_learning_transcript, corrected_transcript) {
                let example_id = uuid::Uuid::new_v4().to_string();
                self.conn.execute(
                    "INSERT INTO agent_examples (id, segment_id, wrong_transcript, human_fix)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![example_id, segment_id, wrong, fix],
                )?;
            }
        }

        self.track_write()?;
        Ok(())
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
             ORDER BY COALESCE(agent_confidence, 0.5) ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], Self::map_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
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
