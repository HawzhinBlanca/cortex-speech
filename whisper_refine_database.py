import sqlite3
import os
import sys
import json
import subprocess
import tempfile
import soundfile as sf
import numpy as np
from transformers import pipeline
import re

sys.stdout.reconfigure(encoding='utf-8')

_HERE = os.path.dirname(os.path.abspath(__file__))   # == the CORTEX repo root

appdata = os.environ.get("APPDATA")
DB_PATH = os.path.join(appdata, "cortex-speech", "cortex-speech.db") if appdata else os.path.expanduser("~/AppData/Roaming/cortex-speech/cortex-speech.db")
CORTEX_DIR = _HERE

# Fine-tuned Kurdish Whisper model on Hugging Face
MODEL_NAME = "razhan/whisper-base-ckb"

def normalize_ckb(text):
    if not text:
        return ""
    # Standardize Kaf variants: Arabic Kaf -> Kurdish Kaf
    text = re.sub(r'[\u0643\u06AA\u06AC]', '\u06a9', text)
    # Standardize Yeh variants: Arabic Yah -> Kurdish Yah (Sorani)
    text = re.sub(r'[\u064A\u06D2]', '\u06cc', text)
    # Alef Maksura -> Yah
    text = text.replace('\u0649', '\u06cc')
    # Remove Tatweel
    text = text.replace('\u0640', '')
    # Remove Arabic diacritics
    text = re.sub(r'[\u064B-\u065F\u0670]', '', text)
    # Collapse spaces
    text = re.sub(r'\s+', ' ', text).strip()
    return text

def convert_to_wav_16k_mono(input_path):
    """
    Decodes any audio file to 16kHz mono WAV using ffmpeg.
    """
    temp_wav = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    temp_wav_path = temp_wav.name
    temp_wav.close()
    
    cmd = [
        "ffmpeg", "-y",
        "-i", input_path,
        "-ac", "1",
        "-ar", "16000",
        "-f", "wav",
        temp_wav_path
    ]
    
    res = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if res.returncode != 0:
        if os.path.exists(temp_wav_path):
            os.remove(temp_wav_path)
        raise RuntimeError(f"FFmpeg conversion failed for {input_path}")
    return temp_wav_path

import argparse

