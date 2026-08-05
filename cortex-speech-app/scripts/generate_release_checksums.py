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

    artifacts = sorted(
        path for path in bundle_dir.rglob("*") if path.is_file() and path.name != manifest_name
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
