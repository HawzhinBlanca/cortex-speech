"""Source policies for the review-UI async-race guards.

These guard against navigate-/act-during-an-in-flight-await data-loss bugs in the Svelte review
components (ReviewMode / ReviewInbox). The failure modes are async races that a human triggers by
navigating or keying while a multi-second IPC call is in flight — not meaningfully unit-testable
without a component-mount + fake-timer harness the project does not use (its frontend tests are pure
functions). So, like the Rust runtime-panic source policies, each guard is pinned at the source: the
check fails the moment the guard is removed or the mutator's structure regresses. Each was fail-before
verified (removing the guard fires the assertion).
"""

import re
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


def _matching_brace(src: str, opening: int) -> int:
    """Return the closing brace paired with ``opening`` for a small source-policy region."""
    if opening == -1 or src[opening] != "{":
        return -1
    depth = 0
    for index in range(opening, len(src)):
        if src[index] == "{":
            depth += 1
        elif src[index] == "}":
            depth -= 1
            if depth == 0:
                return index
    return -1


def _matching_paren(src: str, opening: int) -> int:
    """Return the closing parenthesis paired with ``opening`` for a source-policy call."""
    if opening == -1 or src[opening] != "(":
        return -1
    depth = 0
    for index in range(opening, len(src)):
        if src[index] == "(":
            depth += 1
        elif src[index] == ")":
            depth -= 1
            if depth == 0:
                return index
    return -1


def test_retranscribe_uses_authoritative_champion_row_and_guards_editor_writes() -> None:
    """ReviewMode.doRetranscribe() must use the champion's atomic backend commit, reload that exact row,
    and only then update the local store. It may never recreate the removed alternative-engine whole-row
    upsert: that path combined a stale UI snapshot with guessed provenance after a multi-second ASR await.

    The editor state still belongs to the CURRENT clip, so navigation during the awaits requires a
    current-vs-captured-segment guard between the store update and ``editText``. Without it, one clip's
    machine draft can be saved as another clip's human gold (THE ONE LAW).
    """
    body = _function_body(_read("src/lib/ReviewMode.svelte"), "async function doRetranscribe(")
    champion_call = body.find("await api.transcribeSegment(")
    authoritative_reload = body.find("await api.getSegment(seg.id)", champion_call)
    store_write = body.find("segments.update(", authoritative_reload)
    editor_write = body.find("editText = text;")
    guard = body.find("if (current?.id !== seg.id) return;")
    if -1 in (champion_call, authoritative_reload, store_write, guard, editor_write):
        raise AssertionError(
            "doRetranscribe must call the champion, reload its authoritative committed row, update the store, "
            "and then update the editor — one or more source markers are missing"
        )
    if not (champion_call < authoritative_reload < store_write < guard < editor_write):
        raise AssertionError(
            "doRetranscribe must reload/store the champion row before a current-vs-seg guard, and the guard "
            "must precede editText; otherwise navigation during ASR can put one clip's draft in another editor"
        )
    for forbidden in ("api.updateSegment(", "transcribeSegmentFinetuned"):
        if forbidden in body:
            raise AssertionError(
                f"doRetranscribe still contains {forbidden!r}; production re-transcribe must use only the "
                "champion's atomic backend commit, never a frontend whole-row alternative-engine write"
            )


def test_submit_guards_editor_writes_against_navigation() -> None:
    """ReviewMode.submit() must bind the draft/playback/commit to one clip+revision and then reload
    every rendered projection from database truth. It must not optimistically write the segment store or
    editor after the await: navigation during the native commit could otherwise put one clip's transcript in
    another editor or let a stale local row masquerade as the committed decision."""
    body = _function_body(
        _read("src/lib/reviewModeDecisions.svelte.ts"), "async function submit("
    )
    base_revision = body.find("const revision = baseRevision(segment);")
    draft_flush = body.find("await deps.draft.flush();", base_revision)
    navigation_guard = body.find("deps.queue.current()?.id !== segment.id", draft_flush)
    playback = body.find("await deps.playback.finalize(segment, revision)", navigation_guard)
    truth_guard = body.find("durableUndo.truthWriteStillCurrent(truthLease)", playback)
    operation = body.find("const decisionOperationId = commitOperations.idFor(intent);", truth_guard)
    decision_commit = body.find("const commit = await api.commitReviewV1({")
    response_check = body.find("const effectId = committedEffectId(segment, revision, commit);", decision_commit)
    projection_reload = body.find("await settleTruthProjection(truthLease", response_check)
    markers = (
        base_revision,
        draft_flush,
        navigation_guard,
        playback,
        truth_guard,
        operation,
        decision_commit,
        response_check,
        projection_reload,
    )
    if -1 in markers or list(markers) != sorted(markers):
        raise AssertionError(
            "submit() lost its clip/revision guard, exact operation identity, response validation, or "
            "database-projection settlement ordering"
        )
    if any(
        forbidden in body
        for forbidden in (
            "segments.update(",
            "deps.setEditText(",
            "api.updateSegmentFields(segment.id",
            "api.updateSegmentMetadataV1(segment.id",
            "api.updateSegment(",
        )
    ):
        raise AssertionError(
            "submit() performs a renderer-owned projection/editor write after the atomic human decision; "
            "database truth must be reloaded through settleTruthProjection instead"
        )


def test_review_mode_drafts_are_session_local_until_atomic_decision() -> None:
    """A typed ReviewMode draft is not authoritative human truth until submit() atomically records the
    decision and all effects. Navigation may retain it in the in-session editCache, but navigation and
    teardown must never persist annotatedTranscript by themselves."""
    shell = _read("src/lib/ReviewMode.svelte")
    draft = _read("src/lib/reviewModeDraft.svelte.ts")
    decisions = _read("src/lib/reviewModeDecisions.svelte.ts")
    surface = "\n".join((shell, draft, decisions))
    for forbidden in ("api.updateSegmentFields(", "api.updateSegmentMetadataV1(", "api.updateSegment("):
        if forbidden in surface:
            raise AssertionError(
                f"ReviewMode still contains {forbidden!r}; drafts must stay session-local until the atomic "
                "recordHumanDecision commit"
            )
    if "const editCache = new Map<string, string>()" not in draft or "void queueWrite(" not in draft:
        raise AssertionError(
            "ReviewMode navigation no longer proves that dirty drafts remain in its per-segment session cache"
        )
    if "await api.commitReviewV1({" not in decisions:
        raise AssertionError("ReviewMode lost its only durable human-review commit path")


