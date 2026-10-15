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

    def test_external_offline_is_refused_by_every_automatic_promotion_path(self) -> None:
        registry = (RUST / "registry.rs").read_text(encoding="utf-8")
        runtime = (RUST / "champion_promotion_runtime.rs").read_text(encoding="utf-8")
        gate = function_body(registry, "pub fn gate_and_promote")
        self.assertIn("refuse_external_offline_promotion(", gate, "the scorecard gate refuses unconditionally")
        self.assertNotIn("owner_authorized", gate, "the scorecard gate has no owner exception")
        saga = function_body(runtime, "fn verify_registered_deployment")
        self.assertIn(
            "refuse_external_offline_promotion_unless_owner_authorized(",
            saga,
            "the saga refuses external_offline unless an owner_promote record names the exact candidate",
        )
        guard = function_body(registry, "pub(crate) fn refuse_external_offline_promotion_unless_owner_authorized")
        self.assertIn("owner_authorized == Some(identity)", guard)
        self.assertIn("refuse_external_offline_promotion(manifest)", guard, "every other case keeps the refusal")
        bootstrap = function_body(registry, "pub fn bootstrap_verified_legacy_deployment")
        self.assertIn("DeploymentProvenance::LegacyBootstrap", bootstrap)

    def test_the_saga_exception_comes_only_from_a_verified_owner_record(self) -> None:
        runtime = (RUST / "champion_promotion_runtime.rs").read_text(encoding="utf-8")
        recover = function_body(runtime, "pub(crate) fn recover_running_promotions")
        self.assertIn("owner_promotion::load_verified_authorization(", recover)
        self.assertEqual(1, runtime.count("ProductionPromotionRuntime::new("), "one construction site only")
        self.assertIn("ProductionPromotionRuntime::new(", recover)

    def test_only_the_owner_entry_point_admits_and_only_after_owner_authority(self) -> None:
        callers = [
            str(path.relative_to(ROOT))
            for path in RUST.rglob("*.rs")
            if path.name != "champion_promotion.rs" and "admit_owner_promotion(" in path.read_text(encoding="utf-8")
        ]
        self.assertEqual([str(Path("src-tauri/src/bin/owner_promote.rs"))], callers)
        entry = function_body((RUST / "bin" / "owner_promote.rs").read_text(encoding="utf-8"), "fn run(")
        order = [
            "verify_committed_authorization(",
            "verify_gate_evidence(",
            "InstanceLock::try_lock(",
            "require_interactive_confirmation(",
            "store_authorization(",
            "admit_owner_promotion(",
        ]
        positions = [entry.find(step) for step in order]
        self.assertNotIn(-1, positions, dict(zip(order, positions)))
        self.assertEqual(sorted(positions), positions, "owner authority must be proven before anything is written")
        self.assertNotIn("promote_to_champion(", entry, "the entry point admits; only the saga swaps")

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
