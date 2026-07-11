import sqlite3
import os

db_path = os.path.join(os.environ['APPDATA'], 'cortex-speech', 'cortex-speech.db')
conn = sqlite3.connect(db_path)
cursor = conn.cursor()
cursor.execute("SELECT COUNT(*) FROM speech_segments WHERE audio_path LIKE '%podcast.wav%'")
count = cursor.fetchone()[0]
print(f"Segments from podcast.wav: {count}")
conn.close()
