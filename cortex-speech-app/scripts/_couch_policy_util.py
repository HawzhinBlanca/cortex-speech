"""Shared fail-closed source surface for decomposed Couch Review modules."""

from pathlib import Path


def couch_surface(src_root: Path) -> str:
    """Return the Couch composition root and every shipped Couch authority slice."""
    root = src_root / "couch.rs"
    module_dir = src_root / "couch"
    root_text = root.read_text(encoding="utf-8")
    modules = sorted(module_dir.rglob("*.rs")) if module_dir.is_dir() else []
    if not modules:
        raise AssertionError("Couch module directory is missing or empty")
    text = "\n".join([root_text, *(path.read_text(encoding="utf-8") for path in modules)])
    for required in ("fn start_on_port(", "fn handle_request(", "fn api_queue(", "fn api_decision_authenticated("):
        if required not in text:
            raise AssertionError(f"Couch source surface lost required authority: {required}")
    return text
