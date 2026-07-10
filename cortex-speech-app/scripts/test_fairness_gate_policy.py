"""Regression gate for the fairness-gender-age gate (auto-discovered by run_python_policies.py).

Pins: the committed fairness scorecard stays schema-valid, the gate's disparity logic holds,
and the gate returns GREEN on the committed data — so a bad edit to either the data or the
gate is caught in the sandbox suite (npm run test:python-policies), not only at verify_10 time.
"""
import importlib.util
import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCORECARD = REPO_ROOT / "docs" / "fairness_scorecard.json"
GATE = REPO_ROOT / "scripts" / "fairness_gate.py"


def _load_gate():
    spec = importlib.util.spec_from_file_location("fairness_gate", GATE)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_scorecard_is_wellformed() -> None:
    card = json.loads(SCORECARD.read_text(encoding="utf-8"))
    for key in ("metric", "model", "n_floor", "budget_pts", "groups", "provenance"):
        assert key in card, f"fairness_scorecard.json missing '{key}'"
    assert card["groups"], "scorecard has no groups"
    for axis, groups in card["groups"].items():
        for g, v in groups.items():
            assert isinstance(v.get("n"), int) and v["n"] >= 0, f"{axis}/{g} bad n"
            assert isinstance(v.get("cer"), (int, float)) and v["cer"] >= 0, f"{axis}/{g} bad cer"


def test_gate_selftest_passes() -> None:
    _load_gate()._selftest()  # raises AssertionError on any logic regression


def test_committed_data_is_green() -> None:
    mod = _load_gate()
    card = json.loads(SCORECARD.read_text(encoding="utf-8"))
    ok, gated_any, _ = mod.evaluate(card)
    assert gated_any, "committed data must gate at least one axis (else the gate is vacuous)"
    assert ok, "committed fairness data exceeds its own disparity budget — RED"


def main() -> None:
    test_scorecard_is_wellformed()
    test_gate_selftest_passes()
    test_committed_data_is_green()
    print("fairness gate policy regression passed")


if __name__ == "__main__":
    main()
