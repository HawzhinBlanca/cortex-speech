import os
import sys
import time
import re
import json
import uuid
import subprocess
import sqlite3
import numpy as np
import wave
import tempfile

# Reconfigure stdout for proper UTF-8 output
sys.stdout.reconfigure(encoding='utf-8')

_HERE = os.path.dirname(os.path.abspath(__file__))   # == the CORTEX repo root
CORTEX_DIR = _HERE
TARGET_DIR = os.environ.get("CORTEX_AUDIO_DIR", "")
LOG_FILE = os.path.join(CORTEX_DIR, "ingest_folder.log")

SHERPA_EXE = os.path.join(CORTEX_DIR, "sherpa-onnx-v1.13.2-win-x64-shared-MD-Release", "bin", "sherpa-onnx-vad-with-offline-asr.exe")
VAD_MODEL = os.path.join(CORTEX_DIR, "cortex-speech-app", "src-tauri", "models", "silero_vad_v4.onnx")
TOKENS = os.path.join(CORTEX_DIR, "cortex-speech-app", "src-tauri", "models", "omniasr-ctc-300m", "tokens.txt")
ASR_MODEL = os.path.join(CORTEX_DIR, "cortex-speech-app", "src-tauri", "models", "omniasr-ctc-300m", "model.int8.onnx")

appdata = os.environ.get("APPDATA")
DB_PATH = os.path.join(appdata, "cortex-speech", "cortex-speech.db") if appdata else os.path.expanduser("~/AppData/Roaming/cortex-speech/cortex-speech.db")

CHUNK_DURATION_SEC = 180 # Split large audio files into 3-minute chunks for fast, memory-safe processing

def log(message):
    timestamp = time.strftime("%Y-%m-%d %H:%M:%S")
    log_line = f"[{timestamp}] {message}"
    print(log_line)
    try:
        with open(LOG_FILE, "a", encoding="utf-8") as f:
            f.write(log_line + "\n")
    except:
        pass

def normalize_ckb(text):
    if not text:
        return ""
    # Standardize Kaf variants
    text = re.sub(r'[\u0643\u06AA\u06AC]', '\u06a9', text)
    # Standardize Yeh variants
    text = re.sub(r'[\u064A\u06D2]', '\u06cc', text)
    # Alef Maksura -> Yeh
    text = text.replace('\u0649', '\u06cc')
    # Remove Tatweel
    text = text.replace('\u0640', '')
    # Remove Arabic diacritics
    text = re.sub(r'[\u064B-\u065F\u0670]', '', text)
    
    # Premium Curation / Spelling Corrections
    text = text.replace("ئێسا", "ئێستا")
    text = text.replace("ئەزبوونیان", "ئەزموونیان")
    text = text.replace("خێنیتەو", "بخوێنیتەوە")
    text = text.replace("هبو", "هەبوو")
    
    # Remove ZWNJ and collapse spaces
    text = text.replace("\u200c", " ")
    text = re.sub(r'\s+', ' ', text).strip()
    return text

def is_gibberish(text):
    if re.search(r'[a-zA-ZяЯ]', text):
        return True
    if re.search(r'(.)\1{3,}', text):
        return True
    if "ساراکا" in text:
        return True
    return False

