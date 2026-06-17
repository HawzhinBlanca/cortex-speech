CREATE TABLE IF NOT EXISTS speech_segments (
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

CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(
    id, raw_transcript, normalized_transcript, annotated_transcript,
    content='speech_segments',
    content_rowid='rowid'
);

CREATE INDEX IF NOT EXISTS idx_segments_verified ON speech_segments(verified);
CREATE INDEX IF NOT EXISTS idx_segments_speaker ON speech_segments(speaker_id);
CREATE INDEX IF NOT EXISTS idx_segments_created ON speech_segments(created_at);
