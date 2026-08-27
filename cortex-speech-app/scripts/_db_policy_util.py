"""Shared fail-closed source surface for the decomposed Database facade."""

from pathlib import Path


def database_surface(src_root: Path) -> str:
    """Return `db.rs` and every shipped Database implementation slice."""
    root = src_root / "db.rs"
    module_dir = src_root / "db"
    root_text = root.read_text(encoding="utf-8")
    modules = sorted(module_dir.rglob("*.rs")) if module_dir.is_dir() else []
    if not modules:
        raise AssertionError("Database module directory is missing or empty")
    text = "\n".join([root_text, *(path.read_text(encoding="utf-8") for path in modules)])
    for required in (
        "pub struct Database",
        "pub fn open(path:",
        "pub fn insert_segment(",
        "pub fn record_review_event(",
        "pub fn begin_desktop_playback_session_v1(",
    ):
        if required not in text:
            raise AssertionError(f"Database source surface lost required authority: {required}")
    return text
