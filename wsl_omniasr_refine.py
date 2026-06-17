import sqlite3
import os
import json
import sys
import soundfile as sf
import tempfile
import subprocess
import re
import shutil

# Paths inside WSL
DB_PATH = "/mnt/c/Users/hawzh/AppData/Roaming/cortex-speech/cortex-speech.db"
CORTEX_DIR = "/mnt/c/Users/hawzh/Desktop/CORTEX"

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

def to_wsl_path(win_path):
    if not win_path:
        return ""
    # Strip Win32 long-path prefix if present
    if win_path.startswith('\\\\?\\'):
        win_path = win_path[4:]
    # Replace backslashes with forward slashes
    p = win_path.replace('\\', '/')
    # Replace drive letter (e.g. C:) with mount point (e.g. /mnt/c)
    if p.startswith('C:/') or p.startswith('c:/'):
        p = '/mnt/c/' + p[3:]
    
    # Correct path redirection for Lamo Voice Samples if it contains the old Desktop path
    if "Desktop/Lamo Voice Samples" in p:
        p = p.replace("Desktop/Lamo Voice Samples", "Desktop/CortexAudio/Lamo Voice Samples")
        
    return p

def get_db_connection(db_path):
    if db_path.startswith("/mnt/"):
        pid = os.getpid()
        local_db_path = f"/tmp/cortex-speech-{pid}.db"
        for suffix in ["", "-wal", "-shm"]:
            src = db_path + suffix
            dst = local_db_path + suffix
            if os.path.exists(src):
                try:
                    shutil.copy2(src, dst)
                except Exception as e:
                    print(f"Warning: Failed to copy {src} to {dst}: {e}")
            else:
                if os.path.exists(dst):
                    try:
                        os.remove(dst)
                    except Exception:
                        pass
        conn = sqlite3.connect(local_db_path)
    else:
        conn = sqlite3.connect(db_path)
        local_db_path = db_path
    # Enable WAL mode and busy timeout for concurrent access with the Windows app.
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA busy_timeout=10000")
    conn.execute("PRAGMA synchronous=NORMAL")
    return conn, local_db_path

