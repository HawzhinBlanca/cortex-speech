#!/usr/bin/env python3
"""Champion supremacy + hard stop (owner rule, 2026-08-11). See CLAUDE.md.

Both halves exist because BOTH failed silently on the same real run:

  * A 494-clip review queue was drafted 494/494 by `finetuned-mms-ckb` while `asr_model_size` said
    WSL7B and the champion sat up and idle on both GPUs. `use_finetuned_asr` diverted every clip and
    nothing said so. Measured gap on identical FLEURS ckb clips: 7.03% CER vs 9.32%, and the app runs
    the int8 build whose own baseline is 21.00%.
  * 25 clips whose container the champion could not decode failed one at a time, were counted, and the
    batch ran to "completion" — leaving 462 clips at champion quality and 25 at a weaker engine,
    invisibly mixed.

A partly-drafted dataset that LOOKS finished is worse than a run that stopped, because the mixed
provenance silently poisons every measurement taken from it afterwards. These are source pins: the
behaviour lives across a threaded batch loop and a WSL server, which no unit test can exercise here.
"""

from __future__ import annotations

import re
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src-tauri" / "src"
PIPELINE = SRC / "pipeline.rs"
COMMANDS = SRC / "commands.rs"


def _fn_body(text: str, signature: str) -> str:
    start = text.find(signature)
    if start == -1:
        raise AssertionError(f"{signature!r} not found — this gate would pass vacuously")
    rest = text[start + len(signature):]
    end = rest.find("\n    fn ")
    other = rest.find("\n    pub fn ")
    if other != -1 and (end == -1 or other < end):
        end = other
    return rest[:end if end != -1 else len(rest)]


def test_selecting_the_champion_cannot_be_overridden_by_the_small_model() -> None:
    """`should_use_wsl_primary_asr` must depend ONLY on the selected engine + a client script.

    The removed clause was `&& !(use_finetuned_asr && finetuned_model_paths().is_some())` — the exact
    line that silently handed 494 clips to the smaller model.
    """
    text = PIPELINE.read_text(encoding="utf-8")
    body = _fn_body(text, "fn should_use_wsl_primary_asr(&self) -> bool {")
    assert "use_finetuned_asr" not in body, (
        "should_use_wsl_primary_asr consults use_finetuned_asr again — selecting WSL7B must outrank "
        "the flag, or the champion is silently replaced by the smaller model"
    )
    assert "AsrModelSize::WSL7B" in body, "the gate no longer keys on the selected engine"


def test_the_finetuned_override_yields_to_the_champion() -> None:
    """Every primary-drafter override goes through `finetuned_override_active`, which yields to WSL7B."""
    text = PIPELINE.read_text(encoding="utf-8")
    body = _fn_body(text, "fn finetuned_override_active(&self) -> bool {")
    assert "use_finetuned_asr" in body and "WSL7B" in body, (
        "finetuned_override_active must require the flag AND that the champion is not the selection"
    )
    assert "!=" in body, "the override must be DISABLED when WSL7B is selected, not enabled by it"

    # No primary-drafter branch may read the raw flag any more.
    for match in re.finditer(r"if self\.settings\.use_finetuned_asr\s*\{", text):
        line = text[: match.start()].count("\n") + 1
        raise AssertionError(
            f"pipeline.rs:{line} branches on the raw use_finetuned_asr flag; use "
            "finetuned_override_active() so selecting the champion outranks it"
        )


def test_batch_transcribe_hard_stops_on_the_first_failure() -> None:
    """A failure must cancel the run and be reported as halted — never counted and carried past."""
    text = COMMANDS.read_text(encoding="utf-8")

    assert "record_first_failure(&first_failure" in text, (
        "batch_transcribe no longer records the failure that stopped it"
    )
    assert "cancel.cancel();" in text, (
        "batch_transcribe does not cancel on failure — it would keep drafting and finish with a tally, "
        "leaving a half-champion dataset that looks complete"
    )
    assert '"halted"' in text and '"haltedBy"' in text, (
        "a stopped run must be emitted as halted with its cause, never as an ordinary completion"
    )

    halt = text.find("if first_failure.lock()")
    assert halt != -1, "the hard-stop check is gone from the worker loop"
    assert "break;" in text[halt:halt + 300], "the hard-stop check does not actually stop the loop"


def main() -> None:
    test_selecting_the_champion_cannot_be_overridden_by_the_small_model()
    test_the_finetuned_override_yields_to_the_champion()
    test_batch_transcribe_hard_stops_on_the_first_failure()
    print("champion supremacy + hard stop policy passed")


if __name__ == "__main__":
    main()
