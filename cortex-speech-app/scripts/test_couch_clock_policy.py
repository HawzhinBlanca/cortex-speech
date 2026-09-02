#!/usr/bin/env python3
"""Lease time in the couch is read from the state's clock, and tests age a lease by moving it FORWARD.

MEASURED 2026-08-2x through 2026-09-02: five `couch::tests` built an "expired" grant time with
`Instant::now().checked_sub(LEASE_TTL)`. A monotonic `Instant` cannot precede boot, so for the first
~16 minutes after every Windows restart those tests panicked ("a monotonic clock at least TTL old")
and read as regressions of whatever diff was under test. This workstation restarts several times a
day. `CouchState::clock_skew` (always zero in production) is added by `CouchState::now()`; every
production lease site reads that clock, and a test ages a lease by setting the skew.

Three pins, all greppable, all bite when the shape returns:
1. no test may subtract the TTL from the monotonic clock;
2. production lease logic in `couch/decisions.rs` never reads `Instant::now()` directly;
3. the skew field and the `now()` accessor exist on `CouchState`.
"""

from __future__ import annotations

import re
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src-tauri" / "src"
COUCH = SRC / "couch.rs"
DECISIONS = SRC / "couch" / "decisions.rs"

SUBTRACTS_FROM_BOOT = re.compile(r"checked_sub\(\s*LEASE_TTL|Instant::now\(\)\s*-\s*\(?\s*LEASE_TTL")


TEST_MODULE = "\n#[cfg(test)]\nmod tests {"


def _production_prefix(source: str) -> str:
    """Everything before the test MODULE: the part that ships.

    Cut at `#[cfg(test)]\\nmod tests {`, never at the first `#[cfg(test)]` — decisions.rs carries
    inline `#[cfg(test)]` wrappers hundreds of lines before its lease logic, and a cut there would
    make every assertion below vacuous.
    """
    marker = source.find(TEST_MODULE)
    assert marker > 0, "no `#[cfg(test)] mod tests` marker — this gate would pass vacuously"
    return source[:marker]


def _is_comment(line: str) -> bool:
    """Comments may describe the forbidden pattern (the proof test and the field doc both do)."""
    return line.lstrip().startswith("//")


def test_no_test_subtracts_the_lease_ttl_from_the_monotonic_clock() -> None:
    offenders: list[str] = []
    for path in (COUCH, DECISIONS, QUEUE_AUDIO):
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if SUBTRACTS_FROM_BOOT.search(line) and not _is_comment(line):
                offenders.append(f"{path.relative_to(SRC)}:{number}: {line.strip()[:100]}")
    assert not offenders, (
        "these age a lease by subtracting from Instant::now(), which panics within LEASE_TTL of boot; "
        "set `clock_skew` on the state instead:\n" + "\n".join(f"- {o}" for o in offenders)
    )


QUEUE_AUDIO = SRC / "couch" / "queue_audio.rs"


def test_production_lease_logic_reads_the_state_clock() -> None:
    """Both files that hold lease and playback timing: decisions.rs and queue_audio.rs.

    The first pass covered decisions.rs alone and three lease tests stayed red — queue_audio.rs
    held eight more reads (queue, audio authorization, playback attempts). Any file with lease
    logic that reads the monotonic clock directly is invisible to the state's skew.
    """
    direct: list[str] = []
    for path in (DECISIONS, QUEUE_AUDIO):
        production = _production_prefix(path.read_text(encoding="utf-8"))
        direct.extend(
            f"{path.relative_to(SRC)}:{number}: {line.strip()[:100]}"
            for number, line in enumerate(production.splitlines(), start=1)
            if "Instant::now()" in line
        )
        assert ".now()" in production, f"{path.name}: no site reads the state clock at all — the accessor is unused"
    assert not direct, (
        "production lease logic must read the state clock (`guard.now()` / `lock_state(state).now()`) "
        "so tests can age a lease without touching the monotonic clock:\n" + "\n".join(f"- {d}" for d in direct)
    )


def test_the_state_carries_a_skew_and_an_accessor_that_adds_it() -> None:
    production = _production_prefix(COUCH.read_text(encoding="utf-8"))
    assert re.search(r"^\s*clock_skew:\s*Duration,", production, re.M), "CouchState.clock_skew is missing"
    accessor = re.search(r"fn now\(&self\) -> Instant \{\s*Instant::now\(\)\s*\+\s*self\.clock_skew\s*\}", production)
    assert accessor, "CouchState::now() must be exactly `Instant::now() + self.clock_skew`"


def main() -> None:
    test_no_test_subtracts_the_lease_ttl_from_the_monotonic_clock()
    test_production_lease_logic_reads_the_state_clock()
    test_the_state_carries_a_skew_and_an_accessor_that_adds_it()
    print("couch clock policy passed")


if __name__ == "__main__":
    main()