def test_inbox_undo_bails_while_a_decision_is_in_flight() -> None:
    """ReviewInbox.undo(): a Backspace during an in-flight accept/reject/commitEdit/flag would pop that
    action's just-pushed history entry and fire the inverse op (clearHumanDecision) against the SAME id
    while its record is still in flight — losing the undo; and if the in-flight action then rejects, its
    catch does a second history.slice(0,-1), dropping a PREVIOUS segment's entry. The four persisting
    actions all guard isSubmitting; undo must too."""
    body = _function_body(_read("src/lib/reviewInboxDecisions.svelte.ts"), "async function undo(")
    if "if (state.submitting || deps.draft.state.editing) return;" not in body:
        raise AssertionError(
            "undo() has no submitting/editing guard — a Backspace during an in-flight decision "
            "races clearHumanDecision against the record still in flight and can corrupt the history stack. "
            "Add the guard at the top of undo(), matching the four persisting actions."
        )


def test_frontend_has_no_generic_whole_row_segment_writer() -> None:
    """A whole SpeechSegment necessarily carries annotatedTranscript and verified, so a generic frontend
    update_segment wrapper can bypass the schema-v60 review-effect authority even when its caller meant to
    change an unrelated field. Keep that IPC unreachable from every frontend source file."""
    commands = _read("src/lib/commands.ts")
    if "export async function updateSegment(" in commands:
        raise AssertionError("commands.ts re-exposes the generic whole-row update_segment writer")
    direct_invoke = re.compile(r"invoke(?:<[^>]*>)?\(\s*['\"]update_segment['\"]")
    for path in sorted((REPO_ROOT / "src").rglob("*")):
        if path.suffix not in {".ts", ".svelte"}:
            continue
        src = path.read_text(encoding="utf-8")
        if "api.updateSegment(" in src or direct_invoke.search(src):
            raise AssertionError(
                f"{path.relative_to(REPO_ROOT)} can invoke the generic whole-row segment writer; "
                "review-bearing fields must be structurally unreachable"
            )


def test_app_export_audio_excludes_human_rejected() -> None:
    """The workstation export service must filter ids with isVerifiedGood (verified AND NOT
    human-rejected), never raw s.verified: markBad finalizes a REJECTED clip with verified=true (to pull it
    out of the review queue), so a plain s.verified filter ships human-rejected clips + their bad transcripts
    into the 'verified audio' dataset as if human-gold — the export-honesty / count-must-exclude-rejected
    class. The SettingsPanel export and the Rust export_dataset (!is_human_rejected) already exclude them."""
    body = _function_body(
        _read("src/lib/workstationExportActions.ts"), "  async function exportAudio("
    )
    if ".filter((segment) => isVerifiedGood(segment))" not in body:
        raise AssertionError(
            "workstation exportAudio does not filter with isVerifiedGood — a raw verified filter exports "
            "human-rejected ('mark bad') clips as verified audio. Use `.filter((s) => isVerifiedGood(s))`."
        )
    if re.search(r"\.filter\(\([^)]*\)\s*=>\s*[^)]*\.verified\)", body):
        raise AssertionError("exportAudio still filters raw verified state; rejected clips leak into the export")


def test_waveform_stretches_peaks_across_the_canvas() -> None:
    """The waveform renderer maps each bar to a FRACTIONAL slice of the fixed-length peak array so the peaks fill
    the full canvas width. The old `samplesPerBar = max(1, floor(len/numBars))` with `startIdx =
    i*samplesPerBar` mapped 1 sample:1 bar whenever numBars>len (any zoom>1 / a card wider than
    ~len*barWidth px), cramming all peaks into the left strip while the ruler ticks, word-grid labels and
    the amber playhead span the full width via (t/duration)*w — a playhead-vs-waveform misalignment that
    misleads the reviewer inspecting word alignment (the exact purpose of the zoom slider)."""
    src = _read("src/lib/waveformRenderer.ts")
    if "Math.floor((index * waveform.length) / numBars)" not in src:
        raise AssertionError(
            "Waveform.draw() does not map bars fractionally across the peak array — a 1-sample:1-bar map "
            "crams the waveform into the left while the playhead/labels span the full width. Use "
            "`startIndex = Math.floor((index * waveform.length) / numBars)`."
        )
    if "const samplesPerBar = Math.max(1, Math.floor(waveform.length / numBars));" in src:
        raise AssertionError("Waveform still uses the 1-sample:1-bar samplesPerBar clamp; peaks won't fill the width")


