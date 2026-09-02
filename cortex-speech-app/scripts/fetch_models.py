#!/usr/bin/env python3
"""Fetch + SHA-256-verify Cortex runtime support assets into src-tauri/models/.

The standard fetch/build path provisions only the non-ASR assets required by the application
(Silero VAD and ONNX Runtime). The production ASR is the externally pinned OmniASR-7B champion;
smaller ASR models are never release prerequisites and are never fetched implicitly.

The SHA-256 of every file is pinned and authoritative — a download that does not match its pin is
rejected (no unverifiable artifact is ever placed), mirroring the in-app model manager's
ensure_pinned_sha256 / verify_sha256 policy.

Usage:
  python scripts/fetch_models.py            # provision required runtime support only
  python scripts/fetch_models.py --check    # verify required + any optional artifacts already present
  python scripts/fetch_models.py --include-optional-asr 300m
                                            # explicit diagnostic-only 300M provisioning

``--check`` is fully offline. Optional ASR artifacts may be absent, but a partially present or
hash-mismatched optional model is still an integrity failure. Downloads use pinned upstreams and
are placed only after SHA-256 verification.
"""
import argparse
import hashlib
import io
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

APP_DIR = Path(__file__).resolve().parent.parent
MODELS_DIR = APP_DIR / "src-tauri" / "models"

