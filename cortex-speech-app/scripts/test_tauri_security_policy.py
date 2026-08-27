import json
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
TAURI_CONFIG = REPO_ROOT / "src-tauri" / "tauri.conf.json"
TAURI_WINDOWS_CONFIG = REPO_ROOT / "src-tauri" / "tauri.windows.conf.json"
DEFAULT_CAPABILITY = REPO_ROOT / "src-tauri" / "capabilities" / "default.json"
TAURI_CARGO = REPO_ROOT / "src-tauri" / "Cargo.toml"
LIB_RS = REPO_ROOT / "src-tauri" / "src" / "lib.rs"
MEDIA_RS = REPO_ROOT / "src-tauri" / "src" / "media.rs"

EXPECTED_CAPABILITY_PERMISSIONS = [
    "core:default",
    # 2026-07-14: the close-button fix needed these two. Without allow-destroy, window.destroy() in
    # the close-request handler was silently DENIED by the permission system — every close click was
    # intercepted (to flush the autosave) and then went nowhere, so the app could never actually quit.
    "core:window:allow-close",
    "core:window:allow-destroy",
    "dialog:default",
    "dialog:allow-open",
    "dialog:allow-save",
]
EXPECTED_WINDOWS_RESOURCES = [
    "models/silero_vad_v4.onnx",
    "models/onnxruntime.dll/onnxruntime.dll",
    "models/onnxruntime.dll/onnxruntime_providers_shared.dll",
    "../scripts/cortex_7b_server.py",
    "../scripts/cortex_7b_client.py",
]


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def csp_directives(csp: str) -> dict[str, list[str]]:
    directives: dict[str, list[str]] = {}
    for raw_directive in csp.split(";"):
        parts = raw_directive.strip().split()
        if not parts:
            continue
        directives[parts[0]] = parts[1:]
    return directives


def assert_absent(text: str, forbidden: str, context: str) -> None:
    if forbidden in text:
        raise AssertionError(f"{context} must not contain {forbidden}")


def test_default_capability_stays_minimal() -> None:
    capability = read_json(DEFAULT_CAPABILITY)
    permissions = capability.get("permissions", [])

    if capability.get("windows") != ["main"]:
        raise AssertionError("default capability must only target the main window")
    if permissions != EXPECTED_CAPABILITY_PERMISSIONS:
        raise AssertionError(f"default capability permissions changed: {permissions}")

    serialized = json.dumps(capability, sort_keys=True)
    for forbidden in ["shell:", "fs:", "http:", "updater:", "$APPDATA/**", "$HOME", "$DESKTOP"]:
        assert_absent(serialized, forbidden, DEFAULT_CAPABILITY.name)


def test_renderer_has_no_asset_protocol_filesystem_scope() -> None:
    config = read_json(TAURI_CONFIG)
    asset_protocol = config["app"]["security"]["assetProtocol"]
    if asset_protocol != {"enable": False, "scope": []}:
        raise AssertionError(f"renderer filesystem asset protocol must stay disabled: {asset_protocol}")
    cargo = TAURI_CARGO.read_text(encoding="utf-8")
    if 'features = ["protocol-asset"]' in cargo:
        raise AssertionError("Tauri's path-bearing asset protocol feature must stay disabled")
    library = LIB_RS.read_text(encoding="utf-8")
    if "asset_protocol_scope().allow_directory" in library:
        raise AssertionError("the renderer must never regain a runtime media-cache directory grant")
    if '.register_asynchronous_uri_scheme_protocol(\n            crate::media::MEDIA_PROTOCOL_SCHEME' not in library:
        raise AssertionError("opaque cortex-media protocol registration is missing")
    for required in [
        "try_acquire_media_protocol_worker()",
        "tauri::async_runtime::spawn_blocking",
        "media_protocol_busy_response()",
    ]:
        if required not in library:
            raise AssertionError(f"opaque media protocol lost bounded async admission: {required}")
    media = MEDIA_RS.read_text(encoding="utf-8")
    for required in [
        "const MAX_MEDIA_PROTOCOL_WORKERS: usize = 8;",
        "const MAX_MEDIA_PROTOCOL_RANGE_BYTES: u64 = 1000 * 1024;",
        "#[serde(skip)]",
        "#[specta(skip)]",
    ]:
        if required not in media:
            raise AssertionError(f"opaque media security contract drifted: {required}")
    for source_root in [REPO_ROOT / "src", REPO_ROOT / "e2e"]:
        for path in sorted(source_root.rglob("*")):
            if path.suffix not in {".ts", ".svelte"}:
                continue
            source = path.read_text(encoding="utf-8")
            for forbidden in ["convertFileSrc", "desktopAssetUrl", "asset://"]:
                if forbidden in source:
                    relative = path.relative_to(REPO_ROOT).as_posix()
                    raise AssertionError(f"renderer filesystem URL conversion returned in {relative}: {forbidden}")


def test_updater_is_not_silently_enabled() -> None:
    config = read_json(TAURI_CONFIG)
    serialized = json.dumps(config, sort_keys=True)

    assert_absent(serialized, '"updater"', TAURI_CONFIG.name)
    if config.get("bundle", {}).get("createUpdaterArtifacts") is not False:
        raise AssertionError(
            "updater artifacts must remain explicitly false until the signed opt-in updater contract is complete"
        )
    if config.get("plugins"):
        raise AssertionError("Tauri plugins must stay absent unless explicitly reviewed")


