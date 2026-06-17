import subprocess
import re
import json
import uuid

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

cmd = [
    r".\sherpa-onnx-v1.13.2-win-x64-shared-MD-Release\bin\sherpa-onnx-vad-with-offline-asr.exe",
    "--silero-vad-model=C:\\Users\\hawzh\\Desktop\\CORTEX\\cortex-speech-app\\src-tauri\\models\\silero_vad_v4.onnx",
    "--tokens=C:\\Users\\hawzh\\Desktop\\CORTEX\\cortex-speech-app\\src-tauri\\models\\omniasr-ctc-300m\\tokens.txt",
    "--omnilingual-asr-model=C:\\Users\\hawzh\\Desktop\\CORTEX\\cortex-speech-app\\src-tauri\\models\\omniasr-ctc-300m\\model.int8.onnx",
    "--num-threads=4",
    "C:\\Users\\hawzh\\Desktop\\Lamo Voice Samples\\290426_ZP_EP017_Technoshan_P01-esv2-speech-50p.wav"
]

print(f"Running command: {' '.join(cmd)}")
result = subprocess.run(cmd, capture_output=True, text=True, encoding='utf-8')

lines = result.stdout.splitlines()
segments = []
audio_path = r"C:\Users\hawzh\Desktop\Lamo Voice Samples\290426_ZP_EP017_Technoshan_P01-esv2-speech-50p.wav"

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
