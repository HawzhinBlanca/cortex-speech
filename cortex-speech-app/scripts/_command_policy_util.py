"""Shared fail-closed source surface for decomposed Tauri command modules."""

from pathlib import Path


def command_surface(src_root: Path) -> str:
    """Return the command composition root and every shipped command slice."""
    root = src_root / "commands.rs"
    module_dir = src_root / "commands"
    root_text = root.read_text(encoding="utf-8")
    modules = sorted(module_dir.rglob("*.rs")) if module_dir.is_dir() else []
    if not modules:
        raise AssertionError("command module directory is missing or empty")
    text = "\n".join([root_text, *(path.read_text(encoding="utf-8") for path in modules)])
    if "#[tauri::command]" not in text:
        raise AssertionError("command surface contains no Tauri commands")
    return text


def command_production_surface(src_root: Path) -> str:
    """Return every shipped command slice with inline test modules removed per file."""
    root = src_root / "commands.rs"
    module_dir = src_root / "commands"
    modules = sorted(module_dir.rglob("*.rs")) if module_dir.is_dir() else []
    if not modules:
        raise AssertionError("command module directory is missing or empty")
    sources = [root, *modules]
    production = "\n".join(
        path.read_text(encoding="utf-8").split("#[cfg(test)]\nmod tests", 1)[0]
        for path in sources
    )
    if "#[tauri::command]" not in production:
        raise AssertionError("production command surface contains no Tauri commands")
    return production