def convert_to_wav_16k_mono(input_path):
    """
    Decodes any audio file (mp3, m4a, flac, etc.) to 16kHz mono WAV using system FFmpeg.
    Returns the absolute path to the temp WAV file.
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
    
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=180)
        if res.returncode != 0:
            raise RuntimeError(f"FFmpeg conversion failed: {res.stderr}")
        return temp_wav_path
    except Exception as e:
        if os.path.exists(temp_wav_path):
            os.remove(temp_wav_path)
        raise e

def read_wav_mono_16k(wav_path):
    with wave.open(wav_path, 'rb') as w:
        channels = w.getnchannels()
        rate = w.getframerate()
        frames = w.readframes(w.getnframes())
        samples = np.frombuffer(frames, dtype=np.int16)
        return samples

def process_audio(audio_path):
    log(f"Processing audio file: {audio_path}")
    
    temp_wav_path = None
    try:
        # Convert MP3/WAV/etc. to standard 16k mono WAV using FFmpeg
        temp_wav_path = convert_to_wav_16k_mono(audio_path)
        pcm16 = read_wav_mono_16k(temp_wav_path)
    except Exception as e:
        log(f"Failed to decode and resample {audio_path} via FFmpeg: {e}")
        return
    finally:
        if temp_wav_path and os.path.exists(temp_wav_path):
            try:
                os.remove(temp_wav_path)
            except:
                pass
        
    duration_sec = len(pcm16) / 16000.0
    log(f"Decoded successfully. Duration: {duration_sec:.2f} seconds")
    
    # Chunk the PCM array
    samples_per_chunk = CHUNK_DURATION_SEC * 16000
    num_chunks = int(np.ceil(len(pcm16) / samples_per_chunk))
    
    all_segments = []
    
    for chunk_idx in range(num_chunks):
        start_sample = chunk_idx * samples_per_chunk
        end_sample = min(start_sample + samples_per_chunk, len(pcm16))
        chunk_pcm = pcm16[start_sample:end_sample]
        
        if len(chunk_pcm) < 16000 * 0.5: # Skip tiny fragments under 0.5s
            continue
            
        log(f"Processing Chunk {chunk_idx + 1}/{num_chunks} ({len(chunk_pcm)/16000.0:.1f}s)...")
        
        # Write to temporary file
        with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as temp_wav:
            temp_path = temp_wav.name
            
        try:
            with wave.open(temp_path, 'wb') as w:
                w.setnchannels(1)
                w.setsampwidth(2)
                w.setframerate(16000)
                w.writeframes(chunk_pcm.tobytes())
                
            cmd = [
                SHERPA_EXE,
                f"--silero-vad-model={VAD_MODEL}",
                f"--tokens={TOKENS}",
                f"--omnilingual-asr-model={ASR_MODEL}",
                "--num-threads=4",
                temp_path
            ]
            
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=90, encoding="utf-8", errors="ignore")
            if res.returncode != 0:
                log(f"  -> ASR execution failed for chunk {chunk_idx + 1}")
                continue
                
            stdout = res.stdout
        except Exception as e:
            log(f"  -> Error executing ASR on chunk {chunk_idx + 1}: {e}")
            continue
        finally:
            if os.path.exists(temp_path):
                os.remove(temp_path)
                
        # Parse timestamps and offset them back to original audio coordinates
        pattern = re.compile(r'(\d+\.\d+) -- (\d+\.\d+): (.*)')
        chunk_offset_sec = chunk_idx * CHUNK_DURATION_SEC
        
        for line in stdout.splitlines():
            match = pattern.search(line)
            if match:
                start_sec = float(match.group(1)) + chunk_offset_sec
                end_sec = float(match.group(2)) + chunk_offset_sec
                text = match.group(3).strip()
                
                if not text or is_gibberish(text):
                    continue
                    
                norm_text = normalize_ckb(text)
                if not norm_text:
                    continue
                    
                seg = {
                    "id": str(uuid.uuid4())[:8],
                    "audio_path": audio_path,
                    "raw_transcript": text,
                    "normalized_transcript": norm_text,
                    "annotated_transcript": norm_text,
                    "duration_ms": int((end_sec - start_sec) * 1000),
                    "alignment_json": json.dumps({
                        "source_start_ms": int(start_sec * 1000),
                        "source_end_ms": int(end_sec * 1000)
                    })
                }
                all_segments.append(seg)
                
    if not all_segments:
        log("No clean Kurdish speech segments detected in this audio.")
        return
        
    log(f"Injecting {len(all_segments)} clean segments into production database...")
    
    try:
        conn = sqlite3.connect(DB_PATH)
        conn.execute("PRAGMA busy_timeout=5000;")
        conn.execute("PRAGMA journal_mode=WAL;")
        for s in all_segments:
            conn.execute('''
            INSERT INTO speech_segments (
                id, audio_path, raw_transcript, normalized_transcript, 
                annotated_transcript, alignment_json, duration_ms, verified
            ) VALUES (?,?,?,?,?,?,?,0)
            ON CONFLICT(id) DO UPDATE SET
                audio_path=excluded.audio_path,
                raw_transcript=excluded.raw_transcript,
                normalized_transcript=excluded.normalized_transcript,
                annotated_transcript=excluded.annotated_transcript,
                alignment_json=excluded.alignment_json,
                duration_ms=excluded.duration_ms
            ''', (
                s['id'], s['audio_path'], s['raw_transcript'], 
                s['normalized_transcript'], s['annotated_transcript'], 
                s['alignment_json'], s['duration_ms']
            ))
        conn.commit()
        conn.close()
        log(f"Successfully committed segments for file: {audio_path}")
    except Exception as e:
        log(f"Database injection error: {e}")

def get_all_audio_files(base_dir):
    audio_files = []
    for root, dirs, files in os.walk(base_dir):
        for file in files:
            if file.lower().endswith(('.wav', '.mp3', '.flac', '.m4a', '.ogg', '.wma', '.aac')):
                audio_files.append(os.path.join(root, file))
    return audio_files

def main():
    log("====================================================")
    log("CODEX Recursive Ingestion Pipeline Started")
    log(f"Target Directory: {TARGET_DIR}")
    log(f"Target Database: {DB_PATH}")
    log("====================================================")
    
    if not TARGET_DIR:
        sys.exit("set CORTEX_AUDIO_DIR to the folder of audio files to ingest")
    if not os.path.exists(TARGET_DIR):
        log(f"Error: Target directory does not exist: {TARGET_DIR}")
        return
        
    audio_files = get_all_audio_files(TARGET_DIR)
    log(f"Found {len(audio_files)} audio files in the target directory and its subfolders.")
    
    processed_files = set()
    try:
        conn = sqlite3.connect(DB_PATH)
        cursor = conn.execute("SELECT DISTINCT audio_path FROM speech_segments")
        for row in cursor.fetchall():
            processed_files.add(row[0])
        conn.close()
        log(f"Loaded {len(processed_files)} previously processed audio files from DB.")
    except Exception as e:
        log(f"Could not read DB history: {e}")
        
    files_to_process = [f for f in audio_files if f not in processed_files]
    log(f"{len(files_to_process)} files need to be processed.")
    
    for idx, file_path in enumerate(files_to_process):
        log(f"\n--- Progress: File {idx + 1}/{len(files_to_process)} ---")
        try:
            process_audio(file_path)
        except Exception as e:
            log(f"Failed to process file {file_path}: {e}")
            
    log("\n====================================================")
    log("CODEX Recursive Ingest Pipeline Complete!")
    log("====================================================")

if __name__ == "__main__":
    main()
