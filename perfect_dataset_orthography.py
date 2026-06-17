import os
import json
import sqlite3
import re
import sys

sys.stdout.reconfigure(encoding='utf-8')

MANIFEST_PATH = r"C:\Users\hawzh\Desktop\CORTEX\kurdish_datasets\manifest_verified.json"
DB_PATH = r"C:\Users\hawzh\AppData\Roaming\cortex-speech\cortex-speech.db"

# Kurdish Number Spelling Converter
def num_to_kurdish(n):
    if n == 0:
        return "سفر"
    
    units = ["", "یەک", "دوو", "سێ", "چوار", "پێنج", "شەش", "حەوت", "هەشت", "نۆ"]
    teens = {
        10: "دە", 11: "یازدە", 12: "دوانزدە", 13: "سیانزدە", 14: "چواردە",
        15: "پازدە", 16: "شازدە", 17: "حەڤدە", 18: "هەژدە", 19: "نۆزدە"
    }
    tens = ["", "", "بیست", "سی", "چل", "پەنجا", "شەست", "حەفتا", "هەشتا", "نەوەد"]
    hundreds = ["", "سەد", "دووسەد", "سێسەد", "چوارسەد", "پێنجسەد", "شەشسەد", "حەوتسەد", "هەشتسەد", "نۆسەد"]
    
    parts = []
    
    if n >= 10**9:
        b = n // 10**9
        parts.append(num_to_kurdish(b) + " ملیار")
        n %= 10**9
        
    if n >= 10**6:
        m = n // 10**6
        parts.append(num_to_kurdish(m) + " ملیۆن")
        n %= 10**6
        
    if n >= 1000:
        t = n // 1000
        if t == 1:
            parts.append("هەزار")
        else:
            parts.append(num_to_kurdish(t) + " هەزار")
        n %= 1000
        
    if n >= 100:
        h = n // 100
        parts.append(hundreds[h])
        n %= 100
        
    if n >= 20:
        t = n // 10
        u = n % 10
        parts.append(tens[t])
        if u > 0:
            parts.append(units[u])
    elif n >= 10:
        parts.append(teens[n])
    elif n > 0:
        parts.append(units[n])
        
    return " و ".join([p for p in parts if p])

def normalize_digits(text):
    eastern_to_western = {
        '٠': '0', '١': '1', '٢': '2', '٣': '3', '٤': '4',
        '٥': '5', '٦': '6', '٧': '7', '٨': '8', '٩': '9',
        '۰': '0', '۱': '1', '۲': '2', '۳': '3', '۴': '4',
        '۵': '5', '۶': '6', '۷': '7', '۸': '8', '۹': '9'
    }
    for e_digit, w_digit in eastern_to_western.items():
        text = text.replace(e_digit, w_digit)
    return text

def replace_numbers_with_words(text):
    text = normalize_digits(text)
    
    def repl(match):
        num_str = match.group(0)
        num = int(num_str)
        return num_to_kurdish(num)
        
    # Replace any sequence of digits
    return re.sub(r'\d+', repl, text)

# Kurdish standard substitutions
def normalize_orthography(text):
    if not text:
        return ""
    
    # Character standardizations
    text = text.replace("ك", "ک")
    text = text.replace("ي", "ی")
    text = text.replace("ى", "ی")
    text = text.replace("ة", "ە")
    
    # Specific English loanword mappings for transliteration
    latin_map = {
        r'\(POV\)ی': 'پی ئۆ ڤیی',
        r'POV': 'پی ئۆ ڤی',
        r'ڕەحمət': 'ڕەحمەت',
        r'near death experienceی': 'نیر دێس ئێکسپێریێنسە',
        r'near death experience': 'نیر دێس ئێکسپێریێنس',
        r'the great recession': 'زە گرەیت ڕیسێشن',
        r'A\.I': 'ئەی ئای',
        r'PDF': 'پی دی ئێف',
        r'kgn\.krd': 'کەی جی ئێن دۆت کورد',
        r'k g n a\.k r d': 'کەی جی ئێن ئەی دۆت کورد',
        r'the arms race, nuclear arms race': 'زە ئارمس ڕەیس، نیوکلیەر ئارمس ڕەیس',
        r'non-proliferation treaty': 'نۆن پرۆلیفەرەیشن تریتی',
        r'the climate education school program': 'زە کلایمێت ئێدوکەیشن سکوڵ پرۆگرام',
        r'وەڵڵahi': 'وەڵڵاهی',
        r'My favorite': 'مای فەیڤەریت',
        r'tiktiktiktok': 'تیکتیکتیکتۆک',
        r'Oh My God': 'ئۆ مای گاد',
        r'</h1>': '',
        r'A': 'ئەی',
        r'B': 'بی'
    }
    
    for pattern, replacement in latin_map.items():
        text = re.sub(pattern, replacement, text, flags=re.IGNORECASE)
        
    # Remove any leftover Latin characters that are stray/noise
    text = re.sub(r'[a-zA-Z]', '', text)
    
    # Clean spaces
    text = re.sub(r'\s+', ' ', text).strip()
    return text

def perfect_text(text):
    if not text:
        return ""
    # 1. Replace numbers with words
    text = replace_numbers_with_words(text)
    # 2. Normalize orthography and transliterate Latin
    text = normalize_orthography(text)
    return text

def main():
    print("====================================================")
    print("CODEX Kurdish Dataset Orthographic & Number Perfector")
    print("====================================================")
    
    # 1. Update manifest_verified.json
    if os.path.exists(MANIFEST_PATH):
        print(f"Loading verified manifest: {MANIFEST_PATH}")
        with open(MANIFEST_PATH, "r", encoding="utf-8") as f:
            data = json.load(f)
            
        print(f"Perfecting {len(data)} manifest transcripts...")
        corrected_count = 0
        for item in data:
            orig_text = item.get("verified_transcript", item.get("transcript", ""))
            perf_text = perfect_text(orig_text)
            
            if perf_text != orig_text:
                item["verified_transcript"] = perf_text
                corrected_count += 1
                
        with open(MANIFEST_PATH, "w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        print(f"Successfully updated verified manifest! Corrected {corrected_count} entries.")
    else:
        print("Verified manifest not found.")
        
    # 2. Update SQLite database
    if os.path.exists(DB_PATH):
        print(f"\nOpening database: {DB_PATH}")
        try:
            conn = sqlite3.connect(DB_PATH)
            cursor = conn.cursor()
            
            cursor.execute("SELECT id, raw_transcript, normalized_transcript, annotated_transcript FROM speech_segments")
            rows = cursor.fetchall()
            
            print(f"Perfecting {len(rows)} database transcripts...")
            db_corrected_count = 0
            
            for row in rows:
                seg_id, raw, normalized, annotated = row
                
                # Retrieve current text
                curr_text = annotated or normalized or raw
                if not curr_text:
                    continue
                    
                perf_text = perfect_text(curr_text)
                
                if perf_text != curr_text:
                    # Update database with perfected text
                    cursor.execute(
                        "UPDATE speech_segments SET annotated_transcript = ?, normalized_transcript = ?, updated_at = datetime('now') WHERE id = ?",
                        (perf_text, perf_text, seg_id)
                    )
                    db_corrected_count += 1
                    
            conn.commit()
            conn.close()
            print(f"Successfully updated database! Corrected {db_corrected_count} entries.")
        except Exception as e:
            print(f"Database error: {e}")
    else:
        print("SQLite database not found.")
        
    print("\n====================================================")
    print("Spelling & Number Perfection Complete!")
    print("====================================================")

if __name__ == "__main__":
    main()