OMNIASR_300M_ARCHIVE = (
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/"
    "sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-int8-2025-11-12.tar.bz2"
)
OMNIASR_1B_ARCHIVE = (
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/"
    "sherpa-onnx-omnilingual-asr-1600-languages-1B-ctc-int8-2025-11-12.tar.bz2"
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
# The SAME official 1.24.4 release, linux-x64. Until 2026-08-08 this file provisioned a WINDOWS DLL
# and nothing else, on every platform -- so on Linux there was no ONNX Runtime for `ort` to dlopen.
#
# That is not a cosmetic gap. `ort` is built with `load-dynamic` (Cargo.toml), and when the runtime
# is ABSENT the first session does not fail, it BLOCKS FOREVER. Measured 2026-08-08 with an A/B on
# one Linux binary: without ORT_DYLIB_PATH the Silero VAD unit test was killed at 45 s; with it
# pointed at a real .so the SAME test passed in 0.21 s. Six of 1160 lib tests hung, all VAD/chunking,
# and the pipeline hung at plan_speech_chunks -- which is why the nightly real-audio job was killed
# at its timeout every night since at least 2026-07-30.
#
# The tarball ships lib/libonnxruntime.so as a SYMLINK to libonnxruntime.so.1.24.4, and tarfile's
# isfile() skips symlinks, so the member suffix must name the real versioned file.
ORT_LINUX_TGZ = (
    "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-x64-1.24.4.tgz"
)

# dest is relative to src-tauri/models/. sha256 is authoritative. "member" matches an archive entry
# by suffix.
#
# "platforms" restricts an item to the given sys.platform values; an item WITHOUT the key is fetched
# everywhere. The two Windows DLLs deliberately have no key: tauri.conf.json's bundle.resources lists
# them, and tauri-build validates that list on EVERY platform, so gating them off would break the
# Linux and macOS builds. Only the additive per-OS runtime is gated.
REQUIRED_ITEMS = [
    {
        "dest": "silero_vad_v4.onnx",
        "sha256": "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
        # VENDORED (2026-08-17): upstream replaced the file at SILERO_URL — master 404'd all three CI
        # build smokes, and the bytes now published there hash 6850b0e5…, not our pin. An unpinned
        # third-party URL for a 2.3 MB file the whole build depends on was a time bomb; the exact
        # pinned bytes now live in the repo (vendor/) and the URL is only a fallback if the vendored
        # copy is ever missing. sha256 verification applies to BOTH sources identically.
        "vendor": "vendor/silero_vad_v4.onnx",
        "url": SILERO_URL,
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
    # Flat in models/, because that is one of the places models::init_ort_dylib_path() searches
    # (`<active models dir>/<dylib>`) and ort_dylib_filename() returns "libonnxruntime.so" here.
    # Hashes computed from the downloaded official artifact, not copied from anywhere.
    {
        "dest": "libonnxruntime.so",
        "sha256": "d132535d051344ff5c64c9c200004150559049a81ed330eb4422c1962fb6b7e4",
        "archive": ORT_LINUX_TGZ,
        "member": "libonnxruntime.so.1.24.4",
        "platforms": ("linux",),
    },
    {
        "dest": "libonnxruntime_providers_shared.so",
        "sha256": "086ec1d5388f64153d9c63470d126693db9a182c8ce236d3a1119068471b8a0d",
        "archive": ORT_LINUX_TGZ,
        "member": "libonnxruntime_providers_shared.so",
        "platforms": ("linux",),
    },
    # NOT provisioned: macOS (libonnxruntime.dylib). No gate runs the pipeline on macOS -- the macOS
    # job only builds -- so pinning a third runtime would add an unverified download for coverage
    # nothing currently exercises. Add it the day a macOS job actually runs inference.
]

# Explicit diagnostics only. These pins remain useful for an owner-requested benchmark, but standard
# fetch/check/build/release paths never require or download them. Keep each engine as one group so a
# half-installed model is rejected rather than treated as a usable optional artifact.
OPTIONAL_ASR_ITEMS = {
    "300m": [
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
    ],
    "1b": [
        {
            "dest": "omniasr-ctc-1b/model.int8.onnx",
            "sha256": "f7b74c964039162423b83e3fa950ce24810c9a635d9ff8468b5f4d142b7c1e8c",
            "archive": OMNIASR_1B_ARCHIVE,
            "member": "model.int8.onnx",
        },
        {
            "dest": "omniasr-ctc-1b/tokens.txt",
            "sha256": "a7a044c52cb29cbe8b0dc1953e92cefd4ca16b0ed968177b6beab21f9a7d0b31",
            "archive": OMNIASR_1B_ARCHIVE,
            "member": "tokens.txt",
        },
    ],
    "mms": [
        {
            "dest": "finetuned-mms-ckb/model.onnx",
            "sha256": "064d6ec2225500cf7d47d267402f50c7c7da4d29f34d0fb6a9cb77272aef5ae0",
        },
        {
            "dest": "finetuned-mms-ckb/vocab.json",
            "sha256": "31dcd5c4361451991bd8241eb99bdc822d2ef2d8a4906404884c2196aa8f3a41",
        },
    ],
}
NON_FETCHABLE_OPTIONAL_ASR = frozenset({"mms"})


def applies_here(item: dict) -> bool:
    """Is this item wanted on the current platform? Items without "platforms" apply everywhere."""
    return sys.platform in item["platforms"] if "platforms" in item else True


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_of_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _check_items(items: list[dict], *, missing_is_error: bool) -> int:
    failed = 0
    for item in items:
        if not applies_here(item):
            print(f"  SKIP     {item['dest']} (not for {sys.platform})")
            continue
        dest = MODELS_DIR / item["dest"]
        if not dest.exists():
            label = "MISSING" if missing_is_error else "OPTIONAL"
            print(f"  {label:<8} {item['dest']}")
            if missing_is_error:
                failed += 1
            continue
        actual = sha256_of(dest)
        if actual == item["sha256"]:
            print(f"  OK       {item['dest']}")
        else:
            print(f"  MISMATCH {item['dest']}\n           expected {item['sha256']}\n           actual   {actual}")
            failed += 1
    return failed


def check(optional_asr: frozenset[str] = frozenset()) -> int:
    """Verify required assets and every selected or locally-present optional ASR group."""
    failed = _check_items(REQUIRED_ITEMS, missing_is_error=True)
    for name, items in OPTIONAL_ASR_ITEMS.items():
        present = any((MODELS_DIR / item["dest"]).exists() for item in items if applies_here(item))
        required = name in optional_asr or present
        if not required:
            print(f"  OPTIONAL {name} ASR not installed (not required)")
            continue
        # Once any member exists, require and hash the complete group. A valid model with missing
        # tokens is not a healthy optional installation.
        failed += _check_items(items, missing_is_error=True)
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


def download(optional_asr: frozenset[str] = frozenset()) -> int:
    archive_cache: dict[str, bytes] = {}
    failed = 0
    selected_items = list(REQUIRED_ITEMS)
    for name in sorted(optional_asr):
        if name in NON_FETCHABLE_OPTIONAL_ASR:
            print(f"  CHECK    optional {name} ASR is owner-supplied; verifying local pinned files")
            failed += _check_items(OPTIONAL_ASR_ITEMS[name], missing_is_error=True)
            continue
        selected_items.extend(OPTIONAL_ASR_ITEMS[name])
    for item in selected_items:
        if not applies_here(item):
            print(f"  SKIP     {item['dest']} (not for {sys.platform})")
            continue
        dest = MODELS_DIR / item["dest"]
        if dest.exists() and sha256_of(dest) == item["sha256"]:
            print(f"  SKIP     {item['dest']} (present + verified)")
            continue
        try:
            vendored = APP_DIR / item["vendor"] if "vendor" in item else None
            if vendored is not None and vendored.is_file():
                # In-repo copy first: deterministic, offline, immune to upstream renames/replacements.
                # The sha256 check below still gates it — a tampered vendor file is rejected the same
                # way a tampered download is.
                print(f"  vendored {item['vendor']}")
                data = vendored.read_bytes()
            elif "url" in item:
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
    # Standard provisioning does not fetch optional ASR. It still refuses to bless an optional
    # artifact already on disk if the group is incomplete or any byte fails its pin.
    for name, items in OPTIONAL_ASR_ITEMS.items():
        if name in optional_asr:
            continue
        present = any((MODELS_DIR / item["dest"]).exists() for item in items if applies_here(item))
        if present:
            failed += _check_items(items, missing_is_error=True)
    return failed


def main() -> int:
    ap = argparse.ArgumentParser(description="Fetch + verify Cortex runtime support assets.")
    ap.add_argument("--check", action="store_true", help="verify existing local files only (no network)")
    ap.add_argument(
        "--include-optional-asr",
        action="append",
        choices=sorted(OPTIONAL_ASR_ITEMS),
        default=[],
        metavar="ENGINE",
        help="explicitly require/fetch a diagnostic ASR engine (standard release paths never use this)",
    )
    args = ap.parse_args()
    optional_asr = frozenset(args.include_optional_asr)
    print(f"models dir: {MODELS_DIR}")
    failed = check(optional_asr) if args.check else download(optional_asr)
    if failed:
        print(f"\n{failed} file(s) missing or failed verification.")
        return 1
    print("\nAll required assets and selected/present optional artifacts are SHA-256 verified.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
