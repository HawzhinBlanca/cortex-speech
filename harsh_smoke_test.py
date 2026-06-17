import subprocess
import os
import wave
import struct
import sys

# Reconfigure stdout for proper UTF-8 output
sys.stdout.reconfigure(encoding='utf-8')

def create_corrupted_wav(path):
    with open(path, 'wb') as f:
        f.write(b'RIFF')
        f.write(b'\x00\x00\x00\x00') # Corrupted size
        f.write(b'WAVE')
        f.write(b'JUNK') # Corrupted chunk
        f.write(b'\x00\x00\x00\x00')

def create_zero_byte_wav(path):
    open(path, 'wb').close()

def create_silent_wav(path, duration_seconds=5, sample_rate=16000):
    with wave.open(path, 'w') as f:
        f.setnchannels(1)
        f.setsampwidth(2)
        f.setframerate(sample_rate)
        # Write pure silence (0s)
        for _ in range(sample_rate * duration_seconds):
            f.writeframes(struct.pack('<h', 0))

def run_ai_engine(audio_path):
    cmd = [
        r".\sherpa-onnx-v1.13.2-win-x64-shared-MD-Release\bin\sherpa-onnx-vad-with-offline-asr.exe",
        "--silero-vad-model=C:\\Users\\hawzh\\Desktop\\CORTEX\\cortex-speech-app\\src-tauri\\models\\silero_vad_v4.onnx",
        "--tokens=C:\\Users\\hawzh\\Desktop\\CORTEX\\cortex-speech-app\\src-tauri\\models\\omniasr-ctc-300m\\tokens.txt",
        "--omnilingual-asr-model=C:\\Users\\hawzh\\Desktop\\CORTEX\\cortex-speech-app\\src-tauri\\models\\omniasr-ctc-300m\\model.int8.onnx",
        "--num-threads=4",
        audio_path
    ]
    try:
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        return res.returncode, res.stdout, res.stderr
    except subprocess.TimeoutExpired:
        return -1, "", "TIMEOUT (HUNG)"

def test_scenario(name, audio_path, setup_fn):
    print(f"\n--- Running Harsh Test: {name} ---")
    setup_fn(audio_path)
    code, stdout, stderr = run_ai_engine(audio_path)
    
    if code == -1:
        print("❌ FAIL: Engine hung and had to be killed (Timeout).")
    elif code != 0:
        print(f"✅ PASS: Engine gracefully rejected bad input (Exit Code {code}).")
        # Print a snippet of the error to prove it caught it
        err_snippet = (stderr.strip()[:100] + '...') if stderr else (stdout.strip()[:100] + '...')
        print(f"   Reason captured: {err_snippet}")
    else:
        # If it exited 0, it better not have hallucinated transcriptions
        if "0.000 -- " in stdout:
            print("❌ FAIL: Engine hallucinated text on invalid/silent audio!")
            print(stdout)
        else:
            print("✅ PASS: Engine processed safely without hallucinations.")

if __name__ == "__main__":
    print("Initiating Harsh AI Reliability Smoke Test...")
    test_scenario("Corrupted Header WAV", "corrupted.wav", create_corrupted_wav)
    test_scenario("Zero-Byte Empty File", "empty.wav", create_zero_byte_wav)
    test_scenario("Pure Silence (5 seconds)", "silence.wav", create_silent_wav)
    
    print("\nSmoke test complete. Cleaning up...")
    for f in ["corrupted.wav", "empty.wav", "silence.wav"]:
        if os.path.exists(f):
            os.remove(f)
