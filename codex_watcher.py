import os
import sys
import time
import re
import json
import uuid
import struct
import subprocess
import sqlite3
import numpy as np
import wave
import tempfile

# Reconfigure stdout for proper UTF-8 output
sys.stdout.reconfigure(encoding='utf-8')

WATCH_DIR = r"C:\Users\CortexUser\Desktop\Lamo Voice Samples"
CORTEX_DIR = r"C:\Users\CortexUser\Desktop\CORTEX"
LOG_FILE = os.path.join(CORTEX_DIR, "codex_watcher.log")

SHERPA_EXE = os.path.join(CORTEX_DIR, r"sherpa-onnx-v1.13.2-win-x64-shared-MD-Release\bin\sherpa-onnx-vad-with-offline-asr.exe")
VAD_MODEL = os.path.join(CORTEX_DIR, r"cortex-speech-app\src-tauri\models\silero_vad_v4.onnx")
TOKENS = os.path.join(CORTEX_DIR, r"cortex-speech-app\src-tauri\models\omniasr-ctc-300m\tokens.txt")
ASR_MODEL = os.path.join(CORTEX_DIR, r"cortex-speech-app\src-tauri\models\omniasr-ctc-300m\model.int8.onnx")

appdata = os.environ.get("APPDATA")
DB_PATH = os.path.join(appdata, "cortex-speech", "cortex-speech.db") if appdata else os.path.join(CORTEX_DIR, "cortex-speech.db")

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

