import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
MODELS_RS = REPO_ROOT / "src-tauri" / "src" / "models.rs"
RELEASE_DOCS = REPO_ROOT / "docs" / "RELEASE.md"


def models_source() -> str:
    return MODELS_RS.read_text(encoding="utf-8")


def release_docs() -> str:
    return RELEASE_DOCS.read_text(encoding="utf-8")


def rust_string_constants(suffix: str) -> dict[str, str]:
    pattern = re.compile(rf'pub const\s+([A-Z0-9_]+_{suffix})\s*:\s*&str\s*=\s*"([^"]*)";')
    return {name: value for name, value in pattern.findall(models_source())}


def model_info_blocks() -> list[str]:
    return re.findall(r"ModelInfo\s*\{(.*?)\n\s*\}", models_source(), flags=re.DOTALL)


def block_field(block: str, field: str) -> str:
    match = re.search(rf'{field}:\s*"([^"]*)"', block)
    if match:
        return match.group(1)
    match = re.search(rf"{field}:\s*([A-Z0-9_]+)", block)
    if match:
        return match.group(1)
    return ""


def assert_contains(text: str, expected: str, context: str) -> None:
    if expected not in text:
        raise AssertionError(f"{context} is missing: {expected}")


def first_network_get_after(source: str, start: int) -> int:
    candidates = [
        match.start()
        for pattern in (r"ureq::get", r"DOWNLOAD_AGENT\s*\.\s*get")
        if (match := re.search(pattern, source[start:], flags=re.DOTALL))
    ]
    return start + min(candidates) if candidates else -1


def test_non_empty_sha256_values_are_full_hex_digests() -> None:
    constants = rust_string_constants("SHA256")
    if not constants:
        raise AssertionError("No SHA256 constants found in models.rs")

    for name, value in constants.items():
        if value and not re.fullmatch(r"[0-9a-f]{64}", value):
            raise AssertionError(f"{name} must be a lowercase 64-character SHA256 digest")

    for block in model_info_blocks():
        sha256 = block_field(block, "sha256")
        if sha256 and not re.fullmatch(r"[0-9a-f]{64}", sha256):
            raise AssertionError(f"ModelInfo sha256 must be a lowercase 64-character digest: {sha256}")


def test_direct_model_urls_require_pinned_sha256() -> None:
    for block in model_info_blocks():
        name = block_field(block, "name")
        url = block_field(block, "url")
        sha256 = block_field(block, "sha256")
        if url and not sha256:
            raise AssertionError(f"{name} has a direct download URL but no pinned SHA256")


def test_unpinned_archive_models_are_not_downloadable() -> None:
    source = models_source()

    required_archive_guards = [
        ("OMNIASR_CTC_300M_DIR", "OMNIASR_CTC_300M_ARCHIVE_SHA256"),
        ("OMNIASR_CTC_1B_DIR", "OMNIASR_CTC_1B_ARCHIVE_SHA256"),
        # CAM++ and the denoiser are now single-file .onnx direct downloads (the old tar.bz2 URLs 404'd),
        # pinned by the model SHA (not an archive SHA). The unpinned-not-downloadable guarantee is
        # unchanged — model_download_supported still gates them on a non-empty pinned digest.
        ("CAMPP_DIR", "CAMPP_MODEL_SHA256"),
        ("DENOISER_DIR", "DENOISER_MODEL_SHA256"),
    ]

    for dir_const, sha_const in required_archive_guards:
        assert_contains(source, f"model.filename.starts_with({dir_const})", "model_download_supported")
        assert_contains(source, f"return !{sha_const}.is_empty();", "model_download_supported")

    assert_contains(source, "!model.url.is_empty() && !model.sha256.is_empty()", "model_download_supported")


def test_archive_downloads_preflight_and_verify_sha256() -> None:
    source = models_source()

    expected_pairs = [
        ("Meta OmniASR", "archive_sha256"),
        # Direct .onnx downloads: still preflight the pinned SHA -> network get -> verify SHA, in order.
        ("CAM++ model", "CAMPP_MODEL_SHA256"),
        ("AI Denoiser model", "DENOISER_MODEL_SHA256"),
    ]

    for label, sha_expr in expected_pairs:
        ensure_index = source.find(f"ensure_pinned_sha256", source.find(sha_expr))
        network_index = first_network_get_after(source, ensure_index)
        verify_index = source.find("verify_sha256", network_index)

        if ensure_index < 0:
            raise AssertionError(f"{label} download path must preflight pinned SHA256 before network IO")
        if network_index < 0:
            raise AssertionError(f"{label} download path must make explicit network requests")
        if verify_index < 0:
            raise AssertionError(f"{label} download path must verify SHA256 after writing the archive")
        if not ensure_index < network_index < verify_index:
            raise AssertionError(f"{label} must preflight SHA256, download, then verify SHA256 in that order")
        assert_contains(source, sha_expr, f"{label} SHA256 expression")

    assert_contains(
        source,
        'format!("Missing pinned SHA256 for {label}; refusing to download unverifiable artifact")',
        "missing_pinned_sha256_error",
    )


def test_release_docs_keep_empty_hashes_as_blockers() -> None:
    docs = release_docs()

    assert_contains(
        docs,
        "Downloaded model archives have pinned SHA256 values before release. Empty hash constants are release blockers.",
        "docs/RELEASE.md",
    )


def main() -> None:
    test_non_empty_sha256_values_are_full_hex_digests()
    test_direct_model_urls_require_pinned_sha256()
    test_unpinned_archive_models_are_not_downloadable()
    test_archive_downloads_preflight_and_verify_sha256()
    test_release_docs_keep_empty_hashes_as_blockers()
    print("model provenance policy regression passed")


if __name__ == "__main__":
    main()
