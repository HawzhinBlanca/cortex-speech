import os
import sys
import json
import sqlite3
import subprocess

# Reconfigure stdout for proper UTF-8 output
sys.stdout.reconfigure(encoding='utf-8')

DB_PATH = os.path.join(os.environ['APPDATA'], "cortex-speech", "cortex-speech.db")
INPUT_AUDIO = r"C:\Users\hawzh\Desktop\Hawzhin\كوردي\1Cheshti Mjewr⧸Hajar MukryaniBashi1.mp3"
OUTPUT_DIR = r"C:\Users\hawzh\Desktop\CORTEX\kurdish_datasets\1Cheshti_Mjewr"
WAVS_DIR = os.path.join(OUTPUT_DIR, "wavs")

def main():
    print("====================================================")
    print("Exporting Kurdish Speech Dataset for:")
    print(f"File: {INPUT_AUDIO}")
    print(f"Output Directory: {OUTPUT_DIR}")
    print("====================================================")

    if not os.path.exists(INPUT_AUDIO):
        print(f"Error: Input audio file does not exist at: {INPUT_AUDIO}")
        return

    os.makedirs(WAVS_DIR, exist_ok=True)

    # 1. Connect to DB and fetch segments
    try:
        conn = sqlite3.connect(DB_PATH)
        cursor = conn.execute(
            "SELECT id, raw_transcript, normalized_transcript, annotated_transcript, alignment_json, duration_ms "
            "FROM speech_segments WHERE audio_path LIKE ?",
            ('%1Cheshti Mjewr%',)
        )
        rows = cursor.fetchall()
        conn.close()
        print(f"Found {len(rows)} segments in SQLite database.")
    except Exception as e:
        print(f"Database error: {e}")
        return

    if not rows:
        print("No segments found to export.")
        return

    # 2. Iterate and slice using FFmpeg
    metadata = []
    skipped = 0

    print("Slicing audio chunks and writing metadata...")
    for idx, row in enumerate(rows):
        seg_id, raw, normalized, annotated, alignment_json, duration_ms = row
        
        # Decide which transcript to use (annotated -> normalized -> raw)
        transcript = annotated or normalized or raw
        if not transcript or not transcript.strip():
            skipped += 1
            continue

        try:
            align_data = json.loads(alignment_json)
            start_ms = align_data.get("source_start_ms")
            end_ms = align_data.get("source_end_ms")
            if start_ms is None or end_ms is None:
                raise ValueError("Missing start/end timestamps in alignment_json")
        except Exception as e:
            # Fallback to simple calculation if json is corrupt
            print(f"Warning: Corrupt alignment_json for segment {seg_id}: {e}")
            skipped += 1
            continue

        start_sec = start_ms / 1000.0
        end_sec = end_ms / 1000.0
        duration_sec = end_sec - start_sec

        if duration_sec <= 0:
            skipped += 1
            continue

        # File name for the sliced audio
        wav_filename = f"cheshti_mjewr_{seg_id}.wav"
        wav_path = os.path.join(WAVS_DIR, wav_filename)

        # Slice WAV using FFmpeg (Mono, 16kHz)
        cmd = [
            "ffmpeg", "-y",
            "-ss", f"{start_sec:.3f}",
            "-t", f"{duration_sec:.3f}",
            "-i", INPUT_AUDIO,
            "-ac", "1",
            "-ar", "16000",
            "-f", "wav",
            wav_path
        ]

        try:
            # Run in silence mode to avoid output clutter
            subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            
            # Record in metadata list: file_id | wav_filename | transcript
            metadata.append({
                "id": seg_id,
                "wav_file": f"wavs/{wav_filename}",
                "transcript": transcript,
                "duration_ms": duration_ms
            })
            
            # Show progress every 50 files
            if (len(metadata)) % 50 == 0:
                print(f"Processed {len(metadata)}/{len(rows)} segments...")
        except Exception as e:
            print(f"Failed to slice segment {seg_id}: {e}")
            skipped += 1

    # 3. Write metadata files
    # LJSpeech style format (CSV)
    metadata_csv_path = os.path.join(OUTPUT_DIR, "metadata.csv")
    try:
        with open(metadata_csv_path, "w", encoding="utf-8") as f:
            for item in metadata:
                # Remove any pipe characters from transcript to avoid formatting errors
                clean_ts = item["transcript"].replace("|", " ")
                f.write(f"{item['wav_file']}|{clean_ts}\n")
        
        # Also write a JSON manifest for training frameworks that prefer JSON
        metadata_json_path = os.path.join(OUTPUT_DIR, "manifest.json")
        with open(metadata_json_path, "w", encoding="utf-8") as f:
            json.dump(metadata, f, ensure_ascii=False, indent=2)

        print("\n====================================================")
        print("Export Complete!")
        print(f"Successfully exported: {len(metadata)} WAV slices.")
        print(f"Skipped segments: {skipped}")
        print(f"Metadata CSV: {metadata_csv_path}")
        print(f"Manifest JSON: {metadata_json_path}")
        print("====================================================")
    except Exception as e:
        print(f"Failed to write metadata files: {e}")

if __name__ == "__main__":
    main()
