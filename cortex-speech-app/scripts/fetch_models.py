#!/usr/bin/env python3
"""Fetch + SHA-256-verify the ONNX models Cortex needs into src-tauri/models/.

Why: tauri.conf.json bundle.resources point at model files under src-tauri/models/, which are
gitignored — so a fresh clone cannot `npm run tauri build` until they exist. Run this first
(blocker #1, from-source build). The release INSTALLER already bundles these; this is for building
from source.

The SHA-256 of every file is pinned and authoritative — a download that does not match its pin is
rejected (no unverifiable artifact is ever placed), mirroring the in-app model manager's
ensure_pinned_sha256 / verify_sha256 policy.

Usage:
  python scripts/fetch_models.py            # download any missing/mismatched files, verify, place
  python scripts/fetch_models.py --check    # verify EXISTING local files against the pins (no network)

Note: --check is fully offline and is the CI/dev integrity gate. The download path requires network
to the pinned upstreams (sherpa-onnx / silero-vad / onnxruntime releases).
"""
import argparse
import hashlib
import io
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

MODELS_DIR = Path(__file__).resolve().parent.parent / "src-tauri" / "models"

OMNIASR_300M_ARCHIVE = (
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/"
    "sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-int8-2025-11-12.tar.bz2"
)
SILERO_URL = "https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx"
# ort 2.0 (load-dynamic) does NOT bundle the runtime — the matching ONNX Runtime win-x64 build
# provides onnxruntime.dll + onnxruntime_providers_shared.dll. The SHA-256 below is authoritative;
# if a different ONNX Runtime version is used the hash check will reject it (adjust the version then).
# Pinned to the official 1.24.4 CPU win-x64 release, which is C-API-compatible with ort 2.0.0-rc.12
# (Cargo.toml) and runs the fine-tuned MMS-CTC model on CPU. The earlier v1.20.1 URL never matched the
# pinned hashes (the pins were for a newer runtime), so this fetch step could never succeed until now;
# the two hashes below were verified by downloading this exact official release over HTTPS.
ORT_WIN_ZIP = (
    "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-win-x64-1.24.4.zip"
)

# dest is relative to src-tauri/models/. sha256 is authoritative (matches the in-repo pins in
# src-tauri/src/models.rs for the sherpa models). "member" matches an archive entry by suffix.
ITEMS = [
    {
        "dest": "silero_vad_v4.onnx",
        "sha256": "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
        "url": SILERO_URL,
    },
    {
        "dest": "omniasr-ctc-300m/model.int8.onnx",
        "sha256": "e7c4e54ee4c4c47829cc6667d5d00ed8ea7bef1dcfeef0fce766f77752a2726c",
        "archive": OMNIASR_300M_ARCHIVE,
        "member": "model.int8.onnx",
    },
    {
        "dest": "omniasr-ctc-300m/tokens.txt",
        "sha256": "a7a044c52cb29cbe8b0dc1953e92cefd4ca16b0ed968177b6beab21f9a7d0b31",
        "archive": OMNIASR_300M_ARCHIVE,
        "member": "tokens.txt",
    },
    {
        "dest": "onnxruntime.dll/onnxruntime.dll",
        "sha256": "b95efb2113b603bbbf3f191061c5516a871ed546893c820e4f3b7b6c358dbf2a",
        "archive": ORT_WIN_ZIP,
        "member": "onnxruntime.dll",
    },
    {
        "dest": "onnxruntime.dll/onnxruntime_providers_shared.dll",
        "sha256": "f2540b89707b47895c2a732bfd04e34a695c580d22301ef44c0f01f09b001673",
        "archive": ORT_WIN_ZIP,
        "member": "onnxruntime_providers_shared.dll",
    },
]


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_of_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def check() -> int:
    failed = 0
    for item in ITEMS:
        dest = MODELS_DIR / item["dest"]
        if not dest.exists():
            print(f"  MISSING  {item['dest']}")
            failed += 1
            continue
        actual = sha256_of(dest)
        if actual == item["sha256"]:
            print(f"  OK       {item['dest']}")
        else:
            print(f"  MISMATCH {item['dest']}\n           expected {item['sha256']}\n           actual   {actual}")
            failed += 1
    return failed


def _download(url: str) -> bytes:
    print(f"  downloading {url}")
    with urllib.request.urlopen(url) as resp:  # noqa: S310 - pinned upstream + SHA-verified below
        return resp.read()


def _extract_member(archive_bytes: bytes, url: str, member_suffix: str) -> bytes:
    if url.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(archive_bytes)) as zf:
            for name in zf.namelist():
                if name.endswith(member_suffix) and not name.endswith("/"):
                    return zf.read(name)
    else:  # tar.bz2 / tar.gz
        with tarfile.open(fileobj=io.BytesIO(archive_bytes)) as tf:
            for m in tf.getmembers():
                if m.isfile() and m.name.endswith(member_suffix):
                    f = tf.extractfile(m)
                    if f is not None:
                        return f.read()
    raise RuntimeError(f"member ending in {member_suffix!r} not found in {url}")


def download() -> int:
    archive_cache: dict[str, bytes] = {}
    failed = 0
    for item in ITEMS:
        dest = MODELS_DIR / item["dest"]
        if dest.exists() and sha256_of(dest) == item["sha256"]:
            print(f"  SKIP     {item['dest']} (present + verified)")
            continue
        try:
            if "url" in item:
                data = _download(item["url"])
            else:
                if item["archive"] not in archive_cache:
                    archive_cache[item["archive"]] = _download(item["archive"])
                data = _extract_member(archive_cache[item["archive"]], item["archive"], item["member"])
            actual = sha256_of_bytes(data)
            if actual != item["sha256"]:
                print(f"  REJECT   {item['dest']} (sha256 {actual} != pinned {item['sha256']})")
                failed += 1
                continue
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
            print(f"  WROTE    {item['dest']} ({len(data)} bytes, sha256 verified)")
        except Exception as e:  # noqa: BLE001
            print(f"  ERROR    {item['dest']}: {e}")
            failed += 1
    return failed


def main() -> int:
    ap = argparse.ArgumentParser(description="Fetch + verify Cortex ONNX models.")
    ap.add_argument("--check", action="store_true", help="verify existing local files only (no network)")
    args = ap.parse_args()
    print(f"models dir: {MODELS_DIR}")
    failed = check() if args.check else download()
    if failed:
        print(f"\n{failed} file(s) missing or failed verification.")
        return 1
    print("\nAll model files present and SHA-256 verified.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
