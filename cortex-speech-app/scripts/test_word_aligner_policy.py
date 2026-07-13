#!/usr/bin/env python3
"""Regression gate for setup_word_aligner.py — the CTC->forced-aligner token conversion.

The aligner parses each tokens.txt line as ONE token (index = vocab column). The bundled CTC vocab
is sherpa `SYMBOL<space>ID`, so a raw copy would make every char-lookup and the <pad> blank miss and
silently degrade to the energy heuristic. This pins the conversion: strip the trailing id, keep
order, and REFUSE to write a vocab whose ids aren't index-aligned or that lacks the <pad> blank
(either would produce silently-wrong word timings)."""

import importlib.util
import sys
import tempfile
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "setup_word_aligner", Path(__file__).parent / "setup_word_aligner.py"
)
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)


def _write(lines):
    d = tempfile.mkdtemp()
    p = Path(d) / "tokens.txt"
    p.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return p


def test_strips_ids_and_preserves_order():
    # a realistic slice: specials, a literal space token, Kurdish letters — sherpa "SYMBOL ID" form
    toks = _mod.tokens_stripped(_write(["<s> 0", "<pad> 1", "</s> 2", "<unk> 3", "  4", "ئ 5", "ا 6"]))
    assert toks == ["<s>", "<pad>", "</s>", "<unk>", " ", "ئ", "ا"], toks
    assert toks[1] == "<pad>", "blank must remain at index 1"


def test_rejects_out_of_order_ids():
    # id column not equal to line index => refuse (would misalign the whole vocab)
    try:
        _mod.tokens_stripped(_write(["<s> 0", "<pad> 1", "ئ 5"]))  # 'ئ' claims id 5 on line 2
    except SystemExit as e:
        assert "index order" in str(e)
    else:
        raise AssertionError("must reject ids that are not index-aligned")


def test_rejects_missing_pad_blank():
    try:
        _mod.tokens_stripped(_write(["<s> 0", "<unk> 1", "ئ 2"]))  # no <pad>
    except SystemExit as e:
        assert "<pad>" in str(e)
    else:
        raise AssertionError("must reject a vocab with no <pad> blank")


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    test_strips_ids_and_preserves_order()
    test_rejects_out_of_order_ids()
    test_rejects_missing_pad_blank()
    print("PASS: word aligner setup policy")