def main():
    parser = argparse.ArgumentParser(description="CODEX Kurdish Whisper Refinement Pipeline")
    parser.add_argument("--limit-files", type=int, default=None, help="Limit number of audio files to process")
    parser.add_argument("--limit-segments", type=int, default=None, help="Limit number of segments per audio file")
    parser.add_argument("--dry-run", action="store_true", help="Run transcription but do not commit to database")
    parser.add_argument("--test-one", action="store_true", help="Process exactly one segment as a test, print result, and exit")
    args = parser.parse_args()

    print("====================================================")
    print("CODEX Kurdish Whisper Refinement Pipeline")
    print(f"Target Database: {DB_PATH}")
    print(f"Using Hugging Face Model: {MODEL_NAME}")
    if args.dry_run:
        print("!!! DRY-RUN MODE: Database changes will not be committed !!!")
    if args.test_one:
        print("!!! TEST MODE: Will only process 1 segment and exit without committing !!!")
    print("====================================================")

    if not os.path.exists(DB_PATH):
        print(f"Error: Database not found at {DB_PATH}")
        return

    # 1. Initialize Hugging Face ASR Pipeline
    print("Loading Hugging Face model (downloading weights on first run)...")
    try:
        pipe = pipeline(
            "automatic-speech-recognition",
            model=MODEL_NAME,
            device="cpu",
            ignore_warning=True
        )
        print("Kurdish Whisper pipeline loaded successfully!")
    except Exception as e:
        print(f"Failed to load pipeline: {e}")
        return

    # 2. Query speech segments matching target audio directory
    try:
        conn = sqlite3.connect(DB_PATH)
        cursor = conn.cursor()
        cursor.execute(
            "SELECT id, audio_path, alignment_json, raw_transcript FROM speech_segments WHERE audio_path LIKE ?",
            ('%CORTEX_AUDIO_LIKE%',)
        )
        rows = cursor.fetchall()
        print(f"Found {len(rows)} segments to refine in database.")
    except Exception as e:
        print(f"Database query failed: {e}")
        return

    if not rows:
        print("No segments found to process.")
        conn.close()
        return

    # Group segments by original audiobook file
    from collections import defaultdict
    segments_by_audio = defaultdict(list)
    for row in rows:
        seg_id, audio_path, align_json, raw = row
        segments_by_audio[audio_path].append((seg_id, align_json, raw))

    # Apply limit on files if specified
    audio_paths = sorted(list(segments_by_audio.keys()))
    if args.test_one:
        audio_paths = audio_paths[:1]
    elif args.limit_files is not None:
        audio_paths = audio_paths[:args.limit_files]

    total_files = len(audio_paths)
    print(f"Grouped segments into {len(segments_by_audio)} master audio files. Processing {total_files} of them.")

    processed_count = 0
    updated_count = 0

    for idx, audio_path in enumerate(audio_paths):
        segs = segments_by_audio[audio_path]
        print(f"\n--- Progress: File {idx+1}/{total_files}: {os.path.basename(audio_path)} ---")
        print(f"Processing {len(segs)} segments...")

        if not os.path.exists(audio_path):
            print(f"Warning: Audio source not found: {audio_path}. Skipping.")
            continue

        # Decode master file once to standard 16k WAV
        try:
            temp_wav_path = convert_to_wav_16k_mono(audio_path)
            audio_data, sr = sf.read(temp_wav_path, dtype='float32')
            os.remove(temp_wav_path)
        except Exception as e:
            print(f"Failed to decode or read {audio_path}: {e}")
            continue

        # Apply limit on segments if specified
        if args.test_one:
            segs = segs[:1]
        elif args.limit_segments is not None:
            segs = segs[:args.limit_segments]

        # Transcribe each segment using mapped database limits
        for seg_idx, (seg_id, align_json, raw) in enumerate(segs):
            processed_count += 1
            if processed_count % 20 == 0 or seg_idx == 0 or seg_idx == len(segs) - 1:
                print(f"  -> Segment {seg_idx+1}/{len(segs)} (Global: {processed_count}/{len(rows)})")

            try:
                align_data = json.loads(align_json)
                start_ms = align_data.get("source_start_ms")
                end_ms = align_data.get("source_end_ms")
                if start_ms is None or end_ms is None:
                    continue
            except Exception as e:
                continue

            # Load slice in memory (16 samples per millisecond at 16,000 Hz)
            start_idx = int(start_ms * 16)
            end_idx = int(end_ms * 16)
            segment_audio = audio_data[start_idx:end_idx]

            if len(segment_audio) < 16000 * 0.1:
                continue

            try:
                # Transcribe sliced segment using HF Pipeline
                res = pipe({"raw": segment_audio, "sampling_rate": 16000})
                whisper_text = res.get("text", "").strip()

                # Clean transcription
                clean_text = normalize_ckb(whisper_text)

                print(f"    [Segment {seg_id}]")
                print(f"      Original ASR : {raw}")
                print(f"      Whisper ASR  : {whisper_text}")
                print(f"      Normalized   : {clean_text}")

                if clean_text and not args.dry_run and not args.test_one:
                    cursor.execute(
                        "UPDATE speech_segments SET raw_transcript = ?, normalized_transcript = ?, annotated_transcript = ?, updated_at = datetime('now') WHERE id = ?",
                        (clean_text, clean_text, clean_text, seg_id)
                    )
                    updated_count += 1
            except Exception as e:
                print(f"    * Error on segment {seg_id}: {e}")

        # Commit changes per audiobook (unless dry run)
        if not args.dry_run and not args.test_one:
            conn.commit()

        if args.test_one:
            break

    conn.close()
    print(f"\n====================================================")
    print(f"Whisper Database Refinement Complete!")
    print(f"Total Slices Processed: {processed_count}")
    print(f"Total Transcripts Refined: {updated_count}")
    print(f"====================================================")

    # 3. Clean Rebuild of Datasets (only if not a test or dry-run)
    if not args.dry_run and not args.test_one:
        print("\nTriggering clean rebuild of Gold and Quarantine datasets...")
        rebuild_path = os.path.join(CORTEX_DIR, "rebuild_datasets.py")
        if os.path.exists(rebuild_path):
            res = subprocess.run(["python", rebuild_path], capture_output=True, text=True, encoding="utf-8")
            print(res.stdout)
        else:
            print("Error: rebuild_datasets.py not found in CORTEX directory.")
    else:
        print("\nSkipping dataset rebuild (dry-run or test mode).")

if __name__ == "__main__":
    main()
