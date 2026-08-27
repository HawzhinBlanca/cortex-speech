"""Shared fail-closed source surface for the decomposed processing pipeline."""

from pathlib import Path


def pipeline_surface(src_root: Path) -> str:
    """Return the composition root and every shipped pipeline implementation module."""
    root = src_root / "pipeline.rs"
    module_dir = src_root / "pipeline"
    root_text = root.read_text(encoding="utf-8")
    for declaration in ("mod import_flow;", "mod transcription;"):
        if declaration not in root_text:
            raise AssertionError(f"pipeline composition root lost `{declaration}`")
    modules = sorted(module_dir.rglob("*.rs")) if module_dir.is_dir() else []
    if not modules:
        raise AssertionError("pipeline module directory is missing or empty")
    return "\n".join([root_text, *(path.read_text(encoding="utf-8") for path in modules)])
