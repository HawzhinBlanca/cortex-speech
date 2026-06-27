import sqlite3
import os
import sys
import re

sys.stdout.reconfigure(encoding='utf-8')

appdata = os.environ.get("APPDATA")
DB_PATH = os.path.join(appdata, "cortex-speech", "cortex-speech.db") if appdata else os.path.expanduser("~/AppData/Roaming/cortex-speech/cortex-speech.db")

# Dictionary of common phonetic and split-word errors in Meta OmniASR Sorani Kurdish outputs
CORRECTION_MAP = {
    # Splits and word boundaries
    r'\bب دنګی\b': 'بە دەنگی',
    r'\bبدنجی\b': 'بە دەنگی',
    r'\bبدنجي\b': 'بە دەنگی',
    r'\bأمد\b': 'ئەحمەد',
    r'\bسلیب\b': 'سەلیم',
    r'\bهمی شخم\b': 'هەمیشە خۆم',
    r'\bلابواردوة\b': 'لێ بواردووە',
    r'\bحولی\b': 'هەوڵی',
    r'\bرزغاری\b': 'ڕزگاری',
    r'\bلا خمو\b': 'لە خەم و',
    r'\bدردبدا\b': 'دەرد بدا',
    r'\bتێبی\b': 'کتێبی',
    r'\bچێفتی\b': 'چێشتی',
    r'\bعمد\b': 'ئەحمەد',
    r'\bعلیو\b': 'عەلی و',
    r'\bتومار\b': 'تۆمار',
    r'\bدلیر\b': 'دلێر',
    r'\bنوسین\b': 'نووسین',
    r'\bنوسینی\b': 'نووسینی',
    r'\bسلی\b': 'سەلیم',
    r'\bهبو\b': 'هەبوو',
    r'\bفکر\b': 'فیکر',
    r'\bبەڵک و\b': 'بەڵکو',
    r'\bشالیش\b': 'شادیش',
    r'\bدنګ\b': 'دەنگ',
    r'\bدنګی\b': 'دەنگی',
    r'\bدرێش\b': 'درێژ',
    r'\bدەمو\b': 'دەم و',
    r'\bشاو\b': 'شا و',
    r'\bبجێەوری\b': 'مجێوری',
    r'\bنووسی ومە\b': 'نووسیومە',
    r'\bیاندی ومە\b': 'یا دیتوومە',
    r'\bکۆستۆتەوە\b': 'بیستوومەتەوە',
    r'\bداڵیم دام\b': 'منداڵیمدا',
    r'\bونا خۆشیانی\b': 'و ناخۆشیانەی',
    r'\bدەرسێك\b': 'دەرسێک',
    r'\bتجربی\b': 'تەجرەبەی',
    r'\bپیدا\b': 'پەیدا',
    r'\bتەنگا\b': 'تەنگانەدا',
    r'\bناهومێت\b': 'ناهومێد',
    r'\bلایوابیا\b': 'لای وابێت',
    r'\bڕێگەی\b': 'ڕێگای',
    r'\bڕزگاربوون\b': 'ڕزگاربوون',
    r'\bجیراوە\b': 'گیراوە',
    r'\bسو دیی\b': 'ئاسوودەیی',
    r'\bخاترجم\b': 'خاترجەم',
    r'\bمغلور\b': 'مەغروور',
    r'\bهەمی شه\b': 'هەمیشە',
    r'\bخوەم\b': 'خۆم',
    r'\bلیە\b': 'لێ',
    r'\bبواردووه\b': 'بواردووە',
    r'\bبکوڕکم\b': 'بۆ کوڕەکەم',
    # Letter-level mappings
    r'دنګ': 'دەنگ',
    r'ګ': 'گ',  # Replace Arabic Gaf with standard Kurdish/Persian Gaf
    r'ك': 'ک',
    r'ي': 'ی',
    r'ى': 'ی',
}

def correct_text(text):
    if not text:
        return text
    
    # Apply regex corrections
    for pattern, replacement in CORRECTION_MAP.items():
        text = re.sub(pattern, replacement, text)
        
    return text

def main():
    print("====================================================")
    print("CODEX Kurdish Transcript Refinement & Spelling Corrector")
    print(f"Target Database: {DB_PATH}")
    print("====================================================")

    if not os.path.exists(DB_PATH):
        print(f"Error: Database not found at {DB_PATH}")
        return

    try:
        conn = sqlite3.connect(DB_PATH)
        cursor = conn.cursor()
        
        # Select all speech segments
        cursor.execute("SELECT id, raw_transcript, normalized_transcript, annotated_transcript FROM speech_segments")
        rows = cursor.fetchall()
        print(f"Processing {len(rows)} segments...")
        
        updates = 0
        for row in rows:
            seg_id, raw, normalized, annotated = row
            
            # Refine annotated transcript
            curr_text = annotated or normalized or raw
            refined_text = correct_text(curr_text)
            
            if refined_text != curr_text:
                cursor.execute(
                    "UPDATE speech_segments SET annotated_transcript = ?, normalized_transcript = ?, updated_at = datetime('now') WHERE id = ?",
                    (refined_text, refined_text, seg_id)
                )
                updates += 1
                
        conn.commit()
        conn.close()
        print(f"Successfully refined and corrected {updates} database transcripts!")
        
    except Exception as e:
        print(f"Database operation failed: {e}")
        return

    # Trigger complete clean rebuild of datasets in correct order
    print("\nTriggering E2E Dataset Clean Rebuild...")
    import subprocess
    res_rebuild = subprocess.run(["python", "rebuild_datasets.py"], capture_output=True, text=True, encoding="utf-8")
    print(res_rebuild.stdout)
    
    print("====================================================")
    print("Kurdish Transcript Correction Complete!")
    print("====================================================")

if __name__ == "__main__":
    main()
