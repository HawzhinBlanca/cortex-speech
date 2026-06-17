import os
import sys
import json
import base64
import asyncio
from openai import AsyncOpenAI

sys.stdout.reconfigure(encoding='utf-8')

DATASETS_DIR = r"C:\Users\CortexUser\Desktop\CORTEX\kurdish_datasets"
MANIFEST_PATH = os.path.join(DATASETS_DIR, "manifest.json")
VERIFIED_MANIFEST_PATH = os.path.join(DATASETS_DIR, "manifest_verified.json")
VERIFIED_CSV_PATH = os.path.join(DATASETS_DIR, "metadata_verified.csv")

# Concurrency limit (number of simultaneous requests to OpenRouter)
CONCURRENCY = 15

def load_env():
    env_path = r"C:\Users\CortexUser\Desktop\CORTEX\.env"
    if os.path.exists(env_path):
        with open(env_path, "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("OPENROUTER_API_KEY="):
                    os.environ["OPENROUTER_API_KEY"] = line.strip().split("=", 1)[1].strip('"').strip("'")
                    break

def encode_audio_base64(file_path):
    with open(file_path, "rb") as audio_file:
        return base64.b64encode(audio_file.read()).decode("utf-8")

async def process_segment(client, item, semaphore, system_prompt):
    seg_id = item["id"]
    wav_rel = item["wav_file"]
    wav_full = os.path.join(DATASETS_DIR, wav_rel)
    candidate_transcript = item["transcript"]

    if not os.path.exists(wav_full):
        print(f"[{seg_id}] ERROR: WAV file not found: {wav_full}")
        item["status"] = "error"
        item["error_reason"] = "WAV file missing"
        return item

    audio_base64 = encode_audio_base64(wav_full)

    async with semaphore:
        for attempt in range(5):
            try:
                response = await client.chat.completions.create(
                    model="google/gemini-2.5-flash",
                    messages=[
                        {
                            "role": "system",
                            "content": system_prompt
                        },
                        {
                            "role": "user",
                            "content": [
                                {
                                    "type": "input_audio",
                                    "input_audio": {
                                        "data": audio_base64,
                                        "format": "wav"
                                    }
                                },
                                {
                                    "type": "text",
                                    "text": f"Candidate transcription from local ASR: {candidate_transcript}\nPlease correct it."
                                }
                            ]
                        }
                    ],
                    extra_headers={
                        "HTTP-Referer": "http://localhost",
                        "X-Title": "Agentic Dataset Perfector"
                    }
                )

                verified_text = response.choices[0].message.content.strip()
                item["raw_llm_output"] = verified_text
                
                if "[QUARANTINE" in verified_text.upper() or "QUARANTINE:" in verified_text.upper():
                    item["verified_transcript"] = candidate_transcript
                    item["status"] = "quarantine"
                    item["quarantine_reason"] = verified_text
                    print(f"[{seg_id}] QUARANTINED")
                else:
                    item["verified_transcript"] = verified_text
                    item["status"] = "gold"
                    print(f"[{seg_id}] GOLD: {verified_text}")

                return item

            except Exception as e:
                wait_time = 2 ** attempt
                print(f"[{seg_id}] Attempt {attempt+1} failed: {e}. Retrying in {wait_time}s...")
                await asyncio.sleep(wait_time)

        item["status"] = "error"
        item["error_reason"] = "Failed after 5 retries"
        return item

def save_manifests(data):
    with open(VERIFIED_MANIFEST_PATH, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        
    with open(VERIFIED_CSV_PATH, "w", encoding="utf-8") as f:
        for item in data:
            if item.get("status") == "gold":
                clean_transcript = item.get("verified_transcript", "").replace("|", " ")
                f.write(f"{item['wav_file']}|{clean_transcript}\n")

async def main():
    print("====================================================")
    print("Agentic Dataset Perfector (High-Speed OpenRouter)")
    print("====================================================")

    load_env()
    api_key = os.environ.get("OPENROUTER_API_KEY")
    if not api_key:
        print("ERROR: OPENROUTER_API_KEY is not set in environment or C:\\Users\\hawzh\\Desktop\\CORTEX\\.env")
        sys.exit(1)

    if not os.path.exists(MANIFEST_PATH):
        print(f"ERROR: manifest.json not found at {MANIFEST_PATH}")
        sys.exit(1)

    client = AsyncOpenAI(
        base_url="https://openrouter.ai/api/v1",
        api_key=api_key,
    )

    with open(MANIFEST_PATH, "r", encoding="utf-8") as f:
        manifest_data = json.load(f)

    print(f"Loaded {len(manifest_data)} total segments from manifest.")
    
    verified_data = []
    processed_ids = set()
    if os.path.exists(VERIFIED_MANIFEST_PATH):
        try:
            with open(VERIFIED_MANIFEST_PATH, "r", encoding="utf-8") as f:
                verified_data = json.load(f)
            processed_ids = {item["id"] for item in verified_data}
            print(f"Resuming: {len(processed_ids)} segments already verified.")
        except Exception as e:
            print(f"Warning: Failed to load existing verified manifest: {e}")

    to_process = [item for item in manifest_data if item["id"] not in processed_ids]
    print(f"Remaining segments to process: {len(to_process)}")

    if len(to_process) == 0:
        print("All segments processed!")
        return

    system_prompt = (
        "You are a native Kurdish Sorani linguist and speech transcription expert. "
        "Your task is to review the audio clip and provide a 100% correct, orthographically standardized Kurdish Sorani transcription.\n\n"
        "Rules:\n"
        "1. Standardize Kurdish Kaf (ک) and Kurdish Yeh (ی). Do not use Arabic counterparts.\n"
        "2. Ensure proper suffix spacing (e.g. 'دا', 'وە', 'ی' attached properly or half-spaced).\n"
        "3. Output ONLY the corrected text. Do not add any introduction, notes, explanation, or punctuation markers outside the spoken words.\n"
        "4. If the audio is completely unintelligible, contains only music/noise, or is not in Kurdish Sorani, output '[QUARANTINE: <reason>]'."
    )

    semaphore = asyncio.Semaphore(CONCURRENCY)
    
    # Process in batches to save incrementally
    BATCH_SIZE = 50
    for i in range(0, len(to_process), BATCH_SIZE):
        batch = to_process[i:i+BATCH_SIZE]
        batch_tasks = [process_segment(client, item, semaphore, system_prompt) for item in batch]
        
        # Wait for batch to complete
        results = await asyncio.gather(*batch_tasks)
        verified_data.extend(results)
        
        save_manifests(verified_data)
        print(f"--- Saved progress: {len(verified_data)} / {len(manifest_data)} ---")

    print("\n====================================================")
    print("Verification complete!")
    print(f"Total entries now verified: {len(verified_data)}")
    print("====================================================")

if __name__ == "__main__":
    asyncio.run(main())
