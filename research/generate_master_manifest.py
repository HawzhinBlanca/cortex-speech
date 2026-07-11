import os
import sys
import json

sys.stdout.reconfigure(encoding='utf-8')

_HERE = os.path.dirname(os.path.abspath(__file__))   # == the CORTEX repo root

DATASETS_DIR = os.path.join(_HERE, "kurdish_datasets")
MASTER_CSV = os.path.join(DATASETS_DIR, "metadata.csv")
MASTER_JSON = os.path.join(DATASETS_DIR, "manifest.json")

def main():
    print("====================================================")
    print("CODEX Global Manifest Compiler")
    print(f"Target Directory: {DATASETS_DIR}")
    print("====================================================")

    if not os.path.exists(DATASETS_DIR):
        print(f"Error: Directory {DATASETS_DIR} does not exist.")
        return

    subfolders = [f for f in os.listdir(DATASETS_DIR) if os.path.isdir(os.path.join(DATASETS_DIR, f))]
    
    global_metadata = []
    
    for folder in sorted(subfolders):
        manifest_json = os.path.join(DATASETS_DIR, folder, "manifest.json")
        if not os.path.exists(manifest_json):
            continue

        try:
            with open(manifest_json, "r", encoding="utf-8") as f:
                records = json.load(f)
            
            for r in records:
                # Update the wav_file path to be relative to the datasets root
                # e.g., "1Cheshti_Mjewr_Hajar_MukryaniBashi1/wavs/1Cheshti_Mjewr_Hajar_MukryaniBashi1_abc123.wav"
                rel_wav = f"{folder}/{r['wav_file']}"
                
                global_metadata.append({
                    "id": r["id"],
                    "wav_file": rel_wav,
                    "transcript": r["transcript"],
                    "duration_ms": r["duration_ms"],
                    "speaker_id": folder # Use folder name as speaker ID
                })
        except Exception as e:
            print(f"Error reading {manifest_json}: {e}")

    if not global_metadata:
        print("No speech segments found to compile.")
        return

    print(f"Compiling {len(global_metadata)} records into global manifests...")

    # Write global metadata.csv (LJSpeech style: relative_path|transcript)
    try:
        with open(MASTER_CSV, "w", encoding="utf-8") as f:
            for item in global_metadata:
                # Standard LJSpeech uses | as delimiter; ensure transcript doesn't contain it
                clean_transcript = item["transcript"].replace("|", " ")
                f.write(f"{item['wav_file']}|{clean_transcript}\n")
        print(f"Success: Wrote global CSV manifest to: {MASTER_CSV}")
    except Exception as e:
        print(f"Failed to write CSV manifest: {e}")

    # Write global manifest.json (detailed segment data including speaker labels)
    try:
        with open(MASTER_JSON, "w", encoding="utf-8") as f:
            json.dump(global_metadata, f, ensure_ascii=False, indent=2)
        print(f"Success: Wrote global JSON manifest to: {MASTER_JSON}")
    except Exception as e:
        print(f"Failed to write JSON manifest: {e}")

    print("\n====================================================")
    print("Master Manifest Compilation Complete!")
    print(f"Total entries: {len(global_metadata)}")
    print("====================================================")

if __name__ == "__main__":
    main()