def close_db_connection(conn, db_path, local_db_path, copy_back=True):
    conn.close()
    if db_path.startswith("/mnt/"):
        if copy_back:
            # Copy back to Windows mount path
            for suffix in ["", "-wal", "-shm"]:
                src = local_db_path + suffix
                dst = db_path + suffix
                if os.path.exists(src):
                    try:
                        shutil.copy2(src, dst)
                    except Exception as e:
                        print(f"Warning: Failed to copy back {src} to {dst}: {e}")
                else:
                    if os.path.exists(dst):
                        try:
                            os.remove(dst)
                        except Exception as e:
                            print(f"Warning: Failed to remove stale {dst}: {e}")
        # Clean up temp files from /tmp
        for suffix in ["", "-wal", "-shm"]:
            temp_file = local_db_path + suffix
            if os.path.exists(temp_file):
                try:
                    os.remove(temp_file)
                except Exception:
                    pass

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
    parser = argparse.ArgumentParser(description="WSL CODEX Meta OmniASR 7B v2 Refinement Pipeline")
    parser.add_argument("--limit-files", type=int, default=None, help="Limit number of audio files to process")
    parser.add_argument("--limit-segments", type=int, default=None, help="Limit number of segments per audio file")
    parser.add_argument("--dry-run", action="store_true", help="Run transcription but do not commit to database")
    parser.add_argument("--test-one", action="store_true", help="Process exactly one segment as a test, print result, and exit")
    parser.add_argument("--segment-id", type=str, default=None, help="Process a specific segment ID and write to database")
    parser.add_argument("--stdout-only", action="store_true", help="Do not write updates to DB, output result to stdout as JSON")
    args = parser.parse_args()

    print("====================================================")
    print("WSL CODEX Meta OmniASR 7B v2 Refinement Pipeline")
    print(f"Target Database: {DB_PATH}")
    if args.dry_run:
        print("!!! DRY-RUN MODE: Database changes will not be committed !!!")
    if args.test_one:
        print("!!! TEST MODE: Will only process 1 segment and exit without committing !!!")
    if args.segment_id:
        print(f"!!! SINGLE SEGMENT MODE: Processing segment {args.segment_id} !!!")
    print("====================================================")

    # 1. Initialize OmniASR 7B v2 Pipeline
    print("Loading omnilingual-asr (omniASR_LLM_7B_v2) on GPU...")
    try:
        from omnilingual_asr.models.inference.pipeline import ASRInferencePipeline
        # Loads model using GPU by default if CUDA is available
        pipeline = ASRInferencePipeline(model_card="omniASR_LLM_7B_v2")
        print("OmniASR 7B v2 loaded successfully!")
    except Exception as e:
        sys.stderr.write(f"Failed to load OmniASR pipeline: {e}\n")
        sys.exit(1)

    # 2. Query speech segments matching target audio directory
    if not os.path.exists(DB_PATH):
        sys.stderr.write(f"Error: Database not found at {DB_PATH}\n")
        sys.exit(1)

    conn = None
    local_db_path = None
    try:
        conn, local_db_path = get_db_connection(DB_PATH)
        cursor = conn.cursor()
        if args.segment_id:
            cursor.execute(
                "SELECT id, audio_path, alignment_json, raw_transcript FROM speech_segments "
                "WHERE id = ?",
                (args.segment_id,)
            )
        else:
            cursor.execute(
                "SELECT id, audio_path, alignment_json, raw_transcript FROM speech_segments "
                "WHERE (audio_path LIKE ? OR audio_path LIKE ?) "
                "AND (updated_at IS NULL OR updated_at < ?)",
                ('%CORTEX_AUDIO_LIKE%', '%Lamo Voice Samples%', '2026-06-04 23:26:00')
            )
        rows = cursor.fetchall()
        print(f"Found {len(rows)} segments to refine in database.")
    except Exception as e:
        sys.stderr.write(f"Database query failed: {e}\n")
        if conn:
            try:
                close_db_connection(conn, DB_PATH, local_db_path)
            except Exception:
                pass
        sys.exit(1)

    if not rows:
        print("No segments found to process.")
        if conn:
            try:
                close_db_connection(conn, DB_PATH, local_db_path)
            except Exception:
                pass
        return

    processed_count = 0
    updated_count = 0
    try:
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

        for idx, win_audio_path in enumerate(audio_paths):
            segs = segments_by_audio[win_audio_path]
            wsl_audio_path = to_wsl_path(win_audio_path)
            print(f"\n--- Progress: File {idx+1}/{total_files}: {os.path.basename(wsl_audio_path)} ---")
            print(f"Processing {len(segs)} segments...")

            if not os.path.exists(wsl_audio_path):
                print(f"Warning: Audio source not found: {wsl_audio_path}. Skipping.")
                continue

            # Decode master file once to standard 16k WAV
            try:
                temp_wav_path = convert_to_wav_16k_mono(wsl_audio_path)
                audio_data, sr = sf.read(temp_wav_path, dtype='float32')
                os.remove(temp_wav_path)
            except Exception as e:
                print(f"Failed to decode or read {wsl_audio_path}: {e}")
                continue

            # Apply limit on segments if specified
            if args.test_one:
                segs = segs[:1]
            elif args.limit_segments is not None:
                segs = segs[:args.limit_segments]

            # Prepare all slices for batch processing
            temp_files = []
            valid_segs = []

            for seg_idx, (seg_id, align_json, raw) in enumerate(segs):
                try:
                    align_data = json.loads(align_json)
                    start_ms = align_data.get("source_start_ms")
                    end_ms = align_data.get("source_end_ms")
                    if start_ms is None or end_ms is None:
                        continue
                except Exception:
                    continue

                # Load slice in memory (16 samples per millisecond at 16,000 Hz)
                start_idx = int(start_ms * 16)
                end_idx = int(end_ms * 16)
                segment_audio = audio_data[start_idx:end_idx]

                if len(segment_audio) < 16000 * 0.1:
                    continue

                temp_file = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
                temp_file_path = temp_file.name
                temp_file.close()

                sf.write(temp_file_path, segment_audio, 16000)
                temp_files.append(temp_file_path)
                valid_segs.append((seg_id, raw))

            if not temp_files:
                print("  No valid audio slices to process for this file.")
                continue

            # Run batch transcription
            print(f"  Transcribing {len(temp_files)} segments on GPU using OmniASR 7B v2...")
            try:
                # batch_size=16 is optimal for RTX 4090 (24GB VRAM)
                transcripts = pipeline.transcribe(
                    temp_files, 
                    lang=["ckb_Arab"] * len(temp_files), 
                    batch_size=16
                )

                # Update SQLite database inside an explicit transaction.
                # If the process is killed mid-batch only THIS audio file rolls back.
                for (seg_id, raw), txt in zip(valid_segs, transcripts):
                    clean_text = normalize_ckb(txt)
                    print(f"    [Segment {seg_id}]")
                    print(f"      Original ASR : {raw}")
                    print(f"      OmniASR Text : {txt}")
                    print(f"      Normalized   : {clean_text}")

                    if args.stdout_only:
                        result_payload = {
                            "segment_id": seg_id,
                            "raw_transcript": clean_text,
                            "confidence": 0.95
                        }
                        print(f"__RESULT__={json.dumps(result_payload)}")

                    if clean_text and not args.dry_run and not args.stdout_only and (args.segment_id or not args.test_one):
                        cursor.execute(
                            "UPDATE speech_segments SET raw_transcript = ?, normalized_transcript = ?, annotated_transcript = ?, confidence = ?, updated_at = datetime('now') WHERE id = ? AND (human_decision IS NULL OR human_decision = '')",
                            (clean_text, clean_text, clean_text, 0.95, seg_id)
                        )
                        updated_count += 1

                # Commit this audio file's updates atomically.
                if not args.dry_run and not args.stdout_only and (args.segment_id or not args.test_one):
                    conn.commit()
                    print(f"  Successfully processed and updated {len(temp_files)} segments in database.")
                else:
                    conn.rollback()
                    print(f"  Processed {len(temp_files)} segments. Database changes skipped.")


                processed_count += len(temp_files)

            except Exception as e:
                print(f"  * Batch ASR error: {e}")

            # Clean up temp WAV files
            for f in temp_files:
                if os.path.exists(f):
                    os.remove(f)

            if args.test_one:
                break
    finally:
        if conn:
            close_db_connection(conn, DB_PATH, local_db_path, copy_back=not args.stdout_only)
    print(f"\n====================================================")
    print(f"OmniASR 7B v2 Refinement Complete!")
    print(f"Total Slices Processed: {processed_count}")
    print(f"Total Transcripts Refined: {updated_count}")
    print(f"====================================================")

    # 3. Trigger Clean Rebuild of datasets (only if not a test or dry-run)
    if not args.dry_run and not args.test_one:
        print("\nTranscription complete. Rebuild datasets should be triggered.")

if __name__ == "__main__":
    main()
