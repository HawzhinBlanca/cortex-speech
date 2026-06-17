import os
import sys
import json
import wave
import glob

# Reconfigure stdout for proper UTF-8 output
sys.stdout.reconfigure(encoding='utf-8')

DATASETS_DIR = r"C:\Users\hawzh\Desktop\CORTEX\kurdish_datasets"

def main():
    print("====================================================")
    print("CODEX Dataset Quality Assurance & Validation Utility")
    print(f"Target Directory: {DATASETS_DIR}")
    print("====================================================")

    if not os.path.exists(DATASETS_DIR):
        print(f"Error: Datasets directory does not exist at {DATASETS_DIR}")
        return

    subfolders = [f for f in os.listdir(DATASETS_DIR) if os.path.isdir(os.path.join(DATASETS_DIR, f))]
    print(f"Found {len(subfolders)} dataset subfolders.")

    total_slices = 0
    total_duration_sec = 0.0
    failed_files = []
    sample_rate_mismatches = 0
    channels_mismatches = 0

    all_characters = set()

    for idx, folder in enumerate(subfolders):
        folder_path = os.path.join(DATASETS_DIR, folder)
        metadata_csv = os.path.join(folder_path, "metadata.csv")
        manifest_json = os.path.join(folder_path, "manifest.json")
        wavs_dir = os.path.join(folder_path, "wavs")

        if not os.path.exists(metadata_csv):
            print(f"[MISSING METADATA.CSV] {folder}")
            failed_files.append(folder)
            continue

        if not os.path.exists(manifest_json):
            print(f"[MISSING MANIFEST.JSON] {folder}")
            failed_files.append(folder)
            continue

        # Load CSV metadata
        csv_records = {}
        try:
            with open(metadata_csv, "r", encoding="utf-8") as f:
                for line in f:
                    parts = line.strip().split('|')
                    if len(parts) == 2:
                        csv_records[parts[0]] = parts[1]
        except Exception as e:
            print(f"[ERROR READING CSV] {folder}: {e}")
            failed_files.append(folder)
            continue

        # Load JSON manifest
        try:
            with open(manifest_json, "r", encoding="utf-8") as f:
                manifest_data = json.load(f)
        except Exception as e:
            print(f"[ERROR READING JSON] {folder}: {e}")
            failed_files.append(folder)
            continue

        # Verify matching count
        if len(csv_records) != len(manifest_data):
            print(f"[COUNT MISMATCH] {folder}: CSV has {len(csv_records)}, JSON has {len(manifest_data)}")

        # Verify WAV files
        for record in manifest_data:
            wav_rel_path = record.get("wav_file")
            wav_full_path = os.path.join(folder_path, wav_rel_path)
            transcript = record.get("transcript", "")

            # Check character set
            for char in transcript:
                all_characters.add(char)

            if not os.path.exists(wav_full_path):
                print(f"[MISSING WAV] {folder}: {wav_rel_path}")
                failed_files.append(folder)
                continue

            # Verify WAV properties
            try:
                with wave.open(wav_full_path, "rb") as w:
                    channels = w.getnchannels()
                    rate = w.getframerate()
                    frames = w.getnframes()
                    duration = frames / float(rate)

                    if rate != 16000:
                        sample_rate_mismatches += 1
                    if channels != 1:
                        channels_mismatches += 1

                    total_duration_sec += duration
                    total_slices += 1
            except Exception as e:
                print(f"[CORRUPT WAV] {folder}: {wav_rel_path} - {e}")
                failed_files.append(folder)

    print("\n====================================================")
    print("VALDIATION SUMMARY")
    print("====================================================")
    print(f"Total Validated Subdatasets: {len(subfolders)}")
    print(f"Total Exported Speech Slices: {total_slices}")
    print(f"Total Dataset Duration: {total_duration_sec / 3600.0:.2f} hours ({total_duration_sec:.2f} seconds)")
    print(f"Average Slice Duration: {total_duration_sec / max(1, total_slices):.2f} seconds")
    print(f"Sample Rate Mismatches (expected 16kHz): {sample_rate_mismatches}")
    print(f"Channel Mismatches (expected mono): {channels_mismatches}")
    print(f"Failed Subdatasets / Folders: {len(failed_files)}")
    if failed_files:
        print(f"Details of failures: {list(set(failed_files))}")
    
    # Sort and print Kurdish alphabet characters detected
    kurdish_chars = sorted([c for c in all_characters if '\u0600' <= c <= '\u06FF' or '\u200C' <= c <= '\u200F'])
    print(f"Kurdish/Arabic Unicode Characters present in transcripts: {''.join(kurdish_chars)}")
    print("====================================================")

if __name__ == "__main__":
    main()
