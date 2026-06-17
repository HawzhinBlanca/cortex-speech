import json
import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
DEV_SERVER_PORT = 1420
DEV_SERVER_URL = f"http://localhost:{DEV_SERVER_PORT}"


def read(path: str) -> str:
    return (REPO_ROOT / path).read_text(encoding="utf-8")


def package_json() -> dict:
    return json.loads(read("package.json"))


def tauri_config() -> dict:
    return json.loads(read("src-tauri/tauri.conf.json"))


def assert_contains(text: str, expected: str, context: str) -> None:
    if expected not in text:
        raise AssertionError(f"{context} is missing: {expected}")


def assert_regex(text: str, pattern: str, context: str) -> None:
    if not re.search(pattern, text, flags=re.MULTILINE | re.DOTALL):
        raise AssertionError(f"{context} does not match required pattern: {pattern}")


def test_vite_dev_server_uses_fixed_strict_port() -> None:
    vite = read("vite.config.ts")

    assert_contains(vite, f"port: {DEV_SERVER_PORT}", "vite.config.ts")
    assert_contains(vite, "strictPort: true", "vite.config.ts")
    assert_regex(
        vite,
        r"server:\s*\{(?=.*\bport:\s*1420\b)(?=.*\bstrictPort:\s*true\b)",
        "vite.config.ts server block",
    )


def test_playwright_targets_the_fixed_vite_server() -> None:
    playwright = read("playwright.config.ts")

    assert_contains(playwright, f"baseURL: '{DEV_SERVER_URL}'", "playwright.config.ts")
    assert_contains(playwright, f"url: '{DEV_SERVER_URL}'", "playwright.config.ts")
    assert_contains(playwright, "command: 'npm run dev'", "playwright.config.ts")
    assert_contains(playwright, "reuseExistingServer: !process.env.CI", "playwright.config.ts")
    assert_contains(playwright, "timeout: 120000", "playwright.config.ts")


def test_tauri_dev_config_targets_the_fixed_vite_server() -> None:
    build = tauri_config()["build"]

    if build.get("devUrl") != DEV_SERVER_URL:
        raise AssertionError("src-tauri/tauri.conf.json build.devUrl must match the fixed Vite dev server")
    if build.get("beforeDevCommand") != "npm run dev":
        raise AssertionError("src-tauri/tauri.conf.json must start the checked npm dev script")
    if build.get("beforeBuildCommand") != "npm run build":
        raise AssertionError("src-tauri/tauri.conf.json must build through the checked npm build script")
    if build.get("frontendDist") != "../dist":
        raise AssertionError("src-tauri/tauri.conf.json build.frontendDist must point at the Vite dist output")


def test_package_scripts_do_not_bypass_vite_config() -> None:
    scripts = package_json()["scripts"]

    if scripts.get("dev") != "vite":
        raise AssertionError("package.json dev script must use vite.config.ts directly")
    if "--port" in scripts["dev"] or "--host" in scripts["dev"]:
        raise AssertionError("package.json dev script must not override vite.config.ts server policy")


def main() -> None:
    test_vite_dev_server_uses_fixed_strict_port()
    test_playwright_targets_the_fixed_vite_server()
    test_tauri_dev_config_targets_the_fixed_vite_server()
    test_package_scripts_do_not_bypass_vite_config()
    print("dev server policy regression passed")


if __name__ == "__main__":
    main()
