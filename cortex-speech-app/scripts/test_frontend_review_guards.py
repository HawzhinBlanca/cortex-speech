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


def test_submit_guards_editor_writes_against_navigation() -> None:
    """ReviewMode.submit() (accept/edit): after the recordHumanDecision + updateSegment awaits, the store
    write targets seg by id (correct even if the reviewer navigated away — an 'edit' hashes the whole file,
    hundreds of ms), but editText/lastLoadedOriginal/editedChips belong to the CURRENT clip. Without a
    current-vs-seg recheck, navigating mid-await puts seg's text into another clip's editor (and submit never
    resets lastLoadedId, so that clip's load effect no-ops and never reloads its own text); a subsequent Save
    persists it as that clip's human-verified gold — wrong-segment gold corruption (THE ONE LAW). The guard
    `if (current?.id !== seg.id) return;` must sit between the store write and the editor write. Sibling of
    doRetranscribe's identical guard (found by adversarial hunt-7 / iter 163)."""
    body = _function_body(_read("src/lib/ReviewMode.svelte"), "async function submit(")
    store_write = body.find("await api.updateSegment(updated);")
    editor_write = body.find("editText = text;")
    guard = body.find("if (current?.id !== seg.id) return;")
    if store_write == -1 or editor_write == -1:
        raise AssertionError("submit structure changed (store/editor write markers missing) — gate vacuous")
    if guard == -1 or not (store_write < guard < editor_write):
        raise AssertionError(
            "submit() writes editText without a current-vs-seg guard between the store write and the editor "
            "write: navigating during the decision await would put seg's text into another clip's editor and "
            "Save it as that clip's human gold. Add `if (current?.id !== seg.id) return;`."
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


def test_waveform_stretches_peaks_across_the_canvas() -> None:
    """Waveform.draw() maps each bar to a FRACTIONAL slice of the fixed-length peak array so the peaks fill
    the full canvas width. The old `samplesPerBar = max(1, floor(len/numBars))` with `startIdx =
    i*samplesPerBar` mapped 1 sample:1 bar whenever numBars>len (any zoom>1 / a card wider than
    ~len*barWidth px), cramming all peaks into the left strip while the ruler ticks, word-grid labels and
    the amber playhead span the full width via (t/duration)*w — a playhead-vs-waveform misalignment that
    misleads the reviewer inspecting word alignment (the exact purpose of the zoom slider)."""
    src = _read("src/lib/Waveform.svelte")
    if "Math.floor((i * waveform.length) / numBars)" not in src:
        raise AssertionError(
            "Waveform.draw() does not map bars fractionally across the peak array — a 1-sample:1-bar map "
            "crams the waveform into the left while the playhead/labels span the full width. Use "
            "`startIdx = Math.floor((i * waveform.length) / numBars)`."
        )
    if "const samplesPerBar = Math.max(1, Math.floor(waveform.length / numBars));" in src:
        raise AssertionError("Waveform still uses the 1-sample:1-bar samplesPerBar clamp; peaks won't fill the width")


def test_app_save_handlers_use_field_level_updates() -> None:
    """App.svelte's single-field mutations — handleSaveAnnotation (annotatedTranscript), handleSaveSpeaker
    (speakerId), and handleToggleVerify (verified) — must each persist via api.updateSegmentFields (reads the
    fresh row under the DB lock, writes only the named field, records undo history), NEVER a whole-row
    api.updateSegment(...): a whole-row upsert of the STALE store row reverts any column a concurrent writer
    changed. The Verify button in particular is reachable while the WSL-7B refinement loop is writing
    raw_transcript in the background (it runs on wsl-log events, so it holds NO $isProcessing lock and never
    disables Verify), so a whole-row upsert would silently revert the 7B's fresh transcript. freshRow can't
    help — the store itself is the stale source (hunt-6 / iter 162)."""
    src = _read("src/App.svelte")
    for fn, field in (
        ("handleSaveAnnotation", "annotatedTranscript"),
        ("handleSaveSpeaker", "speakerId"),
        ("handleToggleVerify", "verified"),
    ):
        body = _function_body(src, f"async function {fn}(")
        if "api.updateSegment(" in body:  # any whole-row upsert; updateSegmentFields( does NOT match this
            raise AssertionError(
                f"{fn} whole-row-upserts a stale segment via api.updateSegment(...) — it can revert a "
                f"concurrent writer (a batch op, or the WSL-7B refinement which holds no $isProcessing lock). "
                f"Use api.updateSegmentFields(seg.id, {{ {field} }})."
            )
        if "api.updateSegmentFields(seg.id" not in body:
            raise AssertionError(f"{fn} must persist via api.updateSegmentFields(seg.id, ...) (field-level, lock-safe)")


def test_settings_close_persist_routes_through_savequietly() -> None:
    """SettingsPanel onDestroy is the close-to-save path for theme/sliders (no per-field auto-save). It
    must persist via saveQuietly (which coerces NaN numeric fields, rolls back on backend failure, and
    surfaces an error toast), NEVER a bare fire-and-forget api.updateSettings(localSettings).catch: that
    path skipped coerceSettingsForRuntime, so clearing a type=number field (min/max segment sec,
    maxSpeakers, jurySelfConsistencyN) then closing via the ✕/Escape gesture sent NaN->null and failed the
    ENTIRE backend write, silently discarding every settings edit the user did make (a settings-loss bug)
    while leaving NaN in the reactive store. save() and saveQuietly() both coerce first; onDestroy didn't."""
    body = _function_body(_read("src/lib/SettingsPanel.svelte"), "onDestroy(() => {")
    if "saveQuietly()" not in body:
        raise AssertionError(
            "SettingsPanel onDestroy does not persist via saveQuietly() — the close-to-save path must reuse "
            "it for NaN coercion + rollback + error toast. A bare api.updateSettings here silently discards "
            "all edits when a numeric field was cleared to NaN before the ✕/Escape close."
        )
    if "api.updateSettings(localSettings).catch" in body:
        raise AssertionError(
            "SettingsPanel onDestroy still fire-and-forgets api.updateSettings(localSettings).catch, which "
            "skips coerceSettingsForRuntime — route the close-to-save through saveQuietly() instead."
        )


def test_refinery_panel_renders_undefined_metric_for_a_zero_segment_eval() -> None:
    """RefineryPanel surfaces eval-run WER/CER. A run that scored ZERO segments — the all-engine-fail case
    (every gold clip's transcription errors → run_gold_eval_with_transcriber and both pipeline closed-loop
    paths drop them all, so numSegs=0 and wer/cer persist as 0.0) — must NOT render as a perfect 0.0%. The
    backend already refuses to read a zero-scored eval as a real rate: build_scorecard/render_markdown say
    "WER/CER are undefined (not 0%)" and the promotion gate returns "CANNOT EVALUATE … undefined, not 0".
    This panel is the one surface that shows the raw run.wer/run.cer, so it must mirror that convention:
    WER/CER go through a numSegs-guarded formatter, never a bare pct(run.wer)/pct(evalResult.run.wer)."""
    src = _read("src/lib/RefineryPanel.svelte")
    if "numSegs > 0 ? pct" not in src:
        raise AssertionError(
            "RefineryPanel has no numSegs-guarded WER/CER formatter — a zero-segment eval renders as a "
            "perfect 0.0%. Add `const metric = (x, numSegs) => numSegs > 0 ? pct(x) : '—';` and use it."
        )
    for bare in ("pct(run.wer)", "pct(run.cer)", "pct(evalResult.run.wer)", "pct(evalResult.run.cer)"):
        if bare in src:
            raise AssertionError(
                f"RefineryPanel still renders a raw {bare} — a zero-segment run prints 0.0% (reads as a "
                f"perfect model from zero data). Route it through the numSegs-guarded metric() formatter."
            )


def test_validation_signal_anomaly_distinguishes_not_screened_from_clean() -> None:
    """ValidationPanel's Signal-Anomaly tab must NOT render the 'no anomalies' all-clear before any screen has
    run. signalAnomalyScore is populated ONLY by the manual Run Signal-Anomaly Screen (compute_signal_anomaly_scores);
    the import/transcribe pipeline never sets it — so an empty signalAnomalySegments means NOT SCREENED, not a
    clean sheet. Conflating the two shows a green all-clear over audio nobody has screened. The tab must render a
    distinct 'not screened yet' state when signalAnomalySegments.length === 0, reserving noSignalAnomaly for a
    genuine post-screen all-clear (scores exist but none above threshold)."""
    src = _read("src/lib/ValidationPanel.svelte")
    if "signalAnomalySegments.length === 0" not in src:
        raise AssertionError(
            "ValidationPanel's Signal-Anomaly empty state does not distinguish 'not screened yet' "
            "(signalAnomalySegments.length === 0) from a real all-clear — it shows noSignalAnomaly before any "
            "screen has run, a false green light over unscreened audio. Add an `{:else if "
            "signalAnomalySegments.length === 0}` branch that renders the notScreened message."
        )
    if "signalAnomaly.notScreened" not in src:
        raise AssertionError(
            "ValidationPanel must render validation.signalAnomaly.notScreened for the never-screened state "
            "(distinct from noSignalAnomaly, the post-screen all-clear)."
        )


def test_audioplayer_autoplays_each_clip_not_only_on_source_reload() -> None:
    """AudioPlayer autoplay lived ONLY in handleLoaded (the onloadedmetadata handler), which fires only when
    the <audio> src actually changes. Consecutive review segments from the SAME recording share audioPath, so
    the element never reloads and onloadedmetadata never re-fires — autoplay died after the FIRST clip, exactly
    the per-clip keypress the autoplay setting was meant to remove (see the True-10 audit comments in
    ReviewMode/ReviewInbox). AudioPlayer must also autoplay when the CLIP IDENTITY changes: a clipKey prop plus
    an $effect that plays on a clipKey change (guarded on !loading so a same-source advance plays while a
    different-source reload is left to handleLoaded, avoiding a double play), keyed on identity so a word-tap
    (which only narrows startTime) does not re-autoplay. Both review consumers must pass clipKey."""
    ap = _read("src/lib/AudioPlayer.svelte")
    if "clipKey" not in ap:
        raise AssertionError(
            "AudioPlayer has no clipKey prop — same-source consecutive clips never re-autoplay (autoplay dies "
            "after the first clip). Add a clipKey prop and an $effect that plays on clip-identity change."
        )
    if "autoplayedClip" not in ap:
        raise AssertionError(
            "AudioPlayer has no clip-identity autoplay effect (no autoplayedClip tracking) — a same-source clip "
            "advance won't autoplay because onloadedmetadata only fires on a src change."
        )
    for consumer in ("src/lib/ReviewMode.svelte", "src/lib/ReviewInbox.svelte"):
        if "clipKey=" not in _read(consumer):
            raise AssertionError(f"{consumer} does not pass clipKey to AudioPlayer — autoplay won't advance per clip")


def test_speaker_rename_confirms_before_merging_into_an_existing_speaker() -> None:
    """SpeakerPanel.handleRename renames a speaker via a blanket speaker_id UPDATE that is NOT on the undo
    stack (unlike delete). Renaming into an existing speaker id MERGES both groups — and the input is prefilled
    free-text, so a typo matching an existing id collapses a diarization split (e.g. 50/30 → 80) with no
    window.confirm and no Ctrl+Z to reverse it. handleRename must detect a target that already belongs to
    ANOTHER speaker and window.confirm before the (irreversible) merge, matching StatsDashboard's
    destructive-action pattern (window.confirm on restore/prune)."""
    body = _function_body(_read("src/lib/SpeakerPanel.svelte"), "async function handleRename(")
    if "window.confirm" not in body:
        raise AssertionError(
            "SpeakerPanel.handleRename has no window.confirm guard — a rename colliding with an existing "
            "speaker id silently, irreversibly merges the two groups. Confirm before the merge."
        )
    if "speakers.find" not in body:
        raise AssertionError(
            "handleRename does not check the new name against existing speakers, so it can't tell a MERGE from "
            "a plain relabel. Detect the collision (speakers.find(s => s.speakerId === trimmed && s.speakerId "
            "!== oldId)) and confirm only then."
        )


def test_segment_reload_invalidates_the_frozen_search_scope() -> None:
    """applySearchScope filters the LIVE segments by the FROZEN id-set of the last FTS searchResults. A full
    segments reload (segments.load(), fired on import/batch/refine completion — the store's own comment notes
    "reloads fire while filters are active") replaces every row, so that frozen set is stale: a clip whose
    refined transcript now matches the query has an id absent from the set (never shown), while one that no
    longer matches stays in it (still shown) — in BOTH the curate filter and the review queue, until the user
    retypes. load() must invalidate searchResults on reload so the scope falls back to applySearchScope's LIVE
    substring predicate (the searchResults===null branch) instead of a stale FTS snapshot."""
    src = _read("src/lib/stores/segmentStore.ts")
    anchor = "set(dedupeById(acc));"
    idx = src.find(anchor)
    if idx == -1:
        raise AssertionError("segments load() commit point set(dedupeById(acc)) not found — this gate would pass vacuously")
    # Widish window: the invalidation sits right after the reload commit, but its explanatory comment is long.
    window = src[idx : idx + 1200]
    if "searchResults.set(null)" not in window:
        raise AssertionError(
            "segments.load() does not invalidate searchResults after the reload commit — applySearchScope keeps "
            "filtering the fresh rows by the STALE frozen FTS id-set (new matches hidden, gone-stale matches "
            "kept) in the curate filter and review queue. Add `searchResults.set(null)` after set(dedupeById(acc))."
        )


def test_processing_progress_eta_uses_chunk_scoped_elapsed_not_whole_pipeline() -> None:
    """ProcessingProgress's ETA extrapolates elapsed/done*(remaining). Its `elapsed` must measure ONLY the
    counted (done/total) scope — NOT the whole pipeline, whose elapsed includes the slow whole-file
    reference_transcribing phase that runs BEFORE any per-chunk Progress starts. Feeding the raw whole-pipeline
    elapsedMs to computeProgress made the first chunk's ETA ≈ (reference-phase seconds) × remaining chunks —
    wildly inflated early (then collapsing toward reality). The ETA baseline must RESET when the counted scope
    (total>0) begins, and computeProgress must be called with that chunk-scoped elapsed, while the DISPLAYED
    elapsed stays whole-run."""
    src = _read("src/lib/ProcessingProgress.svelte")
    if "computeProgress(current, total, elapsedMs)" in src:
        raise AssertionError(
            "ProcessingProgress feeds the WHOLE-pipeline elapsedMs to computeProgress, so the ETA is inflated "
            "by the pre-chunk reference_transcribing phase. Compute the ETA from a chunk-scoped elapsed that "
            "resets when total>0 begins."
        )
    if "etaBaselineMs" not in src or "etaElapsedMs" not in src:
        raise AssertionError(
            "ProcessingProgress has no chunk-scoped ETA baseline (etaBaselineMs/etaElapsedMs) — the ETA still "
            "extrapolates from whole-pipeline elapsed including the reference phase."
        )


def test_segment_stats_verified_excludes_placeholder_only_rows() -> None:
    """segmentStats.verified is the dashboard 'verified clips' count. batch_verify (commands/batch.rs) sets
    verified=true with NO content guard, so 'Verify all pending' marks a still-pending placeholder clip
    ('[Pending WSL 7B ASR]', awaiting the 7B) as verified — yet export_dataset drops every placeholder/empty row
    via the training grade. A plain `s.verified` tally therefore OVER-counts what the exported dataset can
    actually contain (the recurring count-must-exclude-placeholder honesty class). The verified bucket must
    require real content (hasRealTranscript), counting a verified-but-contentless clip toward neither — like a
    rejected clip."""
    src = _read("src/lib/stores/segmentStore.ts")
    if "s.verified && hasRealTranscript(s)" not in src:
        raise AssertionError(
            "segmentStats.verified counts bare `s.verified` rows, including batch-verified placeholders/empties "
            "the export drops. Gate the verified bucket on `s.verified && hasRealTranscript(s)`."
        )


def test_verify_all_pending_excludes_placeholder_and_empty_rows() -> None:
    """The ROOT of the class test_segment_stats_verified_excludes_placeholder_only_rows only band-aided at the
    COUNT: handleBatchVerify('pending') builds its id list as $segments.filter(s => !s.verified) with no
    content filter, so "Verify All Pending" marks placeholder ('[Pending WSL 7B ASR]') / empty rows verified.
    They never ship (export drops placeholders) but they STRAND — the WSL-7B refinement loop skips verified
    rows (update_asr_transcript_if_unreviewed's `AND verified=0`), so the placeholder can never be filled — and
    they dishonestly read as verified. The bulk list must exclude no-content rows via hasRealTranscript (hunt-5
    / iter 161)."""
    body = _function_body(_read("src/App.svelte"), "async function handleBatchVerify(")
    if "!s.verified && hasRealTranscript(s)" not in body:
        raise AssertionError(
            "handleBatchVerify('pending') selects `s => !s.verified` with no content filter — 'Verify All "
            "Pending' marks placeholder/empty rows verified (stranding them from 7B refinement). Filter the "
            "pending id list with `!s.verified && hasRealTranscript(s)`."
        )


def test_keyboard_shortcuts_help_each_uses_a_unique_key() -> None:
    """The Keyboard Shortcuts help modal renders {#each shortcuts.filter(...) as s (KEY)}. Keying on
    s.description throws Svelte's each_key_duplicate (crash / dropped row) when two shortcuts share a
    description — and two DO: two navigation-category entries both read 'Keyboard shortcuts (? key)' (the / and
    ? chords). So opening the help modal (via / or ?) crashes it. The key must be unique per row (the loop
    index — the list is static and never reorders), never s.description."""
    src = _read("src/lib/KeyboardShortcuts.svelte")
    if "as s (s.description)" in src:
        raise AssertionError(
            "KeyboardShortcuts {#each} keys on (s.description), which is NOT unique — two shortcuts sharing a "
            "description (the / and ? help chords) collide → each_key_duplicate crashes the help modal. Key on "
            "the loop index instead: `as s, i (i)`."
        )


def test_selection_reseats_playback_centrally_for_store_only_selections() -> None:
    """A selection made from ANOTHER component — ValidationPanel "Go to segment" (jumpToSegment), the
    active-learning / signal-anomaly jumps — can only set the selectedSegmentId store; it cannot reach App's
    currentTime / waveform. If the per-selection view setup (playhead reset + waveform + word timestamps)
    lived ONLY inline in selectSegment(), such store-only selections would leave the AudioPlayer playing the
    PREVIOUS chunk's audio (chunks of one recording share a source audioPath) straight through the new clip's
    endTime while the UI shows the new clip — the reviewer verifies one clip while HEARING another. The reset
    MUST live in a $selectedSegmentId-keyed effect so EVERY selection path gets it. Runtime is Svelte reactive
    glue + an <audio> element, not unit-testable; source-pinned (hunt-4 / iter 160)."""
    src = _read("src/App.svelte")
    marker = "const id = $selectedSegmentId;"
    start = src.find(marker)
    if start == -1:
        raise AssertionError(
            "the $selectedSegmentId-keyed selection effect is gone — a store-only selection (ValidationPanel "
            "'Go to segment') would leave the AudioPlayer on the previous chunk. This gate would pass vacuously."
        )
    rest = src[start:]
    end = len(rest)
    for m in ("\n  $effect(", "\n  function ", "\n  async function ", "\n  let "):
        i = rest.find(m, 1)
        if i != -1:
            end = min(end, i)
    body = rest[:end]
    for needle, why in (
        ("currentTime = chunkPlaybackRange(", "reseat the playhead at the new chunk start"),
        ("loadWaveform(", "refresh the waveform bars for the new chunk"),
    ):
        if needle not in body:
            raise AssertionError(
                f"the $selectedSegmentId effect no longer reseats the view ({why}: {needle!r}). Keep the "
                "per-selection reset in this central effect — inline-only-in-selectSegment leaves store-only "
                "cross-component selections (ValidationPanel 'Go to segment') playing the wrong clip's audio."
            )


def main() -> None:
    test_retranscribe_guards_editor_writes_against_navigation()
    test_submit_guards_editor_writes_against_navigation()
    test_go_draft_persist_bails_on_aligning_and_uses_freshrow()
    test_inbox_undo_bails_while_a_decision_is_in_flight()
    test_app_normalize_uses_freshrow_not_a_stale_spread()
    test_app_export_audio_excludes_human_rejected()
    test_waveform_stretches_peaks_across_the_canvas()
    test_app_save_handlers_use_field_level_updates()
    test_settings_close_persist_routes_through_savequietly()
    test_refinery_panel_renders_undefined_metric_for_a_zero_segment_eval()
    test_validation_signal_anomaly_distinguishes_not_screened_from_clean()
    test_audioplayer_autoplays_each_clip_not_only_on_source_reload()
    test_speaker_rename_confirms_before_merging_into_an_existing_speaker()
    test_segment_reload_invalidates_the_frozen_search_scope()
    test_processing_progress_eta_uses_chunk_scoped_elapsed_not_whole_pipeline()
    test_segment_stats_verified_excludes_placeholder_only_rows()
    test_keyboard_shortcuts_help_each_uses_a_unique_key()
    test_selection_reseats_playback_centrally_for_store_only_selections()
    test_verify_all_pending_excludes_placeholder_and_empty_rows()
    print("frontend review-guard source policy passed")


if __name__ == "__main__":
    main()
