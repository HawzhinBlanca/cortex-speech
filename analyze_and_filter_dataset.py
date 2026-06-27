import os
import sys
import json
import wave
import re
import shutil

sys.stdout.reconfigure(encoding='utf-8')

_HERE = os.path.dirname(os.path.abspath(__file__))   # == the CORTEX repo root

DATASETS_DIR = os.path.join(_HERE, "kurdish_datasets")
MANIFEST_PATH = os.path.join(DATASETS_DIR, "manifest.json")
GOLD_DIR = os.path.join(DATASETS_DIR, "gold_dataset")
QUARANTINE_DIR = os.path.join(DATASETS_DIR, "quarantine_dataset")

# Standard Sorani speaking rate parameters
MIN_CPS = 4.0   # Characters per second (very slow speech)
MAX_CPS = 28.0  # Characters per second (extremely fast/ASR hallucination)

MIN_DURATION_MS = 800   # 0.8 seconds minimum
MAX_DURATION_MS = 30000 # 30.0 seconds maximum

def detect_repetition(text):
    words = text.split()
    if len(words) < 3:
        return False
    # Check for repeated word sequences (e.g., word word word)
    for i in range(len(words) - 2):
        if words[i] == words[i+1] == words[i+2]:
            return True
    return False

def count_kurdish_chars(text):
    # Kurdish specific chars: پ چ ژ ڤ ک گ ڵ ڕ ۆ ێ ە
    kurdish_regex = re.compile(r'[\u0600-\u06FF\u200C-\u200F]')
    return len(kurdish_regex.findall(text))

