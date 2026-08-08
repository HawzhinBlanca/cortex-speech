"""The release checksum manifest must be byte-identical no matter which OS wrote it.

The ordering assertions below are not cosmetic. `sorted(Path, ...)` compares case-INSENSITIVELY on
Windows and case-SENSITIVELY on POSIX, so the generator emitted two different manifests for the same
bundle depending on the runner — while its docstring said "deterministic". That is the whole basis of
a provenance claim: a manifest you cannot reproduce is a manifest you cannot check.

It hid for a long time because the only assertion that could see it happened to match the Windows
order, and this repo's sweeps run on Windows. It surfaced as `Linux Build Smoke` and `macOS Build
Smoke` failing on every run for days. `test_case_ordering_is_platform_independent` is the guard that
would have caught it ON WINDOWS TOO — it pins names whose two orderings disagree, so a revert to
Path-object sorting fails everywhere rather than on the platform nobody runs locally.
"""

import hashlib
import tempfile
from pathlib import Path

from generate_release_checksums import MANIFEST_NAME, generate


def test_case_ordering_is_platform_independent() -> None:
    """Pin an order the two sort rules DISAGREE about, so neither platform can quietly drift.

    'Zebra.bin' sorts before 'apple.bin' by byte value (Z=0x5A < a=0x61) and after it when folded to
    lowercase. Asserting the byte order therefore fails under Path-object sorting on Windows, which is
    exactly the regression that shipped.
    """
    with tempfile.TemporaryDirectory() as raw_dir:
        bundle = Path(raw_dir)
        (bundle / "Zebra.bin").write_bytes(b"z")
        (bundle / "apple.bin").write_bytes(b"a")
        (bundle / "nested").mkdir()
        (bundle / "nested" / "Inner.bin").write_bytes(b"i")

        names = [line.split(" *", 1)[1] for line in generate(bundle).read_text(encoding="ascii").splitlines()]
        assert names == ["Zebra.bin", "apple.bin", "nested/Inner.bin"], names
        # And prove the assertion above is load-bearing: case-folding really does reorder it.
        assert sorted(names, key=str.lower) != names


def main() -> None:
    test_case_ordering_is_platform_independent()

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
            # The prior manifest is an artifact like any other, and "SHA256SUMS" sorts BEFORE
            # "app.tar.gz" by byte value. This used to expect it last, which is what a case-insensitive
            # Windows Path sort produced — the assertion encoded the bug rather than catching it.
            f"{hashlib.sha256(manifest.read_bytes()).hexdigest()} *{MANIFEST_NAME}",
            *expected,
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
