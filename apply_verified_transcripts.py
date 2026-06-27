import sqlite3
import json
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

_HERE = os.path.dirname(os.path.abspath(__file__))   # == the CORTEX repo root

MANIFEST_VERIFIED_PATH = os.path.join(_HERE, "kurdish_datasets", "manifest_verified.json")
DB_PATH = os.path.join(os.environ['APPDATA'], 'cortex-speech', 'cortex-speech.db')

def apply_verified_transcripts():
    if not os.path.exists(MANIFEST_VERIFIED_PATH):
        print(f"Error: {MANIFEST_VERIFIED_PATH} not found.")
        return

    with open(MANIFEST_VERIFIED_PATH, "r", encoding="utf-8") as f:
        verified_data = json.load(f)

    conn = sqlite3.connect(DB_PATH)
    cursor = conn.cursor()

    updates = 0
    quarantines = 0
    
    for item in verified_data:
        seg_id = item["id"]
        status = item.get("status")
        
        if status == "gold":
            verified_text = item.get("verified_transcript", "")
            if verified_text:
                # Update normalized_transcript with the perfectly verified text
                cursor.execute("""
                    UPDATE speech_segments 
                    SET normalized_transcript = ?, 
                        updated_at = CURRENT_TIMESTAMP
                    WHERE id = ?
                """, (verified_text, seg_id))
                updates += 1
        elif status == "quarantine":
            # Blank out the transcript to effectively quarantine it from training
            cursor.execute("""
                UPDATE speech_segments 
                SET normalized_transcript = '', 
                    updated_at = CURRENT_TIMESTAMP
                WHERE id = ?
            """, (seg_id,))
            quarantines += 1

    conn.commit()
    conn.close()

    print(f"Successfully applied perfect verified transcripts to database.")
    print(f"Updated (Gold): {updates} segments")
    print(f"Quarantined (Blanked): {quarantines} segments")

if __name__ == "__main__":
    apply_verified_transcripts()