def test_library_review_truth_is_read_only_and_metadata_writer_is_narrow() -> None:
    """The main library may play and inspect transcript/diff data, but only ReviewMode can correct and
    approve it. Its remaining partial writer is statically and dynamically limited to speaker/alignment
    metadata, and no frontend call may send annotatedTranscript or verified through it."""
    workstation = _read("src/Workstation.svelte")
    transcript_panel = _read("src/lib/WorkstationTranscriptPanel.svelte")
    annotation_panel = _read("src/lib/WorkstationAnnotationPanel.svelte")
    playback_controller = _read("src/lib/workstationPlaybackController.svelte.ts")
    segment_actions = _read("src/lib/workstationSegmentActions.ts")
    app = "\n".join(
        (workstation, transcript_panel, annotation_panel, playback_controller, segment_actions)
    )
    commands = _read("src/lib/commands.ts")
    coordinator = _read("src/lib/segmentMetadataCoordinator.ts")
    review_mode = _read("src/lib/reviewModeDecisions.svelte.ts")

    forbidden_ui = (
        "handleSaveAnnotation",
        "handleToggleVerify",
        "handleNormalize",
        'data-testid="verify-btn"',
        "finishEditingWord",
        "scheduleAutoSave({ annotatedTranscript",
        "Save annotation",
        "Toggle verified",
        "Toggle verification",
        'aria-keyshortcuts="Enter Space F2"',
        "ondblclick={() => playWordClip",
    )
    present = [marker for marker in forbidden_ui if marker in app]
    if present:
        raise AssertionError(
            "Workstation.svelte still exposes a library review mutation path: "
            + ", ".join(repr(marker) for marker in present)
        )

    transcript = "value={$selectedSegment.annotatedTranscript ?? ''}"
    transcript_start = app.find(transcript)
    transcript_end = app.find("</textarea>", transcript_start)
    if transcript_start == -1 or transcript_end == -1 or "readonly" not in app[transcript_start:transcript_end]:
        raise AssertionError("the library's annotated transcript surface is not explicitly read-only")
    retained_capabilities = (
        (annotation_panel, "<DiffView"),
        (playback_controller, "function playWordClip("),
        (segment_actions, "async function align("),
        (segment_actions, "async function saveSpeaker("),
    )
    for owner, retained in retained_capabilities:
        if retained not in owner:
            raise AssertionError(f"the read-only/metadata library capability {retained!r} was removed")

    if "export type SegmentMetadataFields = Partial<" not in commands or (
        "Pick<SpeechSegment, 'speakerId' | 'alignmentJson'>" not in commands
    ):
        raise AssertionError(
            "updateSegmentMetadataV1 is not statically limited to speakerId/alignmentJson metadata"
        )
    wrapper_start = commands.find("export async function updateSegmentMetadataV1(")
    wrapper_end = commands.find("\n}\n", wrapper_start)
    if wrapper_start == -1 or wrapper_end == -1:
        raise AssertionError("updateSegmentMetadataV1 wrapper not found — this gate would pass vacuously")
    wrapper = commands[wrapper_start:wrapper_end]
    for required in ("expected: SegmentMetadataBaseline", "expected: expected.speakerId", "expected: expected.alignmentJson"):
        if required not in wrapper:
            raise AssertionError(f"updateSegmentMetadataV1 lost compare-and-set binding {required!r}")
    for forbidden in ("annotatedTranscript", "verified"):
        if forbidden in wrapper:
            raise AssertionError(f"updateSegmentMetadataV1 can represent forbidden review field {forbidden}")

    if "save: api.updateSegmentMetadataV1" not in app:
        raise AssertionError("Workstation no longer wires metadata through the versioned coordinator")
    if "const unexpected = Object.keys(fields).filter(" not in coordinator:
        raise AssertionError("Metadata coordinator has no runtime fail-closed key allowlist")
    if "key !== 'speakerId' && key !== 'alignmentJson'" not in coordinator:
        raise AssertionError("Metadata coordinator allowlist drifted from the typed wrapper")

    for path in sorted((REPO_ROOT / "src").rglob("*")):
        if path.suffix not in {".ts", ".svelte"}:
            continue
        src = path.read_text(encoding="utf-8")
        cursor = 0
        while True:
            marker = src.find("api.updateSegmentMetadataV1(", cursor)
            if marker == -1:
                break
            opening = src.find("(", marker)
            closing = _matching_paren(src, opening)
            if closing == -1:
                raise AssertionError(f"could not parse updateSegmentMetadataV1 call in {path.relative_to(REPO_ROOT)}")
            call = src[marker : closing + 1]
            for forbidden in ("annotatedTranscript", "verified"):
                if forbidden in call:
                    raise AssertionError(
                        f"{path.relative_to(REPO_ROOT)} sends forbidden review field {forbidden} "
                        "through updateSegmentMetadataV1"
                    )
            cursor = closing + 1

    if "await api.commitReviewV1({" not in review_mode:
        raise AssertionError("ReviewMode no longer owns the atomic human-decision path")


def test_settings_close_persist_routes_through_savequietly() -> None:
    """SettingsPanel onDestroy is the close-to-save path for theme/sliders (no per-field auto-save). It
    must persist via saveQuietly (which coerces NaN numeric fields, rolls back on backend failure, and
    surfaces an error toast), NEVER a bare fire-and-forget api.updateSettings(localSettings).catch: that
    path skipped coerceSettingsForRuntime, so clearing a type=number field (min/max segment sec,
    maxSpeakers, jurySelfConsistencyN) then closing via the ✕/Escape gesture sent NaN->null and failed the
    ENTIRE backend write, silently discarding every settings edit the user did make (a settings-loss bug)
    while leaving NaN in the reactive store. save() and saveQuietly() both coerce first; onDestroy didn't."""
    panel_body = _function_body(_read("src/lib/SettingsPanel.svelte"), "onDestroy(() => {")
    persistence = _read("src/lib/settingsPersistenceController.ts")
    save_on_destroy = _function_body(persistence, "  function saveOnDestroy(")
    if "persistence.saveOnDestroy()" not in panel_body or "void saveQuietly()" not in save_on_destroy:
        raise AssertionError(
            "SettingsPanel onDestroy does not route through saveOnDestroy -> saveQuietly — the close-to-save path must reuse "
            "it for NaN coercion + rollback + error toast. A bare api.updateSettings here silently discards "
            "all edits when a numeric field was cleared to NaN before the ✕/Escape close."
        )
    if "api.updateSettings(" in panel_body:
        raise AssertionError(
            "SettingsPanel onDestroy still writes settings directly, which "
            "skips coerceSettingsForRuntime — route the close-to-save through saveQuietly() instead."
        )


def test_settings_persists_are_serialized_so_a_stale_rollback_cannot_clobber_a_newer_save() -> None:
    """saveQuietly fires from dozens of onblur/onchange handlers, consent withdrawal, and the
    close-to-save path; save() from the button. Unserialized, two overlapping persists each capture
    their own `prev` snapshot, and an OLDER write failing after a NEWER one succeeded rolls the store
    and every input back past the newer persisted state — worst case re-granting a cloud consent the
    user had successfully withdrawn (audit fix 2026-08-20). Every backend settings write must route
    through one queue so a rollback target is always the true last-persisted state."""
    src = _read("src/lib/settingsPersistenceController.ts")
    if "persistQueue.then(job)" not in src or "persistBusy" not in src:
        raise AssertionError(
            "The settings persistence controller no longer chains writes through persistQueue — overlapping saves "
            "can interleave, and a stale rollback then reverts a newer successful save (consent included)."
        )
    for caller, needle in (("saveQuietly", "return enqueuePersist("), ("save", "await enqueuePersist(")):
        body = _function_body(src, f"function {caller}(")
        if needle not in body:
            raise AssertionError(f"{caller}() must persist via enqueuePersist, not a bare api.updateSettings")
    # No settings write may bypass the queue: every updateSettings call sits inside an enqueued job.
    outside = [
        i for i in range(len(src))
        if src.startswith("api.updateSettings(", i) and "enqueuePersist(" not in src[max(0, i - 1200) : i]
    ]
    if outside:
        raise AssertionError("an api.updateSettings call bypasses the persist queue — the race is back")


