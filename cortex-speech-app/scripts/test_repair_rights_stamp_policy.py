"""The legacy repair tool may never stamp owner rights across audio the owner did not supply.

Canon (2026-08-14) is precise about the split: all OWNER-supplied audio is `owner-full-rights`,
unrestricted, clearance CLOSED — while FLEURS is the frozen eval set and Common Voice carries its own
licence. `repair_unfinalized_reviews.py --stamp-like` exists to name the owner's recordings, but a
pattern of nothing but SQL wildcards (`%`, `_%`, `%%`) matches every `audio_path` in the library, so
it would stamp those third-party corpora as the owner's in one unreviewable UPDATE. This pins the
refusal, and that it happens before any database work.
"""

from __future__ import annotations

import argparse
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import repair_unfinalized_reviews as repair  # noqa: E402


def _args(db: Path, pattern: str) -> argparse.Namespace:
    return argparse.Namespace(db=str(db), apply=True, stamp_like=[pattern])


def test_wildcard_only_stamp_patterns_are_refused_before_any_database_work() -> None:
    with tempfile.TemporaryDirectory() as raw:
        # A path that does not exist: if the guard fails to fire, the tool opens (and creates) it,
        # which is itself the proof that the refusal came too late.
        db = Path(raw) / "never-opened.db"
        for pattern in ("%", "%%", "_%", "  %  "):
            assert repair.run(_args(db, pattern)) == 2, f"{pattern!r} was accepted as an owner corpus"
        assert not db.exists(), "the refusal must precede opening the live database"


def test_a_real_path_fragment_is_still_accepted() -> None:
    with tempfile.TemporaryDirectory() as raw:
        db = Path(raw) / "empty.db"
        args = argparse.Namespace(db=str(db), apply=False, stamp_like=["%podcast-episode-%"])
        try:
            repair.run(args)
        except Exception as error:  # noqa: BLE001 - an empty schema fails LATER, not at the guard
            assert "no such table" in str(error), error
        assert db.exists(), "a named owner corpus must reach the database, not be refused"


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_") and callable(value)]
    for test in tests:
        test()
        print(f"  ok  {test.__name__}")
    print(f"REPAIR RIGHTS STAMP POLICY: {len(tests)} pins")
    return 0


if __name__ == "__main__":
    sys.exit(main())
