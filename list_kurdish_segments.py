import sqlite3
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

db = os.path.join(os.environ['APPDATA'], 'cortex-speech', 'cortex-speech.db')
conn = sqlite3.connect(db)
cursor = conn.execute('SELECT audio_path, COUNT(*) FROM speech_segments WHERE audio_path LIKE ? GROUP BY audio_path', ('%CORTEX_AUDIO_LIKE%',))
rows = cursor.fetchall()
conn.close()

print(f"Total files in database from Kurdish folder: {len(rows)}")
for idx, r in enumerate(rows):
    path = r[0]
    count = r[1]
    name = os.path.basename(path)
    print(f"{idx+1}. {count} segments — {name}")
