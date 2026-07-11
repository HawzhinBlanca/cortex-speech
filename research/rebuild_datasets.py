import os
import sys
import shutil
import subprocess

sys.stdout.reconfigure(encoding='utf-8')

_HERE = os.path.dirname(os.path.abspath(__file__))   # == the CORTEX repo root

DATASETS_DIR = os.path.join(_HERE, "kurdish_datasets")

def clean_dir(path):
    if os.path.exists(path):
        print(f"Cleaning: {path}")
        shutil.rmtree(path)

def main():
    print("====================================================")
    print("CODEX Dataset Clean Rebuild Utility")
    print("====================================================")

    # 1. Clean old/duplicate directories
    clean_dir(os.path.join(DATASETS_DIR, "1Cheshti_Mjewr"))
    clean_dir(os.path.join(DATASETS_DIR, "gold_dataset"))
    clean_dir(os.path.join(DATASETS_DIR, "quarantine_dataset"))

    # 2. Run Export
    print("\nRunning export_all_kurdish_datasets.py...")
    res = subprocess.run(["python", "export_all_kurdish_datasets.py"], capture_output=True, text=True, encoding="utf-8")
    print(res.stdout)
    if res.returncode != 0:
        print("Export failed!")
        return

    # 3. Run Master Manifest Generation
    print("\nRunning generate_master_manifest.py...")
    res = subprocess.run(["python", "generate_master_manifest.py"], capture_output=True, text=True, encoding="utf-8")
    print(res.stdout)
    if res.returncode != 0:
        print("Master manifest generation failed!")
        return

    # 4. Run Gold Filtration
    print("\nRunning analyze_and_filter_dataset.py...")
    res = subprocess.run(["python", "analyze_and_filter_dataset.py"], capture_output=True, text=True, encoding="utf-8")
    print(res.stdout)
    if res.returncode != 0:
        print("Gold filtration failed!")
        return

    print("====================================================")
    print("Clean Dataset Rebuild Complete!")
    print("====================================================")

if __name__ == "__main__":
    main()
