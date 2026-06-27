import json
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
TAURI_CONFIG = REPO_ROOT / "src-tauri" / "tauri.conf.json"
DEFAULT_CAPABILITY = REPO_ROOT / "src-tauri" / "capabilities" / "default.json"

EXPECTED_CAPABILITY_PERMISSIONS = [
    "core:default",
    "dialog:default",
    "dialog:allow-open",
    "dialog:allow-save",
]
# The media cache lives under the app DATA dir (`$DATA/cortex-speech/media-cache`), NOT `$APPDATA` —
# the audio-playback fix corrected the asset scope to match where register_media_asset actually writes
# clips, so clip-bounded playback resolves. Still tightly scoped to the media-cache subtree (the
# forbidden-prefix check below rejects any broadening to `$APPDATA/**`, `$HOME`, etc.).
EXPECTED_ASSET_SCOPE = ["$DATA/cortex-speech/media-cache/**"]


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


def test_asset_protocol_stays_media_cache_scoped() -> None:
    config = read_json(TAURI_CONFIG)
    asset_protocol = config["app"]["security"]["assetProtocol"]

    if asset_protocol.get("enable") is not True:
        raise AssertionError("asset protocol must remain explicitly enabled for media-cache playback")
    if asset_protocol.get("scope") != EXPECTED_ASSET_SCOPE:
        raise AssertionError(f"asset protocol scope changed: {asset_protocol.get('scope')}")


def test_updater_is_not_silently_enabled() -> None:
    config = read_json(TAURI_CONFIG)
    serialized = json.dumps(config, sort_keys=True)

    assert_absent(serialized, '"updater"', TAURI_CONFIG.name)
    assert_absent(serialized, "createUpdaterArtifacts", TAURI_CONFIG.name)
    if config.get("plugins"):
        raise AssertionError("Tauri plugins must stay absent unless explicitly reviewed")


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
    # mirrors the existing http://asset.localhost entry already approved in media-src below — a
    # specific localhost origin, NOT a broad http: wildcard (script-src still forbids http:).
    if directives.get("connect-src") != ["'self'", "ipc:", "https://ipc.localhost", "http://ipc.localhost"]:
        raise AssertionError(f"CSP connect-src changed: {directives.get('connect-src')}")
    if directives.get("img-src") != ["'self'", "asset:", "https://asset.localhost"]:
        raise AssertionError(f"CSP img-src changed: {directives.get('img-src')}")
    if directives.get("media-src") != [
        "'self'",
        "blob:",
        "mediastream:",
        "asset:",
        "https://asset.localhost",
        "http://asset.localhost",
    ]:
        raise AssertionError(f"CSP media-src changed: {directives.get('media-src')}")
    if directives.get("worker-src") != ["'self'", "blob:"]:
        raise AssertionError(f"CSP worker-src changed: {directives.get('worker-src')}")


def main() -> None:
    test_default_capability_stays_minimal()
    test_asset_protocol_stays_media_cache_scoped()
    test_updater_is_not_silently_enabled()
    test_csp_blocks_browser_escape_hatches()
    print("tauri security policy regression passed")


if __name__ == "__main__":
    main()
