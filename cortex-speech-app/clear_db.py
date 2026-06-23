import sqlite3
import os

db_path = os.path.expandvars(r"%APPDATA%\cortex-speech\cortex-speech.db")
if os.path.exists(db_path):
    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()
    cursor.execute("SELECT name FROM sqlite_master WHERE type='table';")
    tables = [row[0] for row in cursor.fetchall()]
    
    for table in ["speech_segments", "segment_hypotheses", "segment_edits", "dataset_runs", "agent_stage_events", "agent_import_reports"]:
        if table in tables:
            cursor.execute(f"DELETE FROM {table};")
            print(f"Cleared table: {table}")
            
    conn.commit()
    conn.close()
    print("Database cleared successfully.")
else:
    print("Database file does not exist yet.")
