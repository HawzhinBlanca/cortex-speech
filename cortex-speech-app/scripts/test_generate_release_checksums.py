import hashlib
import tempfile
from pathlib import Path

from generate_release_checksums import MANIFEST_NAME, generate


def main() -> None:
    with tempfile.TemporaryDirectory() as raw_dir:
        bundle = Path(raw_dir)
        (bundle / "msi").mkdir()
        (bundle / "msi" / "cortex.msi").write_bytes(b"signed-installer")
        (bundle / "app.tar.gz").write_bytes(b"archive")

        manifest = generate(bundle)
        expected = [
            f"{hashlib.sha256(b'archive').hexdigest()} *app.tar.gz",
            f"{hashlib.sha256(b'signed-installer').hexdigest()} *msi/cortex.msi",
        ]
        assert manifest.name == MANIFEST_NAME
        assert manifest.read_text(encoding="ascii").splitlines() == expected

        # Regeneration is stable and never hashes the old manifest into itself.
        assert generate(bundle).read_text(encoding="ascii").splitlines() == expected

        platform_manifest = generate(bundle, "SHA256SUMS-windows-latest")
        assert platform_manifest.name == "SHA256SUMS-windows-latest"
        assert platform_manifest.read_text(encoding="ascii").splitlines() == [
            *expected,
            f"{hashlib.sha256(manifest.read_bytes()).hexdigest()} *{MANIFEST_NAME}",
        ]

    with tempfile.TemporaryDirectory() as raw_dir:
        try:
            generate(Path(raw_dir))
        except ValueError as error:
            assert "contains no artifacts" in str(error)
        else:
            raise AssertionError("an empty release bundle must fail closed")

    with tempfile.TemporaryDirectory() as raw_dir:
        bundle = Path(raw_dir)
        (bundle / "artifact").write_bytes(b"x")
        try:
            generate(bundle, "../outside")
        except ValueError as error:
            assert "plain file name" in str(error)
        else:
            raise AssertionError("manifest output must stay inside the bundle")

    print("release checksum regression passed")


if __name__ == "__main__":
    main()
