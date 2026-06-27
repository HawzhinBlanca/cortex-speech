import subprocess
import re
import json
import uuid
import os
import sys

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

_models_dir = os.path.join(_HERE, "cortex-speech-app", "src-tauri", "models")
_vad_model = os.path.join(_models_dir, "silero_vad_v4.onnx")
_tokens = os.path.join(_models_dir, "omniasr-ctc-300m", "tokens.txt")
_asr_model = os.path.join(_models_dir, "omniasr-ctc-300m", "model.int8.onnx")

audio_path = os.environ.get("CORTEX_AUDIO", "")
if not audio_path:
    sys.exit("set CORTEX_AUDIO to the path of the input .wav file")

cmd = [
    r".\sherpa-onnx-v1.13.2-win-x64-shared-MD-Release\bin\sherpa-onnx-vad-with-offline-asr.exe",
    f"--silero-vad-model={_vad_model}",
    f"--tokens={_tokens}",
    f"--omnilingual-asr-model={_asr_model}",
    "--num-threads=4",
    audio_path
]

print(f"Running command: {' '.join(cmd)}")
result = subprocess.run(cmd, capture_output=True, text=True, encoding='utf-8')

lines = result.stdout.splitlines()
segments = []

pattern = re.compile(r'(\d+\.\d+) -- (\d+\.\d+): (.*)')

for line in lines:
    match = pattern.search(line)
    if match:
        start_sec = float(match.group(1))
        end_sec = float(match.group(2))
        text = match.group(3).strip()
        if not text: continue
        
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
            }, ensure_ascii=False)
        }
        segments.append(seg)

output_path = 'Technoshan_P01_perfect_dataset.json'
with open(output_path, 'w', encoding='utf-8') as f:
    json.dump(segments, f, ensure_ascii=False, indent=2)

print(f"Successfully processed {len(segments)} segments.")
if segments:
    print(f"Sample transcript: {segments[0]['normalizedTranscript']}")
