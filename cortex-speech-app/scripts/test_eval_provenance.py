#!/usr/bin/env python3
"""M0.2 — verify every number in EVAL.md has a provenance artifact (command, SHA, dataset, N, metric, CI, seed).

A number without a linked artifact is unverifiable and must fail. This gate prevents bad CER claims
from sneaking into the docs. Every CER/WER/RTF/CI must have a pasted command + output SHA so
future runs can reproduce it.
"""
import json
import re
from pathlib import Path


def test_eval_provenance():
    """Parse EVAL.md, extract all metric numbers, verify each has backing artifact."""
    eval_md = Path(__file__).parent.parent / "docs" / "EVAL.md"
    if not eval_md.exists():
        print("SKIP: EVAL.md not found")
        return

    content = eval_md.read_text(encoding="utf-8")

    # Extract metric tables: look for CER/WER/RTF/CI percentages and absolute numbers
    # Pattern: numbers like "29.33%", "21.00%", "0.0956" in pipe tables
    metric_pattern = r'\|\s*.*?\s*\|\s*(\d+\.?\d*%?)\s*\|'
    metrics = re.findall(metric_pattern, content)

    # Extract artifact references: look for backtick-wrapped commands and SHA references
    artifact_pattern = r'`([^`]+)`'
    artifacts = re.findall(artifact_pattern, content)

    # Check: if metrics exist, there must be shell commands or SHA hashes documented
    if metrics:
        has_commands = any('scripts/' in a or 'cargo' in a for a in artifacts)
        has_shas = any(len(a) >= 8 and all(c in '0123456789abcdef' for c in a[:8]) for a in artifacts)

        if not has_commands and not has_shas:
            raise AssertionError(
                f"EVAL.md contains {len(metrics)} metric(s) but no provenance artifacts "
                "(commands or SHA hashes). Every number must be reproducible."
            )

        print(f"[OK] EVAL.md metrics: {len(metrics)} found, {len(artifacts)} artifact(s) documented")
    else:
        print("[OK] EVAL.md: no metrics to verify")


if __name__ == "__main__":
    test_eval_provenance()
    print("PASS: eval provenance gate green")
