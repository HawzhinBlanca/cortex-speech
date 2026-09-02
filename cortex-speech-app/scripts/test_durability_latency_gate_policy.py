"""Pin the isolated durable-decision latency release gate.

Wall-clock fsync timing is meaningless while the library harness concurrently runs hundreds of
other durable SQLite tests. The benchmark therefore stays ignored in the parallel suite but must
remain an exact, anti-vacuity verify-10 gate with its original 250 ms threshold.
"""

import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve()
APP_ROOT = SCRIPT.parents[1]
REPO_ROOT = APP_ROOT.parent
VERIFY_10 = REPO_ROOT / "scripts" / "verify_10.py"
DB_TESTS = APP_ROOT / "src-tauri" / "src" / "db_tests.rs"
BENCHMARK = "db::tests::the_durability_cost_per_decision_is_measured_not_assumed"


class DurableDecisionLatencyGatePolicyTests(unittest.TestCase):
    def test_verify_10_runs_the_exact_isolated_benchmark_without_a_skip_probe(self) -> None:
        spec = importlib.util.spec_from_file_location("verify_10_durability_latency_policy", VERIFY_10)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        verify = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = verify
        spec.loader.exec_module(verify)

        matches = [entry for entry in verify.GATES if entry[0] == "durable-decision-latency"]
        self.assertEqual(len(matches), 1)
        _name, tier, kind, payload, cwd, probe, _charter = matches[0]
        self.assertEqual((tier, kind, cwd), (1, "cmd", verify.REPO_ROOT))
        self.assertIsNone(probe, "the mandatory durability benchmark must never be environment-skipped")
        self.assertIn(str(verify.APP / "scripts" / "assert_ran.py"), payload)
        self.assertIn("--min 1 --kind cargo", payload)
        self.assertIn(f"--lib {BENCHMARK}", payload)
        self.assertIn("-- --ignored --exact --nocapture --test-threads=1", payload)

    def test_parallel_separation_does_not_weaken_the_latency_threshold(self) -> None:
        source = DB_TESTS.read_text(encoding="utf-8")
        benchmark_at = source.index(f"fn {BENCHMARK.rsplit('::', 1)[1]}()")
        benchmark_body = source[benchmark_at : source.index("\n}", benchmark_at) + 2]
        attribute_window = source[max(0, benchmark_at - 240) : benchmark_at]
        self.assertIn("#[ignore =", attribute_window)
        self.assertIn("per_decision < 0.25", benchmark_body)


if __name__ == "__main__":
    unittest.main()
