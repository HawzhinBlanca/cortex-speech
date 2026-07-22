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


def test_go_draft_persist_bails_on_aligning_and_uses_freshrow() -> None:
    """ReviewMode.go(): navigating with a dirty edit persists it as a draft via a WHOLE-ROW updateSegment.
    That row includes alignment_json/alignment_quality, so (a) it must not run while a background CTC
    alignment is in flight (`!aligning`) — else a draft built from the pre-align row reverts freshly
    persisted CTC timings to heuristic (the whole-row-clobber class) — and (b) the draft must be built from
    freshRow(seg.id, seg), NOT a stale `{...seg}` spread. Every sibling mutator (submit/markBad/
    doRetranscribe) already guards `aligning` + uses freshRow; go() must match."""
    body = _function_body(_read("src/lib/ReviewMode.svelte"), "async function go(")
    if "!saving && !aligning" not in body:
        raise AssertionError(
            "go()'s draft-persist condition does not bail on `aligning` — a navigate during a background "
            "CTC alignment can revert freshly-persisted CTC timings via the whole-row updateSegment. "
            "Add `!aligning` to the `if (dirty && current && !saving && ... )` condition."
        )
    if "freshRow(seg.id, seg)" not in body:
        raise AssertionError("go() must build its draft from freshRow(seg.id, seg), not a stale snapshot")
    if "{ ...seg, annotatedTranscript" in body:
        raise AssertionError(
            "go() is spreading a stale `{ ...seg }` whole row into the draft (the clobber class the "
            "update-segment-whole-row-upsert discipline forbids); use freshRow(seg.id, seg) instead"
        )


def test_inbox_undo_bails_while_a_decision_is_in_flight() -> None:
    """ReviewInbox.undo(): a Backspace during an in-flight accept/reject/commitEdit/flag would pop that
    action's just-pushed history entry and fire the inverse op (clearHumanDecision) against the SAME id
    while its record is still in flight — losing the undo; and if the in-flight action then rejects, its
    catch does a second history.slice(0,-1), dropping a PREVIOUS segment's entry. The four persisting
    actions all guard isSubmitting; undo must too."""
    body = _function_body(_read("src/lib/ReviewInbox.svelte"), "async function undo(")
    if "if (isSubmitting) return;" not in body:
        raise AssertionError(
            "undo() has no `if (isSubmitting) return;` guard — a Backspace during an in-flight decision "
            "races clearHumanDecision against the record still in flight and can corrupt the history stack. "
            "Add the guard at the top of undo(), matching the four persisting actions."
        )


def test_app_normalize_uses_freshrow_not_a_stale_spread() -> None:
    """App.svelte handleNormalize persists a whole-row updateSegment AFTER `await api.normalizeText`. Like
    every sibling transcribe handler, it must build the row from the FRESH store row by id, never spread the
    pre-await `{ ...seg }` snapshot — which reverts any verify/edit/align stamp that landed on the segment
    during the normalize await (the update-segment-whole-row-upsert clobber class)."""
    body = _function_body(_read("src/App.svelte"), "async function handleNormalize(")
    if "$segments.find((s) => s.id === seg.id)" not in body:
        raise AssertionError(
            "handleNormalize spreads a pre-await snapshot into updateSegment instead of the fresh store row "
            "— a concurrent write during the normalize await is silently reverted (whole-row clobber class). "
            "Use `...($segments.find((s) => s.id === seg.id) ?? seg)` like the transcribe handlers."
        )
    if "{ ...seg, normalizedTranscript }" in body:
        raise AssertionError("handleNormalize still spreads the stale `{ ...seg }` whole row; use freshRow-by-id")


def test_app_export_audio_excludes_human_rejected() -> None:
    """App.svelte handleExportAudio must filter the exported clip ids with isVerifiedGood (verified AND NOT
    human-rejected), never raw s.verified: markBad finalizes a REJECTED clip with verified=true (to pull it
    out of the review queue), so a plain s.verified filter ships human-rejected clips + their bad transcripts
    into the 'verified audio' dataset as if human-gold — the export-honesty / count-must-exclude-rejected
    class. The SettingsPanel export and the Rust export_dataset (!is_human_rejected) already exclude them."""
    body = _function_body(_read("src/App.svelte"), "async function handleExportAudio(")
    if "isVerifiedGood(s)" not in body:
        raise AssertionError(
            "handleExportAudio does not filter with isVerifiedGood — a raw s.verified filter exports "
            "human-rejected ('mark bad') clips as verified audio. Use `.filter((s) => isVerifiedGood(s))`."
        )
    if ".filter((s) => s.verified)" in body:
        raise AssertionError("handleExportAudio still filters raw s.verified; rejected clips leak into the export")


def main() -> None:
    test_retranscribe_guards_editor_writes_against_navigation()
    test_go_draft_persist_bails_on_aligning_and_uses_freshrow()
    test_inbox_undo_bails_while_a_decision_is_in_flight()
    test_app_normalize_uses_freshrow_not_a_stale_spread()
    test_app_export_audio_excludes_human_rejected()
    print("frontend review-guard source policy passed")


if __name__ == "__main__":
    main()
