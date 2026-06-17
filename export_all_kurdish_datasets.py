import os
import sys
import json
import sqlite3
import subprocess
import re

# Reconfigure stdout for proper UTF-8 output
sys.stdout.reconfigure(encoding='utf-8')

DB_PATH = os.path.join(os.environ['APPDATA'], "cortex-speech", "cortex-speech.db")
OUTPUT_BASE_DIR = r"C:\Users\CortexUser\Desktop\CORTEX\kurdish_datasets"

def clean_filename(name):
    # Remove extension
    base = os.path.splitext(name)[0]
    # Replace non-alphanumeric characters (except underscore/dash) with underscore
    clean = re.sub(r'[^\w\-]', '_', base)
    # Collapse multiple underscores
    clean = re.sub(r'_+', '_', clean).strip('_')
    return clean

def main():
    print("====================================================")
    print("CODEX Batch Dataset Exporter Started")
    print(f"Target Database: {DB_PATH}")
    print(f"Base Output Directory: {OUTPUT_BASE_DIR}")
    print("====================================================")

    # 1. Fetch all distinct audio paths matching the Kurdish folder
    try:
        conn = sqlite3.connect(DB_PATH)
        cursor = conn.execute(
            "SELECT DISTINCT audio_path FROM speech_segments WHERE audio_path LIKE ? OR audio_path LIKE ?",
            ('%CORTEX_AUDIO_LIKE%', '%Lamo Voice Samples%')
        )
        audio_paths = [r[0] for r in cursor.fetchall()]
        conn.close()
        print(f"Found {len(audio_paths)} distinct Kurdish audio files in database.")
    except Exception as e:
        print(f"Failed to read database: {e}")
        return

    if not audio_paths:
        print("No audio segments found to export.")
        return

    # 2. Process each audio file one by one
    for idx, audio_path in enumerate(audio_paths):
        filename = os.path.basename(audio_path)
        clean_folder = clean_filename(filename)
        output_dir = os.path.join(OUTPUT_BASE_DIR, clean_folder)
        wavs_dir = os.path.join(output_dir, "wavs")

        print(f"\n--- Processing File {idx + 1}/{len(audio_paths)}: {filename} ---")
        print(f"Target Folder: {clean_folder}")

        resolved_audio_path = audio_path
        if resolved_audio_path.startswith('\\\\?\\'):
            resolved_audio_path = resolved_audio_path[4:]
        if "Desktop\\Lamo Voice Samples" in resolved_audio_path:
            resolved_audio_path = resolved_audio_path.replace("Desktop\\Lamo Voice Samples", "Desktop\\Hawzhin\\Lamo Voice Samples")
        elif "Desktop/Lamo Voice Samples" in resolved_audio_path:
            resolved_audio_path = resolved_audio_path.replace("Desktop/Lamo Voice Samples", "Desktop/CortexAudio/Lamo Voice Samples")

        if not os.path.exists(resolved_audio_path):
            print(f"Warning: Original audio file not found at: {resolved_audio_path}. Skipping.")
            continue

        os.makedirs(wavs_dir, exist_ok=True)

        # Fetch segments for this file
        try:
            conn = sqlite3.connect(DB_PATH)
            cursor = conn.execute(
                "SELECT id, raw_transcript, normalized_transcript, annotated_transcript, alignment_json, duration_ms "
                "FROM speech_segments WHERE audio_path = ?",
                (audio_path,)
            )
            rows = cursor.fetchall()
            conn.close()
            print(f"Found {len(rows)} segments in database.")
        except Exception as e:
            print(f"Database error for {filename}: {e}")
            continue

        metadata = []
        skipped = 0

        # Slice each segment
        for row in rows:
            seg_id, raw, normalized, annotated, alignment_json, duration_ms = row
            
            transcript = annotated or normalized or raw
            if not transcript or not transcript.strip():
                skipped += 1
                continue

            try:
                align_data = json.loads(alignment_json)
                start_ms = align_data.get("source_start_ms")
                end_ms = align_data.get("source_end_ms")
                if start_ms is None or end_ms is None:
                    raise ValueError("Missing start/end timestamps")
            except:
                skipped += 1
                continue

            start_sec = start_ms / 1000.0
            end_sec = end_ms / 1000.0
            duration_sec = end_sec - start_sec

            if duration_sec <= 0:
                skipped += 1
                continue

            wav_filename = f"{clean_folder}_{seg_id}.wav"
            wav_path = os.path.join(wavs_dir, wav_filename)

            # Slice WAV using FFmpeg (Mono, 16kHz)
            cmd = [
                "ffmpeg", "-y",
                "-ss", f"{start_sec:.3f}",
                "-t", f"{duration_sec:.3f}",
                "-i", resolved_audio_path,
                "-ac", "1",
                "-ar", "16000",
                "-f", "wav",
                wav_path
            ]

            try:
                subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
                metadata.append({
                    "id": seg_id,
                    "wav_file": f"wavs/{wav_filename}",
                    "transcript": transcript,
                    "duration_ms": duration_ms
                })
            except Exception as e:
                skipped += 1

        # Write metadata for this file
        if metadata:
            metadata_csv_path = os.path.join(output_dir, "metadata.csv")
            metadata_json_path = os.path.join(output_dir, "manifest.json")
            try:
                with open(metadata_csv_path, "w", encoding="utf-8") as f:
                    for item in metadata:
                        clean_ts = item["transcript"].replace("|", " ")
                        f.write(f"{item['wav_file']}|{clean_ts}\n")
                
                with open(metadata_json_path, "w", encoding="utf-8") as f:
                    json.dump(metadata, f, ensure_ascii=False, indent=2)

                print(f"Success: Exported {len(metadata)} segments. Skipped {skipped}.")
            except Exception as e:
                print(f"Failed to write metadata for {filename}: {e}")
        else:
            print("No valid segments exported for this file.")

    print("\n====================================================")
    print("CODEX Batch Dataset Export Pipeline Complete!")
    print("====================================================")

if __name__ == "__main__":
    main()
