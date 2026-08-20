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


def test_a_dead_worker_is_respawned_on_its_own_device_with_a_bounded_budget():
    """2026-08-20 external review: a dead worker used to stay dead for the whole session. Now each
    death respawns on the SAME device until that device's budget is spent; a crash-looping card
    stops burning forks and the fleet degrades loudly instead."""
    deaths = iter([(101, 9), (102, 7), (201, 9), (202, 7)])
    spawned = []
    next_pid = [200]

    def respawn(index):
        spawned.append(index)
        next_pid[0] += 1
        return next_pid[0]

    exits = []
    slept = []
    supervise_workers(
        {101: 0, 102: 1},
        2,
        reap=lambda: next(deaths),
        exit_fn=lambda c: exits.append(c),
        respawn=respawn,
        respawn_budget=1,
        backoff=slept.append,
    )
    assert spawned == [0, 1], f"each device respawns once on its own index; got {spawned}"
    assert slept == [5, 5], "every respawn waits out the backoff"
    assert exits == [1], f"exit only after both budgets are spent and every worker is gone; got {exits}"


def test_no_respawn_after_a_deliberate_shutdown():
    """SIGTERM fan-out sets `stopping`; reaping those deaths must not fork replacements."""
    import threading

    stopping = threading.Event()
    stopping.set()
    deaths = iter([(101, 0), (102, 0)])
    spawned = []
    exits = []
    supervise_workers(
        {101: 0, 102: 1},
        2,
        reap=lambda: next(deaths),
        exit_fn=lambda c: exits.append(c),
        respawn=lambda i: spawned.append(i) or 999,
        stopping=stopping,
        backoff=lambda s: None,
    )
    assert spawned == [], "a shutdown must reap, never respawn"
    assert exits == [1]


def test_a_shutdown_that_arrives_during_the_backoff_still_cancels_the_respawn():
    """The race the test above cannot see: `stopping` is set WHILE the loop sleeps.

    Review 2026-08-20. The loop sampled `stopping` when it reaped, then slept 5 s before forking.
    A SIGTERM landing inside that window had already SIGTERMed the generation it could see, so the
    worker forked after the sleep was one nothing would ever signal — it outlives the parent holding
    the listen port and ~19 GB of VRAM, and blocks the next server start. Fails without the
    post-backoff re-check: `spawned` comes back as [0]."""
    import threading

    stopping = threading.Event()
    deaths = iter([(101, 9), (102, 0)])
    spawned = []
    exits = []
    supervise_workers(
        {101: 0, 102: 1},
        2,
        reap=lambda: next(deaths),
        exit_fn=lambda c: exits.append(c),
        respawn=lambda i: spawned.append(i) or 999,
        stopping=stopping,
        backoff=lambda s: stopping.set(),  # the signal lands mid-wait
    )
    assert spawned == [], "a shutdown during the backoff must cancel the respawn, not race it"
    assert exits == [1]


if __name__ == "__main__":
    test_reaps_every_worker_before_exiting()
    test_single_replica_exits_on_its_death()
    test_a_dead_worker_is_respawned_on_its_own_device_with_a_bounded_budget()
    test_no_respawn_after_a_deliberate_shutdown()
    test_a_shutdown_that_arrives_during_the_backoff_still_cancels_the_respawn()
    print("PASS: champion server graceful-degradation supervisor (4 tests)")
