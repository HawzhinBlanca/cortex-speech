import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
BIN_DIR = REPO_ROOT / "src-tauri" / "src" / "bin"

FORBIDDEN_PATTERNS = [
    (re.compile(r"\.unwrap\s*\("), "use ? or an explicit match instead of unwrap"),
    (re.compile(r"\.expect\s*\("), "return a contextual error instead of expect"),
    (re.compile(r"\bpanic!\s*\("), "return an error instead of panicking"),
    (re.compile(r"std::process::exit\s*\("), "return an error from main instead of exiting inside processing"),
]


def test_batch_cli_entrypoints_do_not_panic_on_runtime_failures() -> None:
    offenders: list[str] = []
    for path in sorted(BIN_DIR.glob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for line_no, line in enumerate(text.splitlines(), start=1):
            for pattern, reason in FORBIDDEN_PATTERNS:
                if pattern.search(line):
                    offenders.append(f"{path.relative_to(REPO_ROOT)}:{line_no}: {reason}: {line.strip()}")
    if offenders:
        formatted = "\n".join(f"- {entry}" for entry in offenders)
        raise AssertionError(f"Batch CLI binaries must report recoverable errors without panics:\n{formatted}")


def main() -> None:
    test_batch_cli_entrypoints_do_not_panic_on_runtime_failures()
    print("batch CLI policy regression passed")


if __name__ == "__main__":
    main()
