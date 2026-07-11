import sqlite3
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

db = os.path.join(os.environ['APPDATA'], 'cortex-speech', 'cortex-speech.db')
conn = sqlite3.connect(db)
# Audio-path filter is configurable (CORTEX_AUDIO_LIKE); defaults to all segments so no private
# local folder name is hardcoded in this public repo.
audio_like = os.environ.get('CORTEX_AUDIO_LIKE', '%')
cursor = conn.execute('SELECT audio_path, COUNT(*) FROM speech_segments WHERE audio_path LIKE ? GROUP BY audio_path', (audio_like,))
rows = cursor.fetchall()
conn.close()

print(f"Total files in database from Kurdish folder: {len(rows)}")
for idx, r in enumerate(rows):
    path = r[0]
    count = r[1]
    name = os.path.basename(path)
    print(f"{idx+1}. {count} segments — {name}")