def main():
    print("====================================================")
    print("CODEX Gold Dataset Curation and Filtration Pipeline")
    print("====================================================")

    if not os.path.exists(MANIFEST_PATH):
        print(f"Error: manifest.json not found at {MANIFEST_PATH}")
        return

    with open(MANIFEST_PATH, "r", encoding="utf-8") as f:
        manifest_data = json.load(f)

    print(f"Total input slices: {len(manifest_data)}")

    gold_slices = []
    quarantined_slices = []

    # Statistics counters
    repetition_flags = 0
    duration_flags = 0
    cps_flags = 0
    non_kurdish_flags = 0
    empty_flags = 0

    for item in manifest_data:
        wav_rel = item["wav_file"]
        wav_full = os.path.join(DATASETS_DIR, wav_rel)
        transcript = item["transcript"].strip()
        duration_ms = item["duration_ms"]
        duration_sec = duration_ms / 1000.0

        # Flag 1: Empty transcript
        if not transcript:
            item["quarantine_reason"] = "Empty Transcript"
            quarantined_slices.append(item)
            empty_flags += 1
            continue

        # Flag 2: Extreme Duration Outlier
        if duration_ms < MIN_DURATION_MS or duration_ms > MAX_DURATION_MS:
            item["quarantine_reason"] = f"Duration Outlier ({duration_sec:.2f}s)"
            quarantined_slices.append(item)
            duration_flags += 1
            continue

        # Flag 3: Speaking Rate (Characters per second) Outlier
        char_count = len(transcript)
        cps = char_count / duration_sec if duration_sec > 0 else 0
        if cps < MIN_CPS or cps > MAX_CPS:
            item["quarantine_reason"] = f"Speaking Rate Outlier ({cps:.1f} CPS)"
            quarantined_slices.append(item)
            cps_flags += 1
            continue

        # Flag 4: Text repetition (hallucinations common in CTC models)
        if detect_repetition(transcript):
            item["quarantine_reason"] = "Repetitive Words/Hallucination"
            quarantined_slices.append(item)
            repetition_flags += 1
            continue

        # Flag 5: Latin character contamination (English words)
        if re.search(r'[a-zA-Z]', transcript):
            item["quarantine_reason"] = "Latin Character Contamination"
            quarantined_slices.append(item)
            non_kurdish_flags += 1
            continue

        # Flag 6: Raw digit contamination (unspelled numbers)
        if re.search(r'\d', transcript):
            item["quarantine_reason"] = "Raw Digit Contamination"
            quarantined_slices.append(item)
            non_kurdish_flags += 1
            continue

        # Flag 7: Non-Kurdish / Non-Arabic character contamination
        # Check percentage of valid Kurdish/Arabic characters versus punctuation/numbers
        kurdish_char_count = count_kurdish_chars(transcript)
        total_letters = len(re.sub(r'\s+', '', transcript))
        if total_letters > 0 and (kurdish_char_count / total_letters) < 0.7:
            item["quarantine_reason"] = f"Character Contamination (Kurdish Ratio: {kurdish_char_count/total_letters:.2f})"
            quarantined_slices.append(item)
            non_kurdish_flags += 1
            continue

        # Passes all filters!
        gold_slices.append(item)

    print("\n--- Filtration Summary ---")
    print(f"✅ Gold Standard Slices: {len(gold_slices)} ({len(gold_slices)/len(manifest_data)*100:.1f}%)")
    print(f"⚠️ Quarantined Slices:   {len(quarantined_slices)} ({len(quarantined_slices)/len(manifest_data)*100:.1f}%)")
    print("--------------------------")
    print(f"  - Empty Transcripts: {empty_flags}")
    print(f"  - Duration Outliers: {duration_flags}")
    print(f"  - Speaking Rate Outliers: {cps_flags}")
    print(f"  - Repetition Hallucinations: {repetition_flags}")
    print(f"  - Non-Kurdish Contamination: {non_kurdish_flags}")
    print("====================================================")

    # Compile the final manifests
    print("\nDeploying Gold Dataset structure...")
    os.makedirs(os.path.join(GOLD_DIR, "wavs"), exist_ok=True)
    os.makedirs(os.path.join(QUARANTINE_DIR, "wavs"), exist_ok=True)

    gold_manifest = []
    for item in gold_slices:
        src_wav = os.path.join(DATASETS_DIR, item["wav_file"])
        filename = os.path.basename(item["wav_file"])
        dest_wav = os.path.join(GOLD_DIR, "wavs", filename)
        
        # Copy audio file to gold directory
        if os.path.exists(src_wav):
            shutil.copy2(src_wav, dest_wav)
            
        gold_manifest.append({
            "id": item["id"],
            "wav_file": f"wavs/{filename}",
            "transcript": item["transcript"],
            "duration_ms": item["duration_ms"],
            "speaker_id": item["speaker_id"]
        })

    quarantine_manifest = []
    for item in quarantined_slices:
        src_wav = os.path.join(DATASETS_DIR, item["wav_file"])
        filename = os.path.basename(item["wav_file"])
        dest_wav = os.path.join(QUARANTINE_DIR, "wavs", filename)
        
        # Copy audio file to quarantine directory
        if os.path.exists(src_wav):
            shutil.copy2(src_wav, dest_wav)
            
        quarantine_manifest.append({
            "id": item["id"],
            "wav_file": f"wavs/{filename}",
            "transcript": item["transcript"],
            "duration_ms": item["duration_ms"],
            "speaker_id": item["speaker_id"],
            "quarantine_reason": item["quarantine_reason"]
        })

    # Save Gold manifests
    with open(os.path.join(GOLD_DIR, "manifest.json"), "w", encoding="utf-8") as f:
        json.dump(gold_manifest, f, ensure_ascii=False, indent=2)
    with open(os.path.join(GOLD_DIR, "metadata.csv"), "w", encoding="utf-8") as f:
        for item in gold_manifest:
            clean_ts = item["transcript"].replace("|", " ")
            f.write(f"{item['wav_file']}|{clean_ts}\n")

    # Save Quarantine manifests
    with open(os.path.join(QUARANTINE_DIR, "manifest.json"), "w", encoding="utf-8") as f:
        json.dump(quarantine_manifest, f, ensure_ascii=False, indent=2)
    with open(os.path.join(QUARANTINE_DIR, "metadata.csv"), "w", encoding="utf-8") as f:
        for item in quarantine_manifest:
            clean_ts = item["transcript"].replace("|", " ")
            f.write(f"{item['wav_file']}|{clean_ts}|{item['quarantine_reason']}\n")

    print(f"Deploy Completed!")
    print(f"  - Gold Manifests: {os.path.join(GOLD_DIR, 'metadata.csv')}")
    print(f"  - Quarantine Manifests: {os.path.join(QUARANTINE_DIR, 'metadata.csv')}")
    print("====================================================")

if __name__ == "__main__":
    main()
