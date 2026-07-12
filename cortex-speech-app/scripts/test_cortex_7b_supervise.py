#!/usr/bin/env python3
"""Regression gate for the champion server's graceful-degradation supervisor (scale resilience).

A single worker death must NOT take the fleet down — the parent keeps serving on the survivors and
only exits once EVERY worker is gone. Verified without real fork()/os.wait() (Linux-only) by
injecting the reap + exit hooks. Runs on any OS."""
import importlib.util
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "cortex_7b_server_sup", Path(__file__).parent / "cortex_7b_server.py"
)
# Import ONLY the supervisor without running the heavy model-loading main(): the module top level does
# no torch/CUDA work (that lives inside worker()), so import is cheap and safe.
_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_mod)
supervise_workers = _mod.supervise_workers


def test_reaps_every_worker_before_exiting():
    """3 replicas, deaths one at a time: the supervisor must reap ALL THREE (degrading through each)
    and exit exactly once, only after none remain. If it exited early on the first death, reap_count
    would be 1 — the whole point of the fix."""
    deaths = iter([(101, 0), (102, 0), (103, 0)])
    reap_count = [0]
    exits = []

    def reap():
        reap_count[0] += 1
        return next(deaths)

    supervise_workers({101, 102, 103}, 3, reap=reap, exit_fn=lambda c: exits.append(c))
    assert reap_count[0] == 3, f"must reap ALL 3 workers before exiting (graceful degradation); got {reap_count[0]}"
    assert exits == [1], f"must exit exactly once with code 1, after all workers gone; got {exits}"


def test_single_replica_exits_on_its_death():
    """A 1-replica fleet has nothing to degrade to, so its death IS the last worker -> exit at once."""
    deaths = iter([(200, 0)])
    exits = []
    supervise_workers({200}, 1, reap=lambda: next(deaths), exit_fn=lambda c: exits.append(c))
    assert exits == [1], f"single replica must exit on its own death; got {exits}"


if __name__ == "__main__":
    test_reaps_every_worker_before_exiting()
    test_single_replica_exits_on_its_death()
    print("PASS: champion server graceful-degradation supervisor")
