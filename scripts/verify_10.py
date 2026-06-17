import os
import json
import re
import sys

def load_json(path):
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)

def check_manifests():
    print("==> Checking manifest version and license alignment...")
    
    # 1. Read package.json
    pkg_path = "cortex-speech-app/package.json"
    if not os.path.exists(pkg_path):
        print(f"Error: {pkg_path} not found.")
        return False
    pkg = load_json(pkg_path)
    pkg_ver = pkg.get("version")
    pkg_license = pkg.get("license")
    
    # 2. Read tauri.conf.json
    tauri_path = "cortex-speech-app/src-tauri/tauri.conf.json"
    if not os.path.exists(tauri_path):
        print(f"Error: {tauri_path} not found.")
        return False
    tauri = load_json(tauri_path)
    tauri_ver = tauri.get("version")
    
    # 3. Read Cargo.toml
    cargo_path = "cortex-speech-app/src-tauri/Cargo.toml"
    if not os.path.exists(cargo_path):
        print(f"Error: {cargo_path} not found.")
        return False
        
    cargo_ver = None
    cargo_license = None
    with open(cargo_path, "r", encoding="utf-8") as f:
        content = f.read()
        ver_match = re.search(r'^version\s*=\s*"([^"]+)"', content, re.MULTILINE)
        lic_match = re.search(r'^license\s*=\s*"([^"]+)"', content, re.MULTILINE)
        if ver_match:
            cargo_ver = ver_match.group(1)
        if lic_match:
            cargo_license = lic_match.group(1)

    print(f"  package.json:   version={pkg_ver}, license={pkg_license}")
    print(f"  tauri.conf.json: version={tauri_ver}")
    print(f"  Cargo.toml:     version={cargo_ver}, license={cargo_license}")
    
    # Assertions
    all_ok = True
    if not (pkg_ver == tauri_ver == cargo_ver):
        print("Error: Version mismatch across manifests!")
        all_ok = False
        
    if pkg_license != "Apache-2.0" or cargo_license != "Apache-2.0":
        print("Error: License mismatch or not Apache-2.0!")
        all_ok = False
        
    return all_ok

def check_required_files():
    print("==> Checking required repository assets...")
    required = [
        "LICENSE",
        "NOTICE",
        "DATA_GOVERNANCE.md",
        "AGENT_CHARTER.md",
        "docs/ROADMAP_TO_10.md",
        "docs/RESEARCH_SOTA_2026.md"
    ]
    all_ok = True
    for filepath in required:
        if os.path.exists(filepath):
            print(f"  [OK]  Found {filepath}")
        else:
            print(f"  [ERR] Missing {filepath}")
            all_ok = False
    return all_ok

def check_provenance_ledger():
    print("==> Checking provenance ledger schema integrity...")
    ledger_path = "docs/provenance_ledger.json"
    if not os.path.exists(ledger_path):
        print("Error: docs/provenance_ledger.json not found.")
        return False
        
    try:
        ledger = load_json(ledger_path)
    except Exception as e:
        print(f"Error parsing JSON: {e}")
        return False
        
    required_keys = [
        "corpus", "sourceUrl", "spdxLicense", "shareAlike", 
        "attributionString", "consentBasis", "redistributionRights", "takedownContact"
    ]
    
    all_ok = True
    for row in ledger:
        corpus = row.get("corpus", "unknown")
        for key in required_keys:
            if key not in row:
                print(f"  [ERR] Corpus '{corpus}' is missing key '{key}'")
                all_ok = False
        if all_ok:
            print(f"  [OK]  Corpus '{corpus}' ledger entry verified ({row.get('spdxLicense')})")
            
    return all_ok

def main():
    print("==================================================")
    print("          CORTEX 10/10 VERIFICATION GATES         ")
    print("==================================================")
    
    ok1 = check_manifests()
    ok2 = check_required_files()
    ok3 = check_provenance_ledger()
    
    print("--------------------------------------------------")
    if ok1 and ok2 and ok3:
        print("CORTEX 10/10: ALL GATES GREEN")
        sys.exit(0)
    else:
        print("CORTEX VERIFICATION FAILED: RED GATES PRESENT")
        sys.exit(1)

if __name__ == "__main__":
    main()