def test_refinery_panel_renders_undefined_metric_for_a_zero_segment_eval() -> None:
    """RefineryPanel surfaces eval-run WER/CER. A run that scored ZERO segments — the all-engine-fail case
    (every gold clip's transcription errors → run_gold_eval_with_transcriber and both pipeline closed-loop
    paths drop them all, so numSegs=0 and wer/cer persist as 0.0) — must NOT render as a perfect 0.0%. The
    backend already refuses to read a zero-scored eval as a real rate: build_scorecard/render_markdown say
    "WER/CER are undefined (not 0%)" and the promotion gate returns "CANNOT EVALUATE … undefined, not 0".
    This panel is the one surface that shows the raw run.wer/run.cer, so it must mirror that convention:
    WER/CER go through a numSegs-guarded formatter, never a bare pct(run.wer)/pct(evalResult.run.wer)."""
    presentation = _read("src/lib/refineryPresentation.ts")
    panel = _read("src/lib/RefineryPanel.svelte")
    evidence = _read("src/lib/RefineryEvidence.svelte")
    if "segmentCount > 0 ? formatRefineryPercent(value) : '—'" not in presentation:
        raise AssertionError(
            "RefineryPanel has no numSegs-guarded WER/CER formatter — a zero-segment eval renders as a "
            "perfect 0.0%. Add `const metric = (x, numSegs) => numSegs > 0 ? pct(x) : '—';` and use it."
        )
    for required in (
        "formatRefineryMetric(evalResult.run.cer, evalResult.run.numSegs)",
        "formatRefineryMetric(evalResult.run.wer, evalResult.run.numSegs)",
        "formatRefineryMetric(run.wer, run.numSegs)",
        "formatRefineryMetric(run.cer, run.numSegs)",
    ):
        if required not in f"{panel}\n{evidence}":
            raise AssertionError(f"Refinery evidence no longer routes {required!r} through the guarded formatter")
    for bare in ("pct(run.wer)", "pct(run.cer)", "pct(evalResult.run.wer)", "pct(evalResult.run.cer)"):
        if bare in f"{panel}\n{evidence}":
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
    owner = _read("src/lib/ValidationPanel.svelte")
    src = _read("src/lib/ValidationSignalAnomalyTab.svelte")
    if (
        "<ValidationSignalAnomalyTab" not in owner
        or "segments={signalAnomalySegments}" not in owner
        or "segments.length === 0" not in src
    ):
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
    controller = _read("src/lib/audioPlayerController.ts")
    if "clipKey" not in ap:
        raise AssertionError(
            "AudioPlayer has no clipKey prop — same-source consecutive clips never re-autoplay (autoplay dies "
            "after the first clip). Add a clipKey prop and an $effect that plays on clip-identity change."
        )
    if "autoplayedClip" not in controller or "controller.autoplaySelection(marker)" not in ap:
        raise AssertionError(
            "AudioPlayer has no clip-identity autoplay effect (no autoplayedClip tracking) — a same-source clip "
            "advance won't autoplay because onloadedmetadata only fires on a src change."
        )
    for consumer in ("src/lib/ReviewModeAudio.svelte", "src/lib/ReviewInboxAudio.svelte"):
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
    anchor = "rawSet(dedupeById(page.items));"
    idx = src.find(anchor)
    if idx == -1:
        raise AssertionError("segments load() page commit point not found — this gate would pass vacuously")
    # Widish window: the invalidation sits right after the reload commit, but its explanatory comment is long.
    window = src[idx : idx + 1200]
    if "searchResults.set(null)" not in window:
        raise AssertionError(
            "segments.load() does not invalidate searchResults after the reload commit — applySearchScope keeps "
            "filtering the fresh rows by the STALE frozen FTS id-set (new matches hidden, gone-stale matches "
            "kept) in the curate filter and review queue. Invalidate it immediately after the page commit."
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
    require real content, counting a verified-but-contentless clip toward neither — like a rejected clip.

    The library is now windowed, so the frontend must not derive corpus statistics from its partial page. The
    authoritative SQL aggregate in stats.rs enforces the transcript/training-grade rule and is pinned by Rust
    tests; this guard requires the store to consume that corpus-wide result."""
    src = _read("src/lib/stores/segmentStore.ts")
    if "api.getDatasetStats()" not in src or "verified: stats.verifiedCount" not in src:
        raise AssertionError(
            "segmentStats.verified must come from the corpus-wide backend aggregate, whose verified bucket "
            "excludes placeholder/empty/rejected rows; a bounded frontend page cannot compute this honestly."
        )


def test_batch_verify_controls_are_not_reachable_and_review_mode_owns_approval() -> None:
    """Schema-v60 human truth requires one playback-bound, immutable decision effect per approval.

    The legacy Verify All/Selected path wrote only ``verified=1`` and therefore produced reviewed rows
    with no effect authority; a healthy snapshot containing such a row could not pass the fail-closed
    restore gate. Keep the compatibility IPC wrapper out of the reachable UI, and leave ReviewMode's
    atomic human-decision command as the approval path.
    """
    app = _read("src/Workstation.svelte")
    commands = _read("src/lib/commands.ts")
    forbidden = (
        "handleBatchVerify(",
        "api.batchVerify(",
        "$t('batchVerify.allPending')",
        "$t('batchVerify.selected')",
    )
    present = [marker for marker in forbidden if marker in app]
    if present:
        raise AssertionError(
            "Workstation.svelte must not expose legacy batch verification under schema v60; found "
            + ", ".join(repr(marker) for marker in present)
        )

    command_markers = ("function batchVerify(", "invoke('batch_verify'", 'invoke("batch_verify"')
    present_commands = [marker for marker in command_markers if marker in commands]
    if present_commands:
        raise AssertionError(
            "Frontend commands must not retain an invocable batch verification wrapper under schema v60; found "
            + ", ".join(repr(marker) for marker in present_commands)
        )

    review_mode = _read("src/lib/reviewModeDecisions.svelte.ts")
    if "await api.commitReviewV1({" not in review_mode:
        raise AssertionError(
            "ReviewMode must retain the revision-bound commitReviewV1 approval path after removing legacy batch verify"
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
    src = _read("src/lib/workstationPlaybackController.svelte.ts")
    if "const selectedIdStore = fromStore(selectedSegmentId);" not in src:
        raise AssertionError(
            "the playback controller no longer subscribes to selectedSegmentId through fromStore — "
            "store-only selection changes cannot reseat playback"
        )
    marker = "const segmentId = selectedIdStore.current;"
    start = src.find(marker)
    if start == -1:
        raise AssertionError(
            "the selectedSegmentId-keyed selection effect is gone — a store-only selection (ValidationPanel "
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


def test_a_skip_never_clears_the_reviewers_draft_on_either_route() -> None:
    """couch.html: a skip is the ABSENCE of a verdict, so it must not delete the typed correction.

    Two routes reach the server, and both used to end in `localStorage.removeItem(draftKey(...))`:
    `decide()` when the POST succeeds, and `flushOutboxOnce()` when a decision queued offline replays.
    A reviewer types a partial correction, cannot judge the clip, and skips — the clip stays PENDING and
    their text is the only copy of that work. Clearing it destroys a correction nobody was ever told was
    saved. The online path was written with the guard; the offline one silently broke it, which is why
    this pins BOTH: an `action === 'skip'` check must gate every draft removal on this page.

    Not unit-testable — the page is a static asset served by `include_str!`, with no mount harness — so
    it is pinned at the source like the sibling guards above. Fail-before verified: deleting either
    guard fires this."""
    page = _read("src-tauri/assets/couch.html")
    routes = {
        "decide(": _function_body(page, "async function decide("),
        "flushOutboxOnce(": _function_body(page, "async function flushOutboxOnce("),
    }
    for name, body in routes.items():
        if "removeItem(draftKey(" not in body:
            continue  # this route no longer clears a draft at all, which is stronger than the guard
        if "'skip'" not in body:
            raise AssertionError(
                f"couch.html {name} deletes the reviewer's draft with no skip guard. A skip writes NOTHING "
                f"to the corpus and leaves the clip pending, so the typed correction is the only copy of "
                f"that work — removing it destroys a correction the reviewer was never told was saved. "
                f"Gate the removal on the action not being 'skip'."
            )


def test_post_jury_cer_uses_the_independently_persisted_jury_transcript() -> None:
    """Do not revive the legacy verdict-vs-itself suppression.

    Schema v48 persists the machine's `jury_transcript` independently from the later human-owned
    verdict text. Exact jury/reference matches are now valid zero-error observations. The former
    `selfReferentialN` heuristic confused correctness with identity and hid a genuinely perfect jury.
    """
    owner = _read("src/lib/RefineryPanel.svelte")
    evidence = _read("src/lib/RefineryEvidence.svelte")
    if "<RefineryEvidence {evalRuns} {trend} {lift}" not in owner:
        raise AssertionError("RefineryPanel no longer passes the independently measured lift to its evidence surface")
    if "{#if lift && lift.n > 0}" not in evidence:
        raise AssertionError("RefineryPanel must display a non-empty independently measured lift")
    for stale in ("selfReferentialN", "refinery-lift-self-referential"):
        if stale in f"{owner}\n{evidence}":
            raise AssertionError(f"RefineryPanel revived the invalid verdict-vs-itself heuristic: {stale}")

    backend = _read("src-tauri/src/eval.rs")
    if "SELECT annotated_transcript, raw_transcript, jury_transcript" not in backend:
        raise AssertionError("label-quality lift no longer reads the independent jury transcript")
    if "self_referential_n" in backend:
        raise AssertionError("backend revived the invalid exact-match-is-self-reference field")


def test_certified_segment_count_is_withheld_while_the_certificate_is_uncalibrated() -> None:
    """An uncalibrated conformal certificate has no defensible "certified" count.

    Measured on the live library 2026-08-03: `confidence` and `ctc_score` are NULL on all 144 rows, so
    every segment scores nonconformity (1-0.5) + 0.1*5 = 1.0 from the two `unwrap_or` defaults alone.
    No cut point beats the target (the Hoeffding slack alone is ~0.29 at n=40 against 5%), so the
    fallback threshold becomes that same 1.0 and EVERY non-rejected segment passes it: 144 - 27 rejects
    = the 117 that rendered as "Certified Segments" in success-cyan, beside the panel's own
    "n/a (uncalibrated)". A count of non-rejected segments wearing a statistical word.

    The count is left intact in the payload; the UI is what must not present it as an achievement. "—"
    is this panel's existing idiom for undefined-not-zero (the threshold tile already uses it).
    """
    src = _read("src/lib/StatsDatasetEvidence.svelte")

    if "cert.isCalibrated ? cert.totalCertified : '—'" not in src:
        raise AssertionError(
            "StatsDashboard.svelte must withhold the certified count while the certificate is "
            "uncalibrated: an uncalibrated threshold certifies whatever passes a fallback, which on a "
            "dataset with no confidence data is everything."
        )
    # Success-cyan is an assertion of its own. Bound to isCalibrated so an uncalibrated readout cannot
    # look like a healthy one even at a glance.
    if "class:text-cyan-400={cert.isCalibrated}" not in src:
        raise AssertionError(
            "the certified-count tile must only wear success-cyan when the certificate is calibrated"
        )


def test_library_reads_fail_loudly_instead_of_reporting_an_empty_library() -> None:
    """A malformed segments payload must THROW, never resolve to an empty result.

    The bounded `getSegmentsPage` read used to `console.error` and return an empty page. That converts
    "the IPC payload was not what this app understands"
    into "your library is empty" — a failure indistinguishable from success. ValidationPanel then shows
    no signal anomalies (a clean bill of health from a broken read), ReviewInbox reports nothing left to
    review, and the segment store renders an empty library.

    The silence was never necessary: all four call sites already sit in `try` blocks with user-visible
    error paths (segmentStore raises a persistent banner with Retry plus a toast, ValidationPanel and
    ReviewMode call notifications.error, ReviewInbox writes a status line). The fallback bypassed every
    one of them and left `console.error` — which no user opens — as the only record.
    """
    src = _read("src/lib/commands.ts")

    for fn, marker in (("getSegmentsPage", "get_segments_page returned "),):
        if marker not in src:
            raise AssertionError(
                f"{fn} no longer throws on a malformed payload. Returning an empty result there reports "
                f"an empty library instead of a failed read, and every caller's error path is bypassed."
            )

    # The specific regression: the old shape returned an empty collection right after logging.
    for bad in ("return { items: [], total: 0, nextCursor: null };",):
        if bad in src:
            raise AssertionError(
                f"commands.ts still contains the silent empty-page fallback {bad!r} — a broken read must "
                f"not be presented to the user as an empty library"
            )


def test_a_failed_waveform_decode_is_not_rendered_as_a_silent_clip() -> None:
    """A blank waveform must say "unreadable", never impersonate quiet audio.

    `loadWaveform`'s catch set `waveformData = []` and said nothing. An empty array and a genuinely
    silent clip render identically — a flat strip — so a failed decode read to the reviewer as "this
    audio is quiet". The 2026-08-05 audit hit exactly this ("the top waveform was blank during
    inspection") and had no way to tell which it was. Worse than cosmetic: the reviewer can go on to
    VERIFY a clip whose shape they never actually saw.

    Same failure-looking-like-success class as the library read guards in commands.ts, and fixed the
    same way — the existing user-visible error path is used instead of swallowing.

    BOTH waveform call sites are covered. ReviewMode was fixed first; grepping the class afterwards
    found the identical swallow in Workstation.svelte's curate view. Fixing one caller and leaving the other
    is how a class of bug survives its own fix, so the guard pins both or it pins nothing.
    """
    for controller, surface, testid in (
        (
            "src/lib/reviewModePlayback.svelte.ts",
            "src/lib/ReviewModeAudio.svelte",
            "review-waveform-error",
        ),
        (
            "src/lib/workstationPlaybackController.svelte.ts",
            "src/lib/WorkstationTranscriptPanel.svelte",
            "curate-waveform-error",
        ),
    ):
        src = _read(controller)
        rendered = _read(surface)
        body = _function_body(src, "  async function loadWaveform(")

        if "waveformError" not in body:
            raise AssertionError(
                f"{controller}: loadWaveform no longer records waveformError — an empty waveform is "
                "indistinguishable from a silent clip, so a failed decode reads as quiet audio"
            )
        if "notifications.error" not in body:
            raise AssertionError(
                f"{controller}: loadWaveform's catch no longer surfaces the failure. Swallowing it leaves "
                "the reviewer looking at a flat strip with no indication the audio could not be read."
            )
        # The success path must CLEAR the error, or one bad clip poisons every later clip.
        if "waveformError = null" not in body:
            raise AssertionError(f"{controller}: loadWaveform must clear waveformError on success, or the notice sticks")
        # And the template has to actually branch on it, or the flag is dead state.
        if f'data-testid="{testid}"' not in rendered:
            raise AssertionError(f"{surface} renders no distinct failed-waveform state — the flag never reaches the UI")


def test_the_uncalibrated_panel_names_the_real_cause_when_there_is_no_confidence_at_all() -> None:
    """"Verify at least 10 segments" must not be shown when verifying cannot possibly help.

    Two different reasons make the certificate uncalibrated, and they need different answers:
      * too little verified data  -> reviewing more clips fixes it;
      * the engine returns NO per-clip confidence -> reviewing more clips changes nothing, ever.

    Measured on the live library 2026-08-04: 67 clips verified, and 0/144 carry a confidence or a
    ctc_score, because the cloud engine returns none (`pipeline.rs`: "Scribe returns no per-segment
    confidence"). The panel showed the first message for the second situation - work that cannot help.

    Worth pinning because the no-confidence branch only became reachable when `has_scoreable_confidence`
    started excluding those clips: that exclusion also zeroed the provenance counters, which silently
    removed the only note that had explained anything. A guard here is what stops the next change
    quietly restoring the wrong advice.
    """
    src = _read("src/lib/StatsDatasetEvidence.svelte")

    if "cert.calibrationNoConfidence > 0" not in src:
        raise AssertionError(
            "StatsDashboard.svelte no longer distinguishes 'no confidence data' from 'too few verified "
            "clips', so an uncalibrated panel tells the owner to verify more segments when that cannot help"
        )
    if "stats.conformalNoConfidence" not in src:
        raise AssertionError("the no-confidence explanation string is not rendered")

    # ORDER IS THE WHOLE POINT: the specific cause must be the `{#if}` and the generic fallback the
    # `{:else if}`. Reversed, the fallback always wins and the specific message is dead code that still
    # reads as present in a grep.
    #
    # Pinned as EXACT branch text, not by comparing string offsets. An offset comparison passed the
    # swapped arrangement during its own fail-before: swapping only the CONDITIONS leaves the
    # no-confidence expression sitting on the `{:else if}` line, which is still earlier in the file than
    # the fallback TEXT inside that branch's body. Comparing a condition's position against a message's
    # position measures nothing. (Third time this exact offset-comparison mistake has been caught here.)
    specific_branch = (
        "{#if cert.calibrationNoConfidence > 0 && cert.calibrationRealPosterior + cert.calibrationHeuristic === 0}"
    )
    generic_branch = "{:else if !cert.isCalibrated}"
    if specific_branch not in src or generic_branch not in src:
        raise AssertionError(
            "the uncalibrated panel must test the SPECIFIC cause first: expected "
            f"`{specific_branch}` as the opening branch, then `{generic_branch}` as the fallback. "
            "Reversed, 'verify at least 10 segments' always wins and the real reason never shows."
        )


def test_readiness_verdict_is_sourced_from_the_export_rule_not_the_verified_count() -> None:
    """Insights' "ready to export" must come from the SAME rule the export gates on.

    These two disagree exactly when it matters. Measured on the live library 2026-08-05: 67 of 144
    clips human-verified, and `training_ready` is 0 for all of them, because no word aligner is
    installed so every clip carries `energy_heuristic_alignment`. A verdict derived from the verified
    count would show a healthy, well-reviewed dataset whose export writes zero rows — the "a tally
    counts rows the export drops" bug this repo has already fixed six times, inverted.

    So the verdict must read `breakdown.summary.trainingReadySegments` (from
    `get_training_grade_breakdown`, which calls `quality::training_grade_for_segment`) and must NOT
    be re-based on `verifiedCount` / `verificationRate` by a later refactor.
    """
    src = _read("src/lib/StatsDashboard.svelte")

    if "getTrainingGradeBreakdown()" not in src:
        raise AssertionError(
            "StatsDashboard.svelte no longer loads get_training_grade_breakdown, so its readiness "
            "verdict cannot agree with what an export would actually write"
        )

    start = src.find("const verdict = $derived")
    if start == -1:
        raise AssertionError("the readiness verdict derivation is gone — this gate would pass vacuously")
    verdict_src = src[start : start + 400]

    if "breakdown.summary.trainingReadySegments" not in verdict_src:
        raise AssertionError(
            "the readiness verdict no longer reads trainingReadySegments — it must be sourced from the "
            f"export's own grade rule, not approximated. Got: {verdict_src[:200]!r}"
        )
    for forbidden in ("verifiedCount", "verificationRate"):
        if forbidden in verdict_src:
            raise AssertionError(
                f"the readiness verdict reads `{forbidden}`. A fully-verified library still exports "
                "nothing when every clip carries a blocking grade reason, so this would show 'ready' "
                "over an export that writes zero rows"
            )


def test_readiness_is_unknown_not_ready_when_its_inputs_failed_to_load() -> None:
    """A headline that defaults to green when its own inputs failed is worse than no headline.

    `breakdown` stays null when `get_training_grade_breakdown` throws (the load is deliberately
    tolerant, so one failed panel cannot blank the whole dashboard). The verdict must render that as
    'unknown'. If a refactor makes null fall through to the ready/not-ready comparison, a backend
    error becomes a claim about the dataset.
    """
    src = _read("src/lib/StatsDashboard.svelte")

    start = src.find("const verdict = $derived")
    if start == -1:
        raise AssertionError("the readiness verdict derivation is gone — this gate would pass vacuously")
    verdict_src = src[start : start + 400]

    if "breakdown === null" not in verdict_src:
        raise AssertionError(
            "the readiness verdict no longer special-cases a failed load, so a backend error renders as "
            "a verdict about the data"
        )

    # Pinned as EXACT branch text, whitespace-normalized — NOT by comparing string offsets. An offset
    # comparison is wrong here and silently so: the declaration is
    # `$derived<'ready' | 'notReady' | 'unknown'>(...)`, so both literals also occur in the TYPE
    # PARAMETER, ahead of every branch body. Ordering by `.find()` therefore compares the annotation,
    # not the logic. (Caught by this file's own fail-before, which is the third time an offset
    # comparison has produced a decorative guard in this repo.)
    normalized = " ".join(verdict_src.split())
    if "breakdown === null ? 'unknown' :" not in normalized:
        raise AssertionError(
            "a null breakdown must resolve to 'unknown' as the FIRST ternary arm; otherwise a failed "
            f"load falls through to the ready/not-ready comparison. Got: {normalized[:220]!r}"
        )


def test_review_decisions_stay_on_screen_in_one_sticky_bar() -> None:
    """ReviewMode's four DECISIONS (accept / save & next / mark bad audio / undo) must live together in
    a sticky action bar.

    External review 2026-08-06 P2.2: at 1280x720 the action row sat below the fold behind the waveform,
    the word-level diff and the retranscribe tools, so a reviewer scrolled for every single decision on
    every single clip — the hot path of the entire product. Mark-bad was in a different row again, up
    with the re-transcribe TOOLS, so the four decisions were never visible together at all.

    Pinned at the source for the same reason as the async-race guards above: the failure is a layout
    fact about a mounted component at a specific viewport, which this project's pure-function frontend
    tests cannot observe. What CAN regress silently is someone dropping the sticky positioning or
    moving a decision back out of the bar, and that is exactly what this catches.
    """
    owner = _read("src/lib/ReviewModeActive.svelte")
    bar_src = _read("src/lib/ReviewActionBar.svelte")

    # Follow the live composition seam instead of assuming the bar's markup and CSS remain embedded in
    # ReviewMode. Requiring both the exact import and mount keeps this from passing on an unused/dead
    # component while still allowing the large workspace to shed presentation-only lines.
    if "import ReviewActionBar from './ReviewActionBar.svelte';" not in owner:
        raise AssertionError("ReviewMode no longer imports the one authoritative ReviewActionBar")
    mount_start = owner.find("<ReviewActionBar")
    if mount_start == -1:
        raise AssertionError("ReviewMode no longer mounts the one authoritative ReviewActionBar")
    mount_end = owner.find("/>", mount_start)
    if mount_end == -1:
        raise AssertionError("ReviewMode's ReviewActionBar mount is malformed")
    mount = owner[mount_start : mount_end + 2]
    for decision, callback in (
        ("accept as-is", "onAccept={() => void decisions.submit(true)}"),
        ("save & next", "onSave={() => void decisions.submit(false)}"),
        ("mark bad audio", "onReject={() => void decisions.markBad()}"),
        ("undo", "onUndo={() => void decisions.undoLast()}"),
    ):
        if callback not in mount:
            raise AssertionError(
                f"ReviewMode no longer wires the {decision!r} decision through its mounted action bar"
            )

    if "position: sticky;" not in bar_src or ".review-action-bar" not in bar_src:
        raise AssertionError(
            "ReviewMode's action bar is no longer sticky — the accept/save/bad-audio/undo decisions fall "
            "below the fold at 1280x720. ReviewInbox's .verb-bar is the reference pattern."
        )
    # `fixed` would leave the flow and could then cover the waveform or a 200%-zoom reflow; `sticky`
    # keeps its space and only pins once it would scroll away.
    if "position: fixed" in bar_src:
        raise AssertionError(
            "the review action bar must be `sticky`, not `fixed` — a fixed bar leaves the flow and can "
            "sit on top of the waveform, the transcript, or a zoomed/reflowed layout"
        )

    bar_start = bar_src.find('data-testid="review-action-bar"')
    if bar_start == -1:
        raise AssertionError('the sticky bar lost its data-testid="review-action-bar" hook')
    # The bar's markup runs to the end of the template (</div> chain before <style>).
    bar = bar_src[bar_start : bar_src.find("<style>", bar_start)]
    for decision, handler in (
        ("accept as-is", "onclick={onAccept}"),
        ("save & next", "onclick={onSave}"),
        ("mark bad audio", "onclick={onReject}"),
        ("undo", "onclick={onUndo}"),
    ):
        if handler not in bar:
            raise AssertionError(
                f"the {decision!r} decision ({handler}) is no longer inside the sticky action bar — all "
                "four review decisions belong together and on screen, not scattered among the tools"
            )

    # A sticky element overlays whatever follows it in flow. The keyboard hint used to sit AFTER the
    # row, so making the row sticky would have made the bar cover the one thing that documents the
    # shortcuts. It has to be inside.
    if "review.kbdHint" not in bar:
        raise AssertionError(
            "the keyboard-shortcut hint moved back outside the sticky bar, where the bar covers it"
        )


def test_poor_audio_thresholds_match_the_rust_authority() -> None:
    """The review UI's "poor audio" numbers must equal `quality::POOR_AUDIO_*`.

    P1.2. `snr < 5 dB` / `clipping > 0.1` was written out by hand in THREE independent places — the
    jury veto that refuses to auto-accept, the suspect-first SQL that orders the review queue, and
    `poorAudio()` in ReviewInboxWorkspace.svelte — each with a comment saying it was "kept identical on
    purpose". The two Rust sites now share a constant and a unit test proves they agree at the
    boundary. TypeScript cannot import a Rust const, so this is the seam that remains, and it is the
    worst one to lose: if the UI's copy drifts above the jury's, the review screen shows a reassuring
    green agreement badge on exactly the clip the gate refused to trust — the failure mode the
    2026-08-06 "agreement is not trustworthiness" fix already had to correct once.

    The copy stays. It can no longer drift SILENTLY.
    """
    rust = _read("src-tauri/src/quality.rs")
    svelte = _read("src/lib/ReviewInboxWorkspace.svelte")

    def rust_const(name: str) -> float:
        m = re.search(rf"pub const {name}: f64 = ([0-9.]+);", rust)
        if not m:
            raise AssertionError(
                f"quality.rs no longer defines {name} — this gate would pass vacuously. If the "
                "constant moved, point this check at its new home rather than deleting it."
            )
        return float(m.group(1))

    snr_rust = rust_const("POOR_AUDIO_SNR_DB")
    clip_rust = rust_const("POOR_AUDIO_CLIPPING_RATIO")

    m = re.search(
        r"segment\.snrDb\s*<\s*([0-9.]+)\).*?segment\.clippingRatio\s*>\s*([0-9.]+)\)",
        svelte,
        re.DOTALL,
    )
    if not m:
        raise AssertionError(
            "ReviewInboxWorkspace.svelte's poorAudio() no longer has the shape this gate reads "
            "(segment.snrDb < N ... segment.clippingRatio > N). If it now takes the verdict from the backend "
            "instead of recomputing it, DELETE this check — that is strictly better than syncing."
        )
    snr_ts, clip_ts = float(m.group(1)), float(m.group(2))

    if (snr_ts, clip_ts) != (snr_rust, clip_rust):
        raise AssertionError(
            f"poor-audio thresholds have DRIFTED: quality.rs says snr<{snr_rust} / clip>{clip_rust}, "
            f"ReviewInboxWorkspace.svelte says snr<{snr_ts} / clip>{clip_ts}. The review UI would disagree with "
            "the jury veto about which clips are untrustworthy."
        )


def test_every_declared_readiness_blocker_action_is_actually_rendered() -> None:
    """Every `action:` a readiness blocker declares must have a branch that renders a control for it.

    External review 2026-08-06 P2.3: "every blocker should have a deterministic next action". The
    Blocker type declared `action?: 'relink' | 'review'` and `pendingReview` had carried
    `action: 'review'` since it was written — but the template only ever branched on `'relink'`, so the
    one blocker a reviewer can always act on rendered as a dead sentence. The readiness card named the
    problem and offered no way to fix it, which is the exact opposite of what that card is for.

    This is a DROPPED-INTENT class of bug, not a typo: the data model and the template disagreed, and
    neither side was wrong on its own, so nothing failed. Adding a third action later is the obvious way
    to reintroduce it. So the two are compared directly — the union of declared actions must equal the
    set the template branches on.
    """
    model = _read("src/lib/statsDashboardModel.ts")
    surface = _read("src/lib/StatsReadinessSection.svelte")

    m = re.search(r"type StatsBlocker = \{[^}]*action\?:\s*([^;}]+)", model)
    if not m:
        raise AssertionError(
            "the Blocker type no longer declares `action?:` in the shape this gate reads — "
            "re-point the check rather than deleting it, or dropped actions become invisible again"
        )
    declared = set(re.findall(r"'([a-zA-Z]+)'", m.group(1)))
    if not declared:
        raise AssertionError("no blocker actions parsed — this gate would pass vacuously")

    rendered = set(re.findall(r"blocker\.action === '([a-zA-Z]+)'", surface))

    unrendered = sorted(declared - rendered)
    if unrendered:
        raise AssertionError(
            f"blocker action(s) {unrendered} are declared but never rendered — a blocker that names a "
            "problem and offers no control is a dead end. Add an `{:else if b.action === ...}` branch, "
            "or drop the action from the Blocker type."
        )
    phantom = sorted(rendered - declared)
    if phantom:
        raise AssertionError(
            f"the template branches on blocker action(s) {phantom} that the Blocker type does not "
            "declare — that branch is unreachable and the control never appears"
        )


def test_reason_code_vocabulary_matches_rust() -> None:
    """The UI's reason-code list must equal `jury::reason` exactly, in both directions.

    P1.2. The codes are PERSISTED by Rust and RENDERED by TypeScript, and neither can import the other.
    Drift is silent and asymmetric:
      - a code in Rust but not the UI renders as a raw untranslated string (visible, recoverable);
      - a code in the UI but not Rust is a label for something that is never written (dead, misleading);
      - a code RENAMED on one side only makes every stored row mean something else.

    Also checks both locales, because the reviewer reads Sorani: an untranslated code would surface
    English internals in the RTL UI, which is the whole reason the codes have human labels at all.

    Note on the regex: `[A-Z_]+` is WRONG here and was caught doing exactly this damage during
    development — it silently skipped T2_NO_MAJORITY and T1_UNRESOLVED because they contain digits, and
    reported a drift that did not exist. Const names may contain digits; the pattern must say so.
    """
    rust = _read("src-tauri/src/jury/mod.rs")
    block = re.search(r"pub mod reason \{(.*?)\n\}", rust, re.DOTALL)
    if not block:
        raise AssertionError("jury::reason module not found — this gate would pass vacuously")
    rust_codes = set(re.findall(r'pub const [A-Z0-9_]+: &str = "([a-z0-9_]+)"', block.group(1)))
    if not rust_codes:
        raise AssertionError("no reason codes parsed from Rust — this gate would pass vacuously")

    ts = _read("src/lib/reasonCodes.ts")
    ts_block = re.search(r"KNOWN_REASON_CODES = \[(.*?)\]", ts, re.DOTALL)
    if not ts_block:
        raise AssertionError("KNOWN_REASON_CODES not found in reasonCodes.ts")
    ts_codes = set(re.findall(r"'([a-z0-9_]+)'", ts_block.group(1)))

    missing_in_ui = sorted(rust_codes - ts_codes)
    if missing_in_ui:
        raise AssertionError(
            f"reason code(s) {missing_in_ui} are written by Rust but unknown to the UI — they will "
            "render as raw untranslated strings. Add them to KNOWN_REASON_CODES and to both locales."
        )
    phantom = sorted(ts_codes - rust_codes)
    if phantom:
        raise AssertionError(
            f"the UI declares reason code(s) {phantom} that Rust never writes — a label for something "
            "that cannot happen. Remove them, or add the constant to jury::reason."
        )

    for locale in ("en", "ckb"):
        src = _read(f"src/lib/i18n/{locale}.ts")
        untranslated = sorted(c for c in rust_codes if f"'reason.{c}'" not in src)
        if untranslated:
            raise AssertionError(
                f"reason code(s) {untranslated} have no {locale} translation — a Sorani reviewer would "
                "read English internals in an RTL screen"
            )


def main() -> None:
    # Discovered, not listed: a hand-maintained call list silently skipped a newly added test
    # (caught 2026-08-20), which is the vacuous-pass failure this suite exists to prevent.
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for test in tests:
        test()
    print(f"frontend review-guard source policy passed ({len(tests)} pins)")



if __name__ == "__main__":
    main()
