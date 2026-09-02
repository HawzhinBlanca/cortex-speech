#!/usr/bin/env python3
"""Fail closed if an untrusted Tauri command can bypass the promotion saga."""

from __future__ import annotations

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
RUST = ROOT / "src-tauri" / "src"


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        raise AssertionError(f"missing function signature {signature!r}")
    brace = source.find("{", start)
    if brace < 0:
        raise AssertionError(f"missing body for {signature!r}")
    depth = 0
    for index in range(brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[brace : index + 1]
    raise AssertionError(f"unterminated body for {signature!r}")


class PromotionTrustBoundary(unittest.TestCase):
    def test_legacy_gate_is_decision_only(self) -> None:
        registry = (RUST / "registry.rs").read_text(encoding="utf-8")
        body = function_body(registry, "pub fn gate_and_promote")
        self.assertNotIn(
            "promote_to_champion(",
            body,
            "the legacy scorecard helper must never bypass pointer/restart/canary/rollback",
        )

    def test_renderer_has_no_promotion_admission_surface_yet(self) -> None:
        lib = (RUST / "lib.rs").read_text(encoding="utf-8")
        handler = re.search(r"\.invoke_handler\(tauri::generate_handler!\[(.*?)\]\)", lib, re.DOTALL)
        self.assertIsNotNone(handler, "could not locate Tauri invoke handler")
        exposed = handler.group(1).lower()
        self.assertNotIn("promote", exposed)
        self.assertNotIn("start_promotion", exposed)

        command_sources = [RUST / "commands.rs", *(RUST / "commands").glob("*.rs")]
        for path in command_sources:
            text = path.read_text(encoding="utf-8")
            self.assertNotIn(
                "champion_promotion::start_promotion",
                text,
                f"{path.name} exposes saga admission before backend evidence authorization exists",
            )

    def test_only_production_runtime_mutates_champion_outside_tests(self) -> None:
        offenders: list[str] = []
        for path in RUST.rglob("*.rs"):
            if path.name in {"registry.rs", "db_tests.rs", "champion_promotion_runtime.rs"}:
                continue
            text = path.read_text(encoding="utf-8")
            if "promote_to_champion(" in text:
                offenders.append(str(path.relative_to(ROOT)))
        self.assertEqual([], offenders, f"direct champion mutation escaped the production saga: {offenders}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
