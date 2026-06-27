import re
import json
import uuid
import os

_HERE = os.path.dirname(os.path.abspath(__file__))   # == the CORTEX repo root

def normalize_ckb(text):
    # Arabic Kaf -> Kurdish Kaf
    text = re.sub(r'[\u0643\u06AA\u06AC]', '\u06a9', text)
    # Arabic Yah -> Kurdish Yah (Sorani)
    text = re.sub(r'[\u064A\u06D2]', '\u06cc', text)
    # Alef Maksura -> Yah
    text = text.replace('\u0649', '\u06cc')
    # Remove Tatweel
    text = text.replace('\u0640', '')
    # Remove Arabic diacritics
    text = re.sub(r'[\u064B-\u065F\u0670]', '', text)
    # Multi-space
    text = re.sub(r'\s+', ' ', text).strip()
    return text

segments = []
audio_path = os.environ.get("CORTEX_AUDIO", "")

try:
    with open('podcast_transcription.txt', 'r', encoding='utf-8') as f:
        lines = f.readlines()
except:
    with open('podcast_transcription.txt', 'r', encoding='utf-16') as f:
        lines = f.readlines()

pattern = re.compile(r'(\d+\.\d+) -- (\d+\.\d+): (.*)')

for line in lines:
    match = pattern.search(line)
    if match:
        start_sec = float(match.group(1))
        end_sec = float(match.group(2))
        text = match.group(3).strip()
        
        if not text:
            continue
            
        # Basic filtering of non-Kurdish junk (like "jяk tu male") if needed, 
        # but let's keep it for now as "raw".
        
        norm_text = normalize_ckb(text)
        
        seg = {
            "id": str(uuid.uuid4())[:8],
            "audioPath": audio_path,
            "rawTranscript": text,
            "normalizedTranscript": norm_text,
            "annotatedTranscript": norm_text,
            "durationMs": int((end_sec - start_sec) * 1000),
            "verified": False,
            "alignmentJson": json.dumps({
                "source_start_ms": int(start_sec * 1000),
                "source_end_ms": int(end_sec * 1000)
            })
        }
        segments.append(seg)

output_path = 'PODCAST-002_perfect_dataset.json'
with open(output_path, 'w', encoding='utf-8') as f:
    json.dump(segments, f, ensure_ascii=False, indent=2)

print(f"Successfully processed {len(segments)} segments.")
print(f"Dataset saved to: {output_path}")
