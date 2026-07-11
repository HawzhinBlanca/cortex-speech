import json
import sqlite3
import os

db_path = 'test_merge.db'
if os.path.exists(db_path):
    os.remove(db_path)

conn = sqlite3.connect(db_path)
conn.execute('''
CREATE TABLE speech_segments (
    id TEXT PRIMARY KEY,
    audio_path TEXT,
    raw_transcript TEXT,
    normalized_transcript TEXT,
    annotated_transcript TEXT,
    alignment_json TEXT,
    duration_ms INTEGER,
    speaker_id TEXT,
    verified INTEGER,
    created_at TEXT,
    updated_at TEXT
)
''')

with open('PODCAST-002_perfect_dataset.json', 'r', encoding='utf-8') as f:
    data = json.load(f)

for s in data:
    conn.execute('''
    INSERT INTO speech_segments (
        id, audio_path, raw_transcript, normalized_transcript, 
        annotated_transcript, alignment_json, duration_ms, 
        speaker_id, verified, created_at, updated_at
    ) VALUES (?,?,?,?,?,?,?,?,?, datetime('now'), datetime('now'))
    ''', (
        s['id'], s['audioPath'], s['rawTranscript'], 
        s['normalizedTranscript'], s['annotatedTranscript'], 
        s['alignmentJson'], s['durationMs'], s.get('speakerId'), 
        1 if s['verified'] else 0
    ))

conn.commit()
print(f"Successfully simulated merge of {len(data)} segments into {db_path}")

# Verify one record
res = conn.execute("SELECT raw_transcript FROM speech_segments LIMIT 1").fetchone()
print(f"Verification: {res[0]}")
conn.close()
