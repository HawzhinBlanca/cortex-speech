"""Write a deterministic SHA-256 manifest for every release artifact in a bundle tree."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path


MANIFEST_NAME = "SHA256SUMS"
READ_CHUNK_BYTES = 1024 * 1024


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(READ_CHUNK_BYTES):
            digest.update(chunk)
    return digest.hexdigest()


def generate(bundle_dir: Path, manifest_name: str = MANIFEST_NAME) -> Path:
    bundle_dir = bundle_dir.resolve()
    if not bundle_dir.is_dir():
        raise ValueError(f"release bundle directory does not exist: {bundle_dir}")
    if not manifest_name or Path(manifest_name).name != manifest_name:
        raise ValueError("checksum manifest name must be a plain file name")

    # Sort by the RELATIVE POSIX STRING — the exact text each line carries — not by the Path object.
    #
    # `sorted(Path, ...)` compares `PurePath._str_normcase`, which is `str(self).lower()` on Windows and
    # `str(self)` on POSIX. So the same bundle produced two different line orders depending on which OS
    # ran this, and the module's first word is "deterministic". Measured 2026-08-08 on
    # ["app.tar.gz", "msi/cortex.msi", "SHA256SUMS"]:
    #     Windows -> ['app.tar.gz', 'msi/cortex.msi', 'SHA256SUMS']
    #     Linux   -> ['SHA256SUMS', 'app.tar.gz', 'msi/cortex.msi']
    #
    # No released manifest was wrong: release.yml runs this under `if: runner.os == 'Windows'` only. But
    # a manifest whose bytes depend on the builder's OS cannot back a reproducibility or provenance
    # claim, and this is what reddened `Linux Build Smoke` and `macOS Build Smoke` on every run.
    #
    # Keying on the written string also removes the separator question: `as_posix()` already normalises
    # `msi\cortex.msi` to `msi/cortex.msi`, so ordering and content now agree by construction.
    artifacts = sorted(
        (path for path in bundle_dir.rglob("*") if path.is_file() and path.name != manifest_name),
        key=lambda path: path.relative_to(bundle_dir).as_posix(),
    )
    if not artifacts:
        raise ValueError(f"release bundle contains no artifacts: {bundle_dir}")

    lines: list[str] = []
    for artifact in artifacts:
        digest = sha256_file(artifact)
        relative = artifact.relative_to(bundle_dir).as_posix()
        lines.append(f"{digest} *{relative}")

    manifest = bundle_dir / manifest_name
    manifest.write_text("\n".join(lines) + "\n", encoding="ascii", newline="\n")
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle_dir", type=Path)
    parser.add_argument("--output", default=MANIFEST_NAME, help="manifest file name inside the bundle")
    args = parser.parse_args()
    manifest = generate(args.bundle_dir, args.output)
    print(f"wrote {manifest}")


if __name__ == "__main__":
    main()