def read_wav_robust(file_path):
    """
    Robustly reads standard PCM, IEEE Float, 24-bit PCM, and BWF WAV files.
    Returns: (pcm_int16_array, channels, sample_rate)
    """
    with open(file_path, 'rb') as f:
        header = f.read(12)
        if len(header) < 12 or header[0:4] != b'RIFF' or header[8:12] != b'WAVE':
            raise ValueError("Not a standard RIFF/WAVE file")
            
        audio_format = 1
        channels = 1
        sample_rate = 16000
        bits_per_sample = 16
        data_bytes = None
        
        while True:
            chunk_header = f.read(8)
            if len(chunk_header) < 8:
                break
            chunk_id, chunk_size = struct.unpack('<4sI', chunk_header)
            
            # Ensure chunk_size is even (WAV chunks are padded to even sizes on disk)
            padded_size = chunk_size + (chunk_size % 2)
            
            if chunk_id == b'fmt ':
                fmt_data = f.read(chunk_size)
                audio_format, channels, sample_rate, _, _, bits_per_sample = struct.unpack('<HHIIHH', fmt_data[:16])
                # Handle WAVEFORMATEXTENSIBLE (tag 65534)
                if audio_format == 65534 and len(fmt_data) >= 26:
                    sub_format = struct.unpack('<H', fmt_data[24:26])[0]
                    audio_format = sub_format
                
                # Skip remaining fmt bytes if any
                if len(fmt_data) < padded_size:
                    f.seek(padded_size - len(fmt_data), 1)
            elif chunk_id == b'data':
                data_bytes = f.read(chunk_size)
                # Skip padding if any
                if len(data_bytes) < padded_size:
                    f.seek(padded_size - len(data_bytes), 1)
                break
            else:
                f.seek(padded_size, 1)
                
        if data_bytes is None:
            raise ValueError("Data chunk not found in WAV file")
            
        # Parse samples based on format
        if audio_format == 1: # PCM Integer
            if bits_per_sample == 16:
                samples = np.frombuffer(data_bytes, dtype=np.int16)
            elif bits_per_sample == 24:
                raw_bytes = np.frombuffer(data_bytes, dtype=np.uint8)
                num_samples = len(raw_bytes) // 3
                raw_bytes = raw_bytes[:num_samples * 3]
                
                b0 = raw_bytes[0::3].astype(np.int32)
                b1 = raw_bytes[1::3].astype(np.int32)
                b2 = raw_bytes[2::3].astype(np.int32)
                
                samples_32 = b0 | (b1 << 8) | (b2 << 16)
                samples_32 = np.where(samples_32 >= 0x800000, samples_32 - 0x1000000, samples_32)
                samples = (samples_32 // 256).astype(np.int16)
            elif bits_per_sample == 32:
                samples_32 = np.frombuffer(data_bytes, dtype=np.int32)
                samples = (samples_32 // 65536).astype(np.int16)
            else:
                raise ValueError(f"Unsupported integer bits-per-sample: {bits_per_sample}")
        elif audio_format == 3: # IEEE Float
            if bits_per_sample == 32:
                samples_float = np.frombuffer(data_bytes, dtype=np.float32)
                samples = (np.clip(samples_float, -1.0, 1.0) * 32767.0).astype(np.int16)
            else:
                raise ValueError(f"Unsupported float bits-per-sample: {bits_per_sample}")
        else:
            raise ValueError(f"Unsupported WAV audio format: {audio_format}")
            
        return samples, channels, sample_rate

def resample_numpy(samples, from_rate, to_rate):
    if from_rate == to_rate or len(samples) == 0:
        return samples
    ratio = to_rate / from_rate
    new_len = int(len(samples) * ratio)
    old_indices = np.arange(len(samples))
    new_indices = np.arange(new_len) / ratio
    return np.interp(new_indices, old_indices, samples).astype(np.int16)

def downmix_to_mono(samples, channels):
    if channels == 1 or len(samples) == 0:
        return samples
    return samples.reshape(-1, channels).mean(axis=1).astype(np.int16)

def process_audio(audio_path):
    log(f"Processing new audio file: {audio_path}")
    
    try:
        raw_samples, channels, sample_rate = read_wav_robust(audio_path)
    except Exception as e:
        log(f"Failed to parse WAV file {audio_path}: {e}")
        return
        
    log(f"WAV Details - Channels: {channels}, Rate: {sample_rate} Hz, Size: {len(raw_samples)} samples")
    
    # 1. Downmix to mono if multi-channel
    mono_samples = downmix_to_mono(raw_samples, channels)
    
    # 2. Resample to 16000 Hz target
    pcm16 = resample_numpy(mono_samples, sample_rate, 16000)
    log(f"Resampled to 16kHz. Duration: {len(pcm16)/16000.0:.2f} seconds")
    
    # 3. Chunk the PCM array
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
        
        # Write to temporary file using tempfile.NamedTemporaryFile
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
            
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=60, encoding="utf-8", errors="ignore")
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

def main():
    if not os.path.exists(WATCH_DIR):
        log(f"Creating watch directory: {WATCH_DIR}")
        os.makedirs(WATCH_DIR, exist_ok=True)
        
    log("====================================================")
    log("CODEX Ultra-Robust Autonomous Kurdish Curator Started")
    log(f"Watching directory: {WATCH_DIR}")
    log(f"Target Database: {DB_PATH}")
    log("====================================================")
    
    processed_files = set()
    
    # Load already processed files from DB if possible
    try:
        conn = sqlite3.connect(DB_PATH)
        cursor = conn.execute("SELECT DISTINCT audio_path FROM speech_segments")
        for row in cursor.fetchall():
            processed_files.add(row[0])
        conn.close()
        log(f"Loaded {len(processed_files)} previously processed audio files from DB.")
    except Exception as e:
        log(f"Could not read DB history: {e}")
        
    while True:
        try:
            current_files = {
                os.path.join(WATCH_DIR, f) 
                for f in os.listdir(WATCH_DIR) 
                if f.lower().endswith(('.wav', '.mp3', '.flac', '.m4a'))
            }
        except Exception as e:
            log(f"Directory read error: {e}")
            time.sleep(5)
            continue
            
        new_files = current_files - processed_files
        
        for file_path in new_files:
            # Sleep briefly to ensure file copy is fully complete
            time.sleep(2)
            try:
                process_audio(file_path)
                processed_files.add(file_path)
            except Exception as e:
                log(f"Error handling {file_path}: {e}")
                
        time.sleep(4)

if __name__ == "__main__":
    main()
