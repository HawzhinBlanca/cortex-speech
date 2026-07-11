import shutil
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPTS_DIR = REPO_ROOT / "scripts"


def relative(path: Path) -> str:
    return str(path.relative_to(REPO_ROOT))


def run(command: list[str]) -> None:
    print(f"\n==> {' '.join(command)}", flush=True)
    subprocess.run(command, cwd=REPO_ROOT, check=True)


def remove_pycache() -> None:
    cache = SCRIPTS_DIR / "__pycache__"
    if not cache.exists():
        return

    scripts_root = SCRIPTS_DIR.resolve(strict=True)
    cache_root = cache.resolve(strict=True)
    try:
        cache_root.relative_to(scripts_root)
    except ValueError as exc:
        raise RuntimeError(f"Refusing to remove cache outside scripts: {cache_root}") from exc

    shutil.rmtree(cache_root)
    print(f"\nRemoved {cache_root}", flush=True)


def main() -> int:
    python_files = sorted(SCRIPTS_DIR.glob("*.py"), key=lambda path: path.name.lower())
    test_files = [path for path in python_files if path.name.startswith("test_")]

    if not test_files:
        raise RuntimeError(f"No Python policy tests found under {SCRIPTS_DIR}")

    try:
        for test in test_files:
            run([sys.executable, relative(test)])

        run([sys.executable, "-m", "py_compile", *[relative(path) for path in python_files]])
    finally:
        remove_pycache()

    # The count is printed so ledger entries can quote a REAL number — a hardcoded "N regressions
    # passed" claim drifts the moment a policy test is added (true-10 sweep 2026-07-11 found the
    # ledger repeating a stale "23" while 29 test scripts actually run).
    print(f"\nPython policy regressions finished: {len(test_files)} policy test scripts passed.", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