def test_windows_installer_source_is_offline_exact_and_has_no_destructive_hooks() -> None:
    config = read_json(TAURI_CONFIG)
    windows_override = read_json(TAURI_WINDOWS_CONFIG)
    bundle = config.get("bundle", {})
    windows = bundle.get("windows", {})

    if bundle.get("targets") != ["msi", "nsis"]:
        raise AssertionError("the product installer contract must remain exactly Windows MSI + NSIS")
    if windows.get("webviewInstallMode") != {"type": "offlineInstaller"}:
        raise AssertionError("Windows installers must embed the offline WebView2 installer")
    if windows.get("allowDowngrades") is not False:
        raise AssertionError(
            "Windows installers must reject arbitrary downgrades; rollback requires a separately proved compatible binary"
        )

    # Tauri's stock MSI/NSIS uninstallers remove their installation tree, not Cortex's separate
    # %APPDATA% data directory.  Custom templates/hooks are the local source surface that could
    # silently add destructive user-data cleanup, so keep both installer definitions exact.  A
    # future explicit delete-data flow needs its own confirmation UX and clean-VM evidence. This
    # source policy does not replace an install/write-hash/uninstall VM drill.
    if windows.get("wix") != {"language": "en-US"}:
        raise AssertionError(
            "custom WiX templates/fragments/actions are forbidden without uninstall-preservation proof"
        )
    if windows.get("nsis") != {
        "installMode": "perMachine",
        "installerIcon": "icons/icon.ico",
        # Only English is configured. A selector with one choice adds an empty decision to both the
        # installer and uninstaller; turn it on only with an actually supported second NSIS locale.
        "displayLanguageSelector": False,
    }:
        raise AssertionError("custom NSIS hooks or installer behavior require uninstall-preservation proof")

    resources = bundle.get("resources")
    if resources != EXPECTED_WINDOWS_RESOURCES:
        raise AssertionError(f"Windows bundled support assets changed: {resources}")
    if windows_override.get("bundle", {}).get("resources") != EXPECTED_WINDOWS_RESOURCES:
        raise AssertionError("tauri.windows.conf.json must preserve the exact audited resource inventory")

    source_root = (REPO_ROOT / "src-tauri").resolve(strict=True)
    for relative in EXPECTED_WINDOWS_RESOURCES:
        declared = source_root / relative
        if declared.is_symlink():
            raise AssertionError(f"bundled support asset must not be a symlink: {relative}")
        resolved = declared.resolve(strict=True)
        try:
            resolved.relative_to(REPO_ROOT.resolve(strict=True))
        except ValueError as error:
            raise AssertionError(f"bundled support asset escapes the application source tree: {relative}") from error
        if not resolved.is_file() or resolved.stat().st_size <= 0:
            raise AssertionError(f"bundled support asset is missing or empty: {relative}")


def test_csp_blocks_browser_escape_hatches() -> None:
    config = read_json(TAURI_CONFIG)
    csp = config["app"]["security"]["csp"]
    directives = csp_directives(csp)

    expected_exact = {
        "default-src": ["'self'"],
        "script-src": ["'self'"],
        "object-src": ["'none'"],
        "base-uri": ["'self'"],
        "frame-ancestors": ["'none'"],
        "form-action": ["'self'"],
        "font-src": ["'self'"],
    }
    for name, expected in expected_exact.items():
        if directives.get(name) != expected:
            raise AssertionError(f"CSP {name} changed: {directives.get(name)}")

    for forbidden in ["'unsafe-eval'", "http:", "https:", "data:"]:
        if forbidden in directives["script-src"]:
            raise AssertionError(f"CSP script-src must not allow {forbidden}")

    # http://ipc.localhost is the Windows WebView2 origin for Tauri's IPC/event channel; without
    # it the event-listen connect is CSP-blocked and import/refresh events can be dropped. This
    # Like the exact cortex-media origin below, this is a specific localhost origin, NOT a broad
    # http: wildcard (script-src still forbids http:).
    if directives.get("connect-src") != ["'self'", "ipc:", "https://ipc.localhost", "http://ipc.localhost"]:
        raise AssertionError(f"CSP connect-src changed: {directives.get('connect-src')}")
    if directives.get("img-src") != ["'self'"]:
        raise AssertionError(f"CSP img-src changed: {directives.get('img-src')}")
    if directives.get("media-src") != [
        "'self'",
        "blob:",
        "mediastream:",
        "cortex-media:",
        "http://cortex-media.localhost",
    ]:
        raise AssertionError(f"CSP media-src changed: {directives.get('media-src')}")
    if directives.get("worker-src") != ["'self'", "blob:"]:
        raise AssertionError(f"CSP worker-src changed: {directives.get('worker-src')}")


def main() -> None:
    test_default_capability_stays_minimal()
    test_renderer_has_no_asset_protocol_filesystem_scope()
    test_updater_is_not_silently_enabled()
    test_windows_installer_source_is_offline_exact_and_has_no_destructive_hooks()
    test_csp_blocks_browser_escape_hatches()
    print("tauri security policy regression passed")


if __name__ == "__main__":
    main()
