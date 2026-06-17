import os
import sys
import json
import base64
import asyncio
import random
import difflib
from openai import AsyncOpenAI
import csv

sys.stdout.reconfigure(encoding='utf-8')

DATASETS_DIR = r"C:\Users\hawzh\Desktop\CORTEX\kurdish_datasets"
MANIFEST_PATH = os.path.join(DATASETS_DIR, "manifest.json")
REPORT_PATH = os.path.join(DATASETS_DIR, "brutal_reality_report.csv")

# Sample size for the brutal reality check
SAMPLE_SIZE = 100
CONCURRENCY = 10

def load_env():
    env_path = r"C:\Users\hawzh\Desktop\CORTEX\.env"
    if os.path.exists(env_path):
        with open(env_path, "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("OPENROUTER_API_KEY="):
                    os.environ["OPENROUTER_API_KEY"] = line.strip().split("=", 1)[1].strip('"').strip("'")
                    break

def encode_audio_base64(file_path):
    with open(file_path, "rb") as audio_file:
        return base64.b64encode(audio_file.read()).decode("utf-8")

def calculate_wer(reference, hypothesis):
    ref_words = reference.split()
    hyp_words = hypothesis.split()
    
    # Simple Levenshtein distance for words
    d = [[0] * (len(hyp_words) + 1) for _ in range(len(ref_words) + 1)]
    for i in range(len(ref_words) + 1):
        d[i][0] = i
    for j in range(len(hyp_words) + 1):
        d[0][j] = j
        
    for i in range(1, len(ref_words) + 1):
        for j in range(1, len(hyp_words) + 1):
            if ref_words[i - 1] == hyp_words[j - 1]:
                cost = 0
            else:
                cost = 1
            d[i][j] = min(
                d[i - 1][j] + 1,      # deletion
                d[i][j - 1] + 1,      # insertion
                d[i - 1][j - 1] + cost # substitution
            )
            
    if len(ref_words) == 0:
        return 1.0 if len(hyp_words) > 0 else 0.0
    return d[len(ref_words)][len(hyp_words)] / len(ref_words)

async def process_segment(client, item, semaphore, system_prompt):
    seg_id = item["id"]
    wav_rel = item["wav_file"]
    wav_full = os.path.join(DATASETS_DIR, wav_rel)
    candidate_transcript = item["transcript"]

    if not os.path.exists(wav_full):
        item["status"] = "error"
        item["error_reason"] = "WAV file missing"
        return item

    audio_base64 = encode_audio_base64(wav_full)

    async with semaphore:
        for attempt in range(3):
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
                                    "text": f"Evaluate this local ASR transcription: {candidate_transcript}"
                                }
                            ]
                        }
                    ],
                    temperature=0.0,
                    extra_headers={
                        "HTTP-Referer": "http://localhost",
                        "X-Title": "Brutal Reality Check"
                    }
                )

                verified_text = response.choices[0].message.content.strip()
                item["gemini_transcript"] = verified_text
                
                if "[QUARANTINE" in verified_text.upper() or "QUARANTINE:" in verified_text.upper():
                    item["status"] = "quarantine"
                    item["wer"] = 1.0
                else:
                    item["status"] = "gold"
                    wer = calculate_wer(verified_text, candidate_transcript)
                    item["wer"] = wer
                    
                print(f"[{seg_id}] ASR: {candidate_transcript}")
                print(f"[{seg_id}] GEM: {verified_text}")
                print(f"[{seg_id}] WER: {item.get('wer', 1.0):.2f}\n")

                return item

            except Exception as e:
                wait_time = 2 ** attempt
                print(f"[{seg_id}] Attempt {attempt+1} failed: {e}. Retrying in {wait_time}s...")
                await asyncio.sleep(wait_time)

        item["status"] = "error"
        item["error_reason"] = "Failed after 3 retries"
        return item

async def main():
    print("====================================================")
    print("Brutal Reality Check: Dataset Auditing")
    print("====================================================")

    load_env()
    api_key = os.environ.get("OPENROUTER_API_KEY")
    if not api_key:
        print("ERROR: OPENROUTER_API_KEY is not set.")
        sys.exit(1)

    with open(MANIFEST_PATH, "r", encoding="utf-8") as f:
        manifest_data = json.load(f)

    # Randomly sample the dataset to get a representative audit
    sample = random.sample(manifest_data, min(SAMPLE_SIZE, len(manifest_data)))
    print(f"Randomly sampled {len(sample)} segments out of {len(manifest_data)} total for brutal auditing.")

    client = AsyncOpenAI(
        base_url="https://openrouter.ai/api/v1",
        api_key=api_key,
    )

    system_prompt = (
        "You are an expert audio auditor and native Kurdish Sorani linguist. "
        "Listen to the audio clip and provide the 100% correct, orthographically standardized Kurdish Sorani transcription. "
        "Do not include any punctuation (like periods or commas) unless they are absolutely clear from the tone. "
        "Output ONLY the correct transcription text. Do not add any notes, introductions, or explanations. "
        "If the audio is completely unintelligible, hallucinated, only music/noise, or not Kurdish Sorani, output exactly '[QUARANTINE]'."
    )

    semaphore = asyncio.Semaphore(CONCURRENCY)
    tasks = [process_segment(client, item, semaphore, system_prompt) for item in sample]
    
    print(f"Starting analysis with Gemini 1.5 Pro...")
    results = await asyncio.gather(*tasks)
    
    # Calculate aggregate metrics
    valid_results = [r for r in results if r.get("status") in ["gold", "quarantine"]]
    quarantines = [r for r in valid_results if r.get("status") == "quarantine"]
    golds = [r for r in valid_results if r.get("status") == "gold"]
    
    if valid_results:
        avg_wer = sum(r.get("wer", 1.0) for r in valid_results) / len(valid_results)
    else:
        avg_wer = 0.0

    print("\n====================================================")
    print("AUDIT RESULTS")
    print("====================================================")
    print(f"Total Sampled: {len(results)}")
    print(f"Valid Processed: {len(valid_results)}")
    print(f"Hallucinated / Unintelligible (Quarantined): {len(quarantines)} ({(len(quarantines)/max(1, len(valid_results)))*100:.1f}%)")
    print(f"Average Word Error Rate (WER): {avg_wer:.2%}")
    print("====================================================")
    
    # Save detailed report
    with open(REPORT_PATH, "w", newline='', encoding="utf-8") as csvfile:
        fieldnames = ["id", "wav_file", "status", "wer", "candidate_transcript", "gemini_transcript", "error_reason"]
        writer = csv.DictWriter(csvfile, fieldnames=fieldnames)
        writer.writeheader()
        for r in results:
            writer.writerow({
                "id": r.get("id", ""),
                "wav_file": r.get("wav_file", ""),
                "status": r.get("status", ""),
                "wer": round(r.get("wer", 1.0), 3) if "wer" in r else "",
                "candidate_transcript": r.get("transcript", ""),
                "gemini_transcript": r.get("gemini_transcript", ""),
                "error_reason": r.get("error_reason", "")
            })
            
    print(f"Detailed report saved to: {REPORT_PATH}")

if __name__ == "__main__":
    asyncio.run(main())
