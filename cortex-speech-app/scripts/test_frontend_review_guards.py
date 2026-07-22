"""Source policies for the review-UI async-race guards.

These guard against navigate-/act-during-an-in-flight-await data-loss bugs in the Svelte review
components (ReviewMode / ReviewInbox). The failure modes are async races that a human triggers by
navigating or keying while a multi-second IPC call is in flight — not meaningfully unit-testable
without a component-mount + fake-timer harness the project does not use (its frontend tests are pure
functions). So, like the Rust runtime-panic source policies, each guard is pinned at the source: the
check fails the moment the guard is removed or the mutator's structure regresses. Each was fail-before
verified (removing the guard fires the assertion).
"""

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


def _read(rel: str) -> str:
    return (REPO_ROOT / rel).read_text(encoding="utf-8")


def _function_body(src: str, sig: str) -> str:
    """The text of the function starting at `sig`, up to the next top-level (2-space-indented) function."""
    start = src.find(sig)
    if start == -1:
        raise AssertionError(f"{sig!r} not found — this gate would pass vacuously")
    rest = src[start + len(sig):]
    end = len(rest)
    for marker in ("\n  async function ", "\n  function "):
        idx = rest.find(marker)
        if idx != -1:
            end = min(end, idx)
    return rest[:end]


def test_retranscribe_guards_editor_writes_against_navigation() -> None:
    """ReviewMode.doRetranscribe(): after the multi-second ASR await, the DB/store write targets the
    captured seg by id (correct even if the reviewer navigated away), but the editor-state writes
    (editText/lastLoadedOriginal/draftModels) belong to the CURRENT clip. Without a current-vs-seg
    recheck, navigating mid-await puts seg's MACHINE text into another clip's editor, and a subsequent
    Save persists it as that clip's human-verified gold — a wrong-segment gold corruption (THE ONE LAW).
    The guard `if (current?.id !== seg.id) return;` must sit between the store write and the editor write."""
    body = _function_body(_read("src/lib/ReviewMode.svelte"), "async function doRetranscribe(")
    store_write = body.find("await api.updateSegment(updated);")
    editor_write = body.find("editText = text;")
    guard = body.find("if (current?.id !== seg.id) return;")
    if store_write == -1 or editor_write == -1:
        raise AssertionError("doRetranscribe structure changed (store/editor write markers missing) — gate vacuous")
    if guard == -1 or not (store_write < guard < editor_write):
        raise AssertionError(
            "doRetranscribe writes editText without a current-vs-seg guard between the store write and the "
            "editor write: navigating during the ASR await would put seg's machine text into another clip's "
            "editor and Save it as that clip's human gold. Add `if (current?.id !== seg.id) return;`."
        )


def main() -> None:
    test_retranscribe_guards_editor_writes_against_navigation()
    print("frontend review-guard source policy passed")


if __name__ == "__main__":
    main()
