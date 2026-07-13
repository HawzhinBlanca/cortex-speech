#!/usr/bin/env python3
"""Enable precise word-level alignment by reusing the bundled OmniASR-CTC model as the forced aligner.

WHY THIS EXISTS
The app's ForcedAligner (src-tauri/src/aligner.rs) does exact CTC forced alignment when a
`mms_aligner.onnx` + `mms_aligner_tokens.txt` pair is present in its model dir; otherwise it falls
back to an energy heuristic that spaces words evenly and does NOT track the voice. No aligner model
is shipped, so word timings are heuristic. But the bundled OmniASR-CTC-300M model is a perfect
drop-in: its ONNX signature matches the aligner contract exactly (single raw-PCM input [1,N] f32 ->
logits [1,frames,9812]), it emits at ~50 fps (matching the aligner's 0.02 s stride), and its
character vocab covers every Central Kurdish letter with `<pad>` as the CTC blank. Verified by
greedy-decoding real Kurdish audio (correct transcript) and by replicating the forced alignment
(word durations vary 0.08–1.32 s, tracking the voice, vs the heuristic's flat spacing).

WHERE IT INSTALLS
The aligner reads `model_manager.models_dir` (pipeline.rs:3146) — the RAW app-data models dir,
`%APPDATA%/cortex-speech/models`, NOT the resolved bundled dir the ASR path uses. So the files must
go there. (The mismatch is a pre-existing quirk of pipeline.rs:3146 using the raw dir; this script
works with it rather than changing Codex-owned pipeline.rs.)

WHAT IT DOES (idempotent)
Hardlinks (or copies) the bundled `omniasr-ctc-300m/model.int8.onnx` to
`<app-data>/models/mms_aligner.onnx` and writes `mms_aligner_tokens.txt` (the sherpa `SYMBOL ID`
tokens reduced to one SYMBOL per line, index-aligned, as aligner.rs expects). No model download.

USAGE
  python scripts/setup_word_aligner.py            # auto-detect bundled model + app-data dir
  python scripts/setup_word_aligner.py --app-data "%APPDATA%/cortex-speech"
  python scripts/setup_word_aligner.py --check    # verify only, exit 1 if not installed

After install, click "Align" on a segment (or re-align) to get CTC word timings. Existing segments
keep their heuristic timings until re-aligned; new alignments use the CTC path.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path


def find_bundled_ctc() -> Path | None:
    """Locate omniasr-ctc-300m/model.int8.onnx among the same candidate dirs the app checks."""
    here = Path(__file__).resolve().parent.parent  # cortex-speech-app/
    candidates = [
        here / "src-tauri" / "target" / "release" / "models",
        here / "src-tauri" / "models",
        here / "src-tauri" / "target" / "debug" / "models",
    ]
    for d in candidates:
        m = d / "omniasr-ctc-300m" / "model.int8.onnx"
        if m.is_file():
            return d
    return None


def default_app_data() -> Path:
    base = os.environ.get("CORTEX_APP_DATA_DIR")
    if base:
        return Path(base)
    appdata = os.environ.get("APPDATA")  # Roaming on Windows
    if appdata:
        return Path(appdata) / "cortex-speech"
    return Path.home() / ".local" / "share" / "cortex-speech"


def tokens_stripped(ctc_tokens: Path) -> list[str]:
    """sherpa `SYMBOL<space>ID` -> one SYMBOL per line, verifying id == line index."""
    lines = ctc_tokens.read_text(encoding="utf-8").split("\n")
    if lines and lines[-1] == "":
        lines = lines[:-1]
    out, bad = [], 0
    for i, ln in enumerate(lines):
        # sherpa is `SYMBOL<space>ID` with a SINGLE space separator; match greedily up to the last
        # space so a symbol that IS a space (line "  4" == space-symbol + sep + id) keeps its space
        # rather than collapsing to empty (a non-greedy \s+ ate both spaces — caught by the gate).
        m = re.match(r"^(.*) (\d+)$", ln)
        if not m or int(m.group(2)) != i:
            bad += 1
            out.append(ln)
        else:
            out.append(m.group(1))
    if bad:
        raise SystemExit(f"ERROR: {bad} token line(s) not in `SYMBOL ID` index order — refusing to write a corrupt vocab")
    if "<pad>" not in out:
        raise SystemExit("ERROR: token vocab has no <pad> blank — aligner blank detection would fail")
    return out


def install(app_data: Path, bundled: Path) -> int:
    models = app_data / "models"
    models.mkdir(parents=True, exist_ok=True)
    ctc_model = bundled / "omniasr-ctc-300m" / "model.int8.onnx"
    ctc_tokens = bundled / "omniasr-ctc-300m" / "tokens.txt"
    if not ctc_model.is_file() or not ctc_tokens.is_file():
        print(f"ERROR: bundled CTC model/tokens not found under {bundled}", file=sys.stderr)
        return 1

    # Tokens FIRST (validate + write), model link second: an abort between the two steps must never
    # leave a model without its vocab — the app treats that pair as "aligner installed" and every
    # alignment would silently degrade. Tokens-without-model is harmless (aligner stays unavailable).
    toks = tokens_stripped(ctc_tokens)
    (models / "mms_aligner_tokens.txt").write_text("\n".join(toks) + "\n", encoding="utf-8")

    aligner_model = models / "mms_aligner.onnx"
    if aligner_model.exists():
        aligner_model.unlink()
    try:
        os.link(ctc_model, aligner_model)  # hardlink — no 365 MB duplicate
        how = "hardlinked"
    except OSError:
        import shutil

        shutil.copy2(ctc_model, aligner_model)
        how = "copied"

    print(f"OK: mms_aligner.onnx {how} + {len(toks)} tokens -> {models}")
    print('Click "Align" on a segment to use CTC word alignment.')
    return 0


def check(app_data: Path) -> int:
    models = app_data / "models"
    m, t = models / "mms_aligner.onnx", models / "mms_aligner_tokens.txt"
    ok = m.is_file() and t.is_file()
    print(f"aligner installed: {ok} ({models})")
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--app-data", help="app data dir (default: %APPDATA%/cortex-speech)")
    ap.add_argument("--check", action="store_true", help="verify only")
    args = ap.parse_args(argv)
    app_data = Path(args.app_data) if args.app_data else default_app_data()
    if args.check:
        return check(app_data)
    bundled = find_bundled_ctc()
    if bundled is None:
        print("ERROR: could not locate the bundled omniasr-ctc-300m model", file=sys.stderr)
        return 1
    return install(app_data, bundled)


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    sys.exit(main())
