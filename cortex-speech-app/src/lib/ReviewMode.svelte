<script lang="ts">
  import { tick } from 'svelte';
  import { get } from 'svelte/store';
  import { segments, selectedSegmentId, searchScopedSegments, searchQuery } from './stores/segmentStore';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { settings } from './stores/settingsStore';
  import { showReviewInbox, showConfirmDialog, isProcessing } from './stores/uiStore';
  import { physicalKey } from './keyboard';
  import { t } from './i18n';
  import Waveform from './Waveform.svelte';
  import AudioPlayer from './AudioPlayer.svelte';
  import EmptyState from './EmptyState.svelte';
  import {
    parseWordTimestamps,
    parseSourceMeta,
    chunkPlaybackRange,
    segmentSourceFilename,
    segmentChunkLabel,
  } from './alignment';
  import { reviewProgress } from './reviewProgress';
  import { wordPlayBounds, replaceWordToken } from './wordEdit';
  import { isPlaceholderTranscript } from './segmentQuality';
  import type { SpeechSegment, WordTimestamp } from './types';
  import type { SegmentConsensus } from './commands';

  interface Props {
    // Pro next-steps surfaced when the whole queue is reviewed (wired by App to its export / exit).
    onExport?: () => void;
    onDone?: () => void;
  }
  let { onExport, onDone }: Props = $props();

  // M2.5/P1.4: suspect-first queue toggle. When on, the pending group is reordered by the backend's
  // suspect ranking (escalated first, then lowest agent confidence, then chronological) so the reviewer
  // lands on the riskiest clips first. Off by default — the plain pending-first order is unchanged.
  let suspectFirst = $state(false);
  // Cached id→rank from the backend command; new segments (not in the map) sort to the end.
  let suspectRank = $state<Map<string, number> | null>(null);

  async function toggleSuspectFirst() {
    const next = !suspectFirst;
    if (next) {
      try {
        const ordered = await api.getSegmentsSuspectFirst();
        suspectRank = new Map(ordered.map((s, i) => [s.id, i]));
      } catch (e) {
        notifications.error($t('review.suspectFirstFailed'), { detail: String(e) });
        return; // stay off if the fetch failed
      }
    } else {
      suspectRank = null;
    }
    suspectFirst = next;
    index = 0; // land on the top of the reordered queue
  }

  // True-10 audit: a curate-mode SEARCH now scopes the review queue (review one source file, one
  // speaker, or any search subset) — with an explicit banner so the scope is never silent. Only the
  // search scopes; the verified-filter is ignored here because review mode has its own
  // pending-first ordering (a verified-only filter would render the queue permanently "all done").
  const searchScoped = $derived($searchQuery.trim().length > 0);

  // Simple, focused review queue: one clip at a time. Pending (unverified) first,
  // then the rest — so a reviewer always lands on work that needs doing.
  // searchScopedSegments (NOT filteredSegments) enforces the search-only contract above: the
  // curate "✓ Verified" chip must never leak in and empty the queue (true-10 audit).
  const queue = $derived.by<SpeechSegment[]>(() => {
    const all = searchScoped ? $searchScopedSegments : $segments;
    const pending = all.filter((s) => !s.verified);
    const done = all.filter((s) => s.verified);
    if (suspectFirst && suspectRank) {
      const rank = suspectRank;
      pending.sort((a, b) => (rank.get(a.id) ?? Infinity) - (rank.get(b.id) ?? Infinity));
    }
    return [...pending, ...done];
  });

  let index = $state(0);
  // M2.6/P1.5: on first queue availability, resume at the restored session cursor (the last segment
  // the reviewer acted on) instead of always restarting at 0. One-shot — never fights later navigation.
  let cursorRestored = $state(false);
  $effect(() => {
    if (cursorRestored || queue.length === 0) return;
    const targetId = $selectedSegmentId;
    if (targetId) {
      const pos = queue.findIndex((s) => s.id === targetId);
      // Land on the restored cursor only when it still needs work. A VERIFIED cursor (the last clip
      // the reviewer ACTED on) would reopen finished gold as the very first thing on screen — one
      // accidental keypress from destroying it (the 2026-07-14 live-test incident). Verified cursor →
      // start at the queue head (first pending) instead.
      if (pos >= 0 && !queue[pos].verified) index = pos;
    }
    cursorRestored = true;
  });
  const current = $derived(queue[index] ?? null);
  // Audit 2026-08-05: the position counter above counts the QUEUE while the progress text counted the
  // whole CORPUS, so an active search silently split the denominator and the two lines contradicted
  // each other on screen. One call now produces both. `progress.allReviewed` stays corpus-scoped on
  // purpose — a fully-reviewed SEARCH SUBSET must never fire the completion banner. See
  // reviewProgress.ts, where that rule is unit-tested.
  const progress = $derived(reviewProgress(queue, $segments));
  // Hoisted so the label is computed once instead of twice per render, and so the chunk position gets
  // a NOUN in the markup below: bare "61/144" beside "Clip 1 of 144" read as a third progress counter.
  const chunkLabel = $derived(current ? segmentChunkLabel(current.alignmentJson) : null);

  let editText = $state('');
  let waveformData = $state<number[]>([]);
  // Non-null ONLY when the decode failed. Distinguishes "could not read the audio" from a genuinely
  // quiet clip, which an empty array alone cannot.
  let waveformError = $state<string | null>(null);
  let currentTime = $state(0);
  let playerDuration = $state(0);
  let playing = $state(false);
  let saving = $state(false);
  let lastLoadedId = $state<string | null>(null);

  // Offline best-of-N consensus draft (ability-weighted vote across this clip's ASR hypotheses) + the
  // per-word agreement that drives the disagreement highlight. Only shown when 2+ models voted.
  let consensus = $state<SegmentConsensus | null>(null);
  // Engines that actually produced this clip's draft, recorded (never inferred). Shown as an honest
  // provenance badge even for a single-engine clip (e.g. only the OmniASR-7B Champion ran), where the
  // multi-model consensus card below is intentionally hidden.
  let draftModels = $state<string[]>([]);
  let consensusSeq = 0;
  async function loadConsensus(seg: SpeechSegment) {
    const seq = ++consensusSeq;
    consensus = null;
    draftModels = [];
    try {
      const c = await api.getSegmentConsensus(seg.id);
      if (seq !== consensusSeq) return;
      draftModels = c.models ?? [];
      consensus = c.words.length > 0 && c.modelCount >= 2 ? c : null;
    } catch {
      if (seq === consensusSeq) {
        consensus = null;
        draftModels = [];
      }
    }
  }

  // The engine id (matching api.engineLabel) that the champion re-transcribe ACTUALLY runs: the
  // configured primary model, read from settings — never assumed. Keeps the provenance badge honest
  // if the user switched the primary away from the default 7B.
  function primaryEngineId(): string {
    const s = get(settings);
    // useFinetunedAsr is documented as OVERRIDING the model selection: with it on, the backend's
    // transcribe() drafts with the fine-tuned MMS before any configured-engine code runs — a badge
    // naming the configured model would be the exact wrong-engine attribution this function exists
    // to prevent (true-10 audit 2026-07-09). (If the fine-tuned files are missing the backend falls
    // back to the configured engine; the fully-honest fix is the backend returning the engine id it
    // used — until then this matches the documented, common case.)
    if (s.useFinetuned) return 'finetuned-mms-ckb';
    const map: Record<string, string> = {
      'wsl-7b': 'omniasr-wsl-7b',
      'ctc-1b': 'omniasr-ctc-1b',
      'ctc-300m': 'omniasr-ctc-300m',
    };
    return map[s.asrModel] ?? 'omniasr-wsl-7b';
  }

  // Re-transcribe THIS clip with a chosen engine when the current draft is wrong. 'champion' routes
  // through the configured primary engine (the OmniASR-7B Champion by default — needs its server up);
  // 'finetuned' runs the embedded fine-tuned MMS-1B (CPU/ONNX, always available). A re-transcription is
  // machine output, so verified is reset (it must never be kept as if a human confirmed it).
  let retranscribing = $state(false);
  function retranscribe(engine: 'champion' | 'finetuned') {
    const seg = current;
    // P2.3b: refuse while a batch/import is running. doRetranscribe writes rawTranscript, which is NOT
    // in the targeted field-update whitelist, so it must whole-row upsert — and the store row is stale
    // vs the DB during a batch, so it would revert the batch's writes. Re-transcribe is itself a machine
    // op, so refusing it during another machine run is correct (matches the curate transcribe handlers).
    if (!seg || retranscribing || saving || aligning || $isProcessing) return;
    // GUARD (2026-07-15, from the live-test incident): re-transcribing a VERIFIED clip replaces the
    // human-reviewed gold with a machine draft and reopens it — destroying review work must never be
    // one accidental click. The pre-save snapshot goes on the undo stack either way, so even a
    // confirmed destructive re-transcribe is reversible via Undo review.
    if (seg.verified) {
      showConfirmDialog.set({
        title: $t('review.retranscribeVerifiedTitle'),
        message: $t('review.retranscribeVerifiedMessage'),
        confirmLabel: $t('review.retranscribeVerifiedConfirm'),
        danger: true,
        onConfirm: () => void doRetranscribe(engine),
      });
      return;
    }
    void doRetranscribe(engine);
  }
  async function doRetranscribe(engine: 'champion' | 'finetuned') {
    const seg = current;
    // `aligning` too: the clip-load background align persists fresh CTC timings mid-flight; an
    // upsert built from the pre-align snapshot would revert them (stale-spread clobber). `$isProcessing`
    // (P2.3b): rawTranscript is not field-update-whitelisted so this must whole-row upsert, which would
    // revert a concurrent batch's writes (the store is stale vs the DB during a batch) — refuse instead.
    if (!seg || retranscribing || saving || aligning || $isProcessing) return;
    retranscribing = true;
    try {
      const result =
        engine === 'finetuned'
          ? await api.transcribeSegmentFinetuned(seg.audioPath, seg.alignmentJson)
          : await api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id);
      const text = result.text;
      const updated: SpeechSegment = {
        ...freshRow(seg.id, seg),
        rawTranscript: result.rawTranscript,
        annotatedTranscript: text,
        verified: false,
      };
      // Re-transcribing a reviewed clip is destructive (gold replaced, clip reopened): snapshot the
      // pre-change row so Undo review restores the FULL pre-decision state (lossless, incl. decision
      // columns). Pushed only here — after the ASR succeeded, immediately before the write — so a
      // failed attempt + retry can never double-push.
      if (seg.verified) {
        undoHistory = [...undoHistory, { id: seg.id, prev: { ...seg } }];
      }
      await api.updateSegment(updated);
      segments.update((list) => list.map((s) => (s.id === seg.id ? updated : s)));
      notifications.success($t('review.retranscribed'));
      // The DB/store write above targets seg by id and is correct even if the reviewer navigated away
      // during the multi-second ASR await. But everything below mutates the CURRENTLY shown editor
      // (editText/lastLoadedOriginal/draftModels/consensus/alignment) — if navigation changed `current`
      // mid-flight, applying seg's draft here would put seg's MACHINE text into another clip's editor,
      // and a subsequent Save would persist it as THAT clip's human-verified gold: a wrong-segment gold
      // corruption (THE ONE LAW). Bail; seg's clip reloads its fresh draft + re-aligns when reopened.
      if (current?.id !== seg.id) return;
      editText = text;
      editedChips = {}; // a fresh draft replaces the transcript wholesale — drop stale chip fixes
      // The re-transcribed draft is the new baseline (not a "dirty" edit). Do NOT reset lastLoadedId —
      // the clip id is unchanged, so the load effect must stay a no-op; resetting it would re-run
      // loadConsensus and wipe the provenance badge we set just below.
      lastLoadedOriginal = text;
      // The owner just produced this draft with the chosen engine, so name it on the provenance badge
      // immediately (honest — it's exactly what was used). A single-engine re-transcribe has no
      // multi-model consensus, so hide that card. The fine-tuned button always runs the MMS-1B; the
      // champion button runs the CONFIGURED primary engine (pipeline.transcribe), so read it from
      // settings rather than assuming the 7B — otherwise a user who switched the primary would get a
      // badge naming a model that never ran.
      draftModels = [engine === 'finetuned' ? 'finetuned-mms-ckb' : primaryEngineId()];
      consensus = null;
      await ensureWordTimings(updated);
    } catch (e) {
      // Champion (7B) down: offer retry-or-offline rather than a dead-end. Never a silent downgrade.
      if (engine === 'champion' && api.is7bUnavailableError(e)) {
        showConfirmDialog.set({
          title: $t('asr.championUnavailableTitle'),
          message: $t('asr.championUnavailableMessage'),
          confirmLabel: $t('asr.tryAgain'),
          danger: false,
          onConfirm: () => void doRetranscribe('champion'),
          secondary: { label: $t('asr.useOfflineModel'), onClick: () => void doRetranscribe('finetuned') },
        });
      } else {
        notifications.error($t('review.retranscribeFailed'), { detail: String(e) });
      }
    } finally {
      retranscribing = false;
    }
  }

  // "This draft is wrong" — mark the clip bad so it leaves the review queue and is excluded from
  // dataset export, but the audio + draft are KEPT (reversible: re-review to accept, or re-transcribe).
  // Records a human 'reject' decision (verdict = human_reject) which the export path already honors.
  async function markBad() {
    const seg = current;
    if (!seg || saving || retranscribing || aligning) return;
    // No blocking confirm (true-10 audit): 'x' is undoable via Backspace now, so a native
    // window.confirm per press only broke the keyboard flow.
    saving = true;
    try {
      undoHistory = [...undoHistory, { id: seg.id, prev: { ...seg } }];
      // recordHumanDecision FIRST so a validation failure aborts before updateSegment commits (same
      // ordering rationale as submit()). freshRow AFTER it: same stale-spread rationale as submit().
      await api.recordHumanDecision(seg.id, 'reject', null);
      // P2.3b: targeted field update (verified is whitelisted) — no whole-row upsert of the batch-stale
      // store row, which would revert a concurrent batch's writes to this segment.
      await api.updateSegmentFields(seg.id, { verified: true });
      segments.update((list) => list.map((s) => (s.id === seg.id ? { ...s, verified: true } : s)));
      notifications.success($t('review.markedBad'));
      advance();
    } catch (e) {
      undoHistory = undoHistory.slice(0, -1); // the decision did not persist — drop the phantom entry
      notifications.error($t('notifications.saveFailed'), { detail: String(e) });
    } finally {
      saving = false;
    }
  }

  // True-10 audit: in-loop undo. The global Ctrl+Z only reverts update_segment (not the human
  // decision), leaving split state; this mirrors the inbox instead — clear the decision AND restore
  // the pre-decision segment in one action, then re-land the cursor on the undone clip.
  let undoHistory = $state<{ id: string; prev: SpeechSegment }[]>([]);

  async function undoLast() {
    const last = undoHistory[undoHistory.length - 1];
    if (!last || saving || retranscribing) return;
    saving = true;
    undoHistory = undoHistory.slice(0, -1);
    try {
      // LOSSLESS restore (2026-07-15 fix, reproduced in db::tests::redecision_undo_...): the old
      // clearHumanDecision + updateSegment pair NULLed a PRIOR decision when undoing a REdecision,
      // because updateSegment deliberately omits the decision columns. restoreSegmentSnapshot writes
      // the whole pre-save snapshot — prior decision, verdict fields, escalation state and all — so
      // undo returns the row to its exact pre-decision state in one atomic upsert.
      await api.restoreSegmentSnapshot(last.prev);
      segments.update((list) => list.map((s) => (s.id === last.id ? last.prev : s)));
      editCache.delete(last.id);
      const idx = queue.findIndex((s) => s.id === last.id);
      if (idx >= 0) index = idx;
      notifications.success($t('review.undone'));
    } catch (e) {
      // Not undone — put the entry back so the undo stays retryable.
      undoHistory = [...undoHistory, last];
      notifications.error($t('review.undoFailed'), { detail: String(e) });
    } finally {
      saving = false;
    }
  }
  // Cloud watcher: one Gemini-2.5-Pro listen on THIS clip (the T2 judge, on demand). Gemini hears the
  // audio and checks the draft verbatim — including repeated words ("کە کە") the local ASR often
  // collapses — and returns a corrected transcript + reason. The verdict renders inline; the reviewer
  // stays the decider (a "use this text" click only fills the editor, never auto-verifies).
  let cloudChecking = $state(false);
  let cloudCheck = $state<{ id: string; result: import('./commands').T2Result } | null>(null);
  async function runCloudCheck() {
    const seg = current;
    if (!seg || cloudChecking || saving || retranscribing) return;
    cloudChecking = true;
    cloudCheck = null;
    try {
      const result = await api.runT2ForSegment(seg.id, $settings.llmApiKey);
      cloudCheck = { id: seg.id, result };
    } catch (e) {
      notifications.error($t('review.cloudCheckFailed'), { detail: String(e) });
    } finally {
      cloudChecking = false;
    }
  }

  // A word is "contested" (worth a second look) when under two-thirds weighted agreement.
  const CONTESTED = 0.67;

  // Word-level alignment for the current clip (forced or heuristic). When present it
  // powers the listen-strip: tap a word to hear it, colour the low-confidence ones so
  // the reviewer's eye lands on likely errors, and karaoke-highlight the active word.
  const words = $derived<WordTimestamp[]>(parseWordTimestamps(current?.alignmentJson));

  // The clip's window within its source file. Without this the player plays the WHOLE file
  // and the waveform playhead is whole-file-relative — so you don't hear/see the one sentence
  // you're correcting. Bound the player to [start,end] and show the waveform clip-relative.
  const range = $derived(chunkPlaybackRange(parseSourceMeta(current?.alignmentJson)));
  // Word timestamps from the aligner are CLIP-relative (0-based within the chunk), so compare
  // against currentTime minus the clip's start offset; otherwise an offset chunk never highlights.
  const activeWordIndex = $derived.by(() => {
    const clipT = currentTime - range.startTime;
    return words.findIndex((w) => clipT >= w.start && clipT < w.end);
  });
  const clipLength = $derived(
    range.endTime > range.startTime ? range.endTime - range.startTime : playerDuration,
  );
  const clipPosition = $derived(
    range.endTime > range.startTime
      ? Math.max(0, Math.min(currentTime - range.startTime, clipLength))
      : currentTime,
  );

  // "Play exact words": when word timings exist, bound playback to the spoken span (first word →
  // last word, small pad) so Play hears the words, not the silence/music the VAD chunk padded around
  // them. Falls back to the whole clip when there are no word timings yet.
  const SPOKEN_PAD = 0.12;
  const hasWords = $derived(words.length > 0 && range.endTime > range.startTime);
  const playStart = $derived(
    hasWords ? range.startTime + Math.max(0, words[0].start - SPOKEN_PAD) : range.startTime,
  );
  const playEnd = $derived(
    hasWords
      ? range.startTime + Math.min(clipLength, words[words.length - 1].end + SPOKEN_PAD)
      : range.endTime,
  );

  // Lazily compute + PERSIST forced-alignment word timings the first time a clip is opened without
  // them, so the spoken-span playback + tap-a-word strip light up for the whole existing backlog
  // (alignment reuses the saved transcript + audio; it does NOT re-run ASR). Best-effort: review still
  // works with whole-clip playback if alignment is unavailable.
  let aligning = $state(false);
  const alignAttempted = new Set<string>();
  async function ensureWordTimings(seg: SpeechSegment) {
    // Re-align when timings are MISSING or still the energy heuristic (evenly spaced words that do
    // not track the voice): imported clips always carry heuristic timings, so gating on "has
    // timestamps" alone froze the entire backlog at heuristic quality even after a real CTC aligner
    // was installed. Idempotent: without an aligner the backend re-persists the same heuristic, and
    // alignAttempted stops per-session repeats either way.
    const hasRealTimings =
      parseWordTimestamps(seg.alignmentJson).length > 0 && seg.alignmentQuality !== 'energy_heuristic';
    if (hasRealTimings || alignAttempted.has(seg.id)) return;
    const text = originalText(seg);
    if (!text.trim() || text.includes('[Pending') || text.includes('[ASR unavailable')) return;
    alignAttempted.add(seg.id);
    aligning = true;
    try {
      await api.alignSegment(seg.audioPath, text, seg.alignmentJson ?? null, seg.id);
      await segments.load(); // align_segment persisted the timings; reload so `words` derives them
    } catch {
      // best-effort — leave whole-clip playback in place
    } finally {
      aligning = false;
    }
  }

  function originalText(seg: SpeechSegment): string {
    return seg.annotatedTranscript ?? seg.normalizedTranscript ?? seg.rawTranscript ?? '';
  }

  // Rebuild an upsert payload from the FRESHEST store copy of the row. update_segment writes the
  // WHOLE row (insert_segment ON CONFLICT — including alignment_json and alignment_quality), so
  // spreading a segment captured before a multi-second await lets the background aligner's freshly
  // persisted CTC timings be silently reverted by the stale copy. ensureWordTimings reloads the
  // store right after it persists, so re-reading by id at persist time carries those columns.
  function freshRow(id: string, fallback: SpeechSegment): SpeechSegment {
    return get(segments).find((s) => s.id === id) ?? fallback;
  }

  // Plain (non-reactive) cache of in-progress edits keyed by segment id, so switching clips — via
  // prev/next OR a queue reorder from a concurrent store reload — never silently discards an unsaved
  // correction. Cleared per id on a successful save.
  const editCache = new Map<string, string>();
  let lastLoadedOriginal = '';

  // Load the editable text + waveform whenever the current clip changes.
  $effect(() => {
    const seg = current;
    if (!seg || seg.id === lastLoadedId) return;
    // Stash the OUTGOING clip's unsaved edit before we switch away — but if the user reverted it back
    // to the original, DROP any previously-cached edit so a discarded correction can't resurrect.
    if (lastLoadedId) {
      if (editText.trim() !== lastLoadedOriginal.trim()) {
        editCache.set(lastLoadedId, editText);
      } else {
        editCache.delete(lastLoadedId);
      }
    }
    lastLoadedId = seg.id;
    lastLoadedOriginal = originalText(seg);
    editText = editCache.get(seg.id) ?? lastLoadedOriginal;
    currentTime = 0;
    playing = false;
    editingWordIndex = null; // never carry an open chip editor across clips
    editedChips = {}; // chip fixes are per-clip display state; don't leak them onto the next clip
    loadWaveform(seg);
    void ensureWordTimings(seg);
    void loadConsensus(seg);
  });

  // Drop a stale getWaveform response: switching clips A -> B while A's decode (up to ~30 s for a large
  // source) is still in flight must NOT let A's later-resolving waveform overwrite B's. Last-call-wins via
  // a monotonic sequence, mirroring segmentStore.load()'s loadSeq guard.
  let waveformLoadSeq = 0;
  async function loadWaveform(seg: SpeechSegment) {
    const seq = ++waveformLoadSeq;
    try {
      const data = await api.getWaveform(seg.audioPath, 240, seg.alignmentJson);
      if (seq !== waveformLoadSeq) return; // a newer clip started loading; this response is stale
      waveformData = data;
      waveformError = null;
    } catch (e) {
      if (seq !== waveformLoadSeq) return;
      // Audit 2026-08-05 #5 saw "the top waveform was blank during inspection". This catch used to
      // set [] and say nothing, so a FAILED decode rendered exactly like a silent clip: a reviewer
      // reads a flat strip as "this audio is quiet", not as "the app could not read your file". Same
      // failure-looking-like-success class the read guards in commands.ts exist for, and the reviewer
      // may then verify a clip they never actually saw the shape of.
      waveformData = [];
      waveformError = String(e);
      notifications.error($t('review.waveformFailed'), { detail: waveformError });
    }
  }

  const dirty = $derived(current ? editText.trim() !== originalText(current).trim() : false);

  async function submit(acceptAsIs: boolean) {
    const seg = current;
    // Guard `retranscribing` too (retranscribe/markBad already do): a champion re-transcribe can take
    // several seconds, and accepting/saving mid-flight would record a human decision on the stale
    // pre-retranscribe draft that the in-flight run is about to overwrite — the human "accept" would
    // land on text the reviewer no longer sees.
    if (!seg || saving || retranscribing || aligning) return;
    const original = originalText(seg).trim();
    const text = acceptAsIs ? original : editText.trim();
    // Never save an empty edit (mirrors the Save button's disabled guard — the Ctrl+Enter shortcut
    // would otherwise bypass it, blank the transcript, and split the segment's state).
    if (!acceptAsIs && !text) return;
    // Never let a placeholder ("[Pending WSL 7B ASR]" / "[ASR unavailable…]") be verified as gold: the
    // owner must re-transcribe it or mark it bad. Accepting it would count a non-transcript as reviewed
    // (inflating verified counts) while the export rubric silently drops it. An edit that REPLACES the
    // placeholder with real text is allowed (text is no longer a placeholder).
    if (isPlaceholderTranscript(text)) {
      notifications.error($t('review.cannotVerifyPlaceholder'));
      return;
    }
    saving = true;
    // Map to a real human decision: an actual change is an "edit" (the typed text becomes gold), a
    // no-change save is an "accept".
    const isEdit = !acceptAsIs && text !== original;
    try {
      undoHistory = [...undoHistory, { id: seg.id, prev: { ...seg } }];
      // BOTH calls are required, recordHumanDecision FIRST: it validates the decision (an empty edit
      // throws here) so a failure aborts BEFORE updateSegment commits — no split state where the clip
      // is `verified` with no human_decision (or a blanked transcript). recordHumanDecision records the
      // human_decision (so the jury never re-adjudicates) + feeds the learning flywheel; updateSegment
      // marks `verified` (queue/progress advance) and writes annotated_transcript where the editor +
      // FTS search read it. If updateSegment fails after recordHumanDecision, the clip stays unverified
      // (in the queue) and re-review self-heals it.
      // Accept-what-you-SEE: pass the displayed text even on accept so verdict_transcript (the
      // COALESCE-preferred gold source) becomes exactly what the human approved. Passing null let an
      // UNSEEN jury verdict_transcript survive and get exported as human-verified gold. For an edit the
      // typed text is the label; for an accept the displayed original is. (Backend only captures a
      // LOOP-0 memory / ledger row on 'edit', so an accept with text stays a pure verdict overwrite.)
      await api.recordHumanDecision(seg.id, isEdit ? 'edit' : 'accept', text);
      // Build the upsert AFTER the slow decision call (whole-file hash on 'edit' takes hundreds of
      // ms) from the freshest store row: update_segment writes the whole row, so a pre-await spread
      // would revert timings the background aligner persisted inside that window.
      // P2.3b (audit F2 completeness): persist ONLY the whitelisted fields via the targeted field
      // update — never a whole-row updateSegment of freshRow (the STORE row), which is stale vs the DB
      // while a background batch (normalize/transcribe/rediarize) writes this segment on its own
      // connection, so the upsert reverted the batch's freshly-written columns. A field update touches
      // only annotatedTranscript + verified (both whitelisted), so it is clobber-safe vs a batch AND an
      // aligner. The backend re-reads the fresh row under the db lock (no TOCTOU).
      await api.updateSegmentFields(seg.id, { annotatedTranscript: text, verified: true });
      segments.update((list) =>
        list.map((s) => (s.id === seg.id ? { ...s, annotatedTranscript: text, verified: true } : s)),
      );
      editCache.delete(seg.id); // persisted — drop the in-progress copy
      notifications.success($t('saved'));
      // The DB/store write above targets seg by id and is correct even if the reviewer navigated away during
      // the decision await (an 'edit' hashes the whole file — hundreds of ms). But everything below mutates
      // the CURRENTLY shown editor (editText/lastLoadedOriginal/editedChips) — if navigation changed `current`
      // mid-flight, applying seg's text here would put it into ANOTHER clip's editor (and submit never resets
      // lastLoadedId, so the coalesced load effect no-ops that clip and never reloads its own text), and a
      // subsequent Save would persist seg's text as THAT clip's human-verified gold: a wrong-segment gold
      // corruption (THE ONE LAW). Bail without advancing; seg is already saved and the current clip keeps its
      // own draft. Mirrors doRetranscribe's identical guard.
      if (current?.id !== seg.id) return;
      lastLoadedOriginal = text; // the saved text is now the baseline for dirty-tracking
      editText = text;
      editedChips = {}; // the fixes are now baked into the saved transcript; drop the overlay
      advance();
    } catch (e) {
      undoHistory = undoHistory.slice(0, -1); // the decision did not persist — drop the phantom entry
      notifications.error($t('notifications.saveFailed'), { detail: String(e) });
    } finally {
      saving = false;
    }
  }

  function advance() {
    // After a save the just-verified clip drops to the done tail; jump to the next clip that still
    // needs a human (the first remaining pending). If NONE remain, set index = -1 so `current` resolves
    // to null and the "all done" empty state renders — never clamp back onto an already-verified clip
    // (which would silently re-open finished work for re-editing).
    const nextPending = queue.findIndex((s) => !s.verified);
    index = nextPending >= 0 ? nextPending : -1;
  }
  async function go(delta: number) {
    const target = Math.max(0, Math.min(queue.length - 1, index + delta));
    if (target === index) return;
    // Persist any unsaved edit as a DRAFT before navigating, so the load $effect can't silently discard
    // the reviewer's typed corrections when `current` changes. Navigation is not a verify, so the
    // segment's `verified` state is left untouched; the edit is recoverable and Reset still discards it.
    // A CLEARED textarea is dirty too but must NOT be drafted: persisting annotatedTranscript=''
    // blanks the gold transcript with no undo entry (submit() refuses the same state). The cleared
    // text survives in editCache for this session, so nothing is lost by skipping the persist.
    // P2.3b: persist the navigation DRAFT via the targeted field update (annotatedTranscript only). The
    // old whole-row updateSegment of the store row reverted a concurrent batch's writes to this segment
    // (batch-stale store), and needed a `!aligning` skip to avoid reverting the aligner's timings too. A
    // field update never touches alignmentJson, so it is clobber-safe vs BOTH a batch and an in-flight
    // aligner — no `aligning` skip needed, and the draft is persisted immediately rather than deferred to
    // the unmount flush / editCache.
    if (dirty && current && !saving && editText.trim()) {
      const seg = current;
      const text = editText.trim();
      saving = true;
      try {
        await api.updateSegmentFields(seg.id, { annotatedTranscript: text });
        segments.update((list) => list.map((s) => (s.id === seg.id ? { ...s, annotatedTranscript: text } : s)));
      } catch (e) {
        notifications.error($t('notifications.saveFailed'), { detail: String(e) });
      } finally {
        saving = false;
      }
    }
    index = target;
  }
  function resetToOriginal() {
    if (current) editText = originalText(current);
    editedChips = {}; // revert the chip overlay too, so it can't claim a fix the reset gold lacks
  }

  // Unmount flush (true-10 audit): editCache is component-local, so leaving review mode via the
  // ActivityRail destroyed any in-progress correction with zero warning — the same data-loss class
  // the curate autosave work eliminated. Persist the dirty edit as a DRAFT on teardown, exactly like
  // go() does on navigation (annotatedTranscript only — never verified, so it is not a decision).
  // Root-level $effect: its teardown runs once, on component destroy.
  $effect(() => {
    return () => {
      const seg = current;
      // The empty-edit guard mirrors go() and submit(): a cleared textarea on teardown must not
      // persist annotatedTranscript='' and blank the gold transcript with no undo entry.
      if (!seg || !dirty || saving || !editText.trim()) return;
      const text = editText.trim();
      // P2.3 (audit F2): persist ONLY annotatedTranscript via the TARGETED field update — never the
      // whole-row updateSegment. The old whole-row draft spread the (possibly pre-align) store row, so
      // if a background CTC alignment finished after teardown its real timings were reverted (the
      // whole-row-clobber class). A field update never touches alignmentJson, so it is clobber-safe
      // even mid-align and needs NO `aligning` guard — which would instead LOSE this edit, since
      // editCache dies with the component and teardown cannot re-stash it.
      segments.update((list) => list.map((s) => (s.id === seg.id ? { ...s, annotatedTranscript: text } : s)));
      // Fire-and-forget: teardown cannot await. Surface a failure — the notification store outlives
      // this component — so a lost draft is never silent.
      api.updateSegmentFields(seg.id, { annotatedTranscript: text }).catch((e) => {
        notifications.error($t('notifications.saveFailed'), { detail: String(e) });
      });
    };
  });

  // Transient word-bounded playback window. While set, the player plays exactly
  // [wordStartOverride, wordEndOverride] — so tapping a word plays THAT word (and Loop loops that
  // word, not the whole span, because BOTH bounds are overridden). Cleared whenever playback stops
  // (word finished, pause, clip switch) or on any non-word retarget (replay, waveform seek), so the
  // main Play button always plays the full spoken span.
  let wordStartOverride = $state<number | null>(null);
  let wordEndOverride = $state<number | null>(null);
  function clearWordOverride() {
    wordStartOverride = null;
    wordEndOverride = null;
  }
  $effect(() => {
    if (!playing) clearWordOverride();
  });

  // Which word chip is being edited in place (null = none). Reset on clip change.
  let editingWordIndex = $state<number | null>(null);
  // Display-only overlay of committed chip fixes (index → corrected word). Shows the reviewer's
  // correction on the strip WITHOUT mutating alignment_json — the gold lives in editText; the chip's
  // word text + timings stay the ASR alignment (a listening aid). Reset on clip change / Reset, so
  // Accept-as-is / undo never revert to a chip that claims a fix the gold transcript lacks.
  let editedChips = $state<Record<number, string>>({});
  const chipText = (w: WordTimestamp, i: number) => editedChips[i] ?? w.word;

  // The word-strip container, so an explicit close (Enter/Escape) can restore focus to the chip
  // (WCAG 2.4.3 — a removed <input> otherwise drops focus to <body>). NOT called on blur: a Tab/click
  // away already moved focus correctly, and yanking it back would fight the reviewer.
  let stripEl = $state<HTMLElement | undefined>();
  function refocusChip(i: number) {
    void tick().then(() => stripEl?.querySelector<HTMLButtonElement>(`[data-chip="${i}"]`)?.focus());
  }

  // Single tap / Enter / Space on a word chip = hear EXACTLY that word. Listen-only: it never opens
  // an input or steals keyboard focus, so the single-key review flow keeps working. Word times are
  // clip-relative; wordPlayBounds returns absolute file time, padded + clamped.
  function playWord(w: WordTimestamp) {
    const b = wordPlayBounds(w, range.startTime, range.endTime, SPOKEN_PAD);
    // Idempotent: a double-click dispatches click,click,dblclick — don't hard-reseek the same word
    // 2–3× (an audible stutter) when it is already the active playing window.
    if (playing && wordStartOverride === b.start && wordEndOverride === b.end) return;
    wordStartOverride = b.start;
    wordEndOverride = b.end;
    currentTime = b.start;
    playing = true;
  }

  // Double-tap / F2 on a chip = edit that word inline (and hear it). A DELIBERATE gesture, so a plain
  // listen tap never mounts an input under the reviewer's keystrokes.
  function startWordEdit(w: WordTimestamp, i: number) {
    playWord(w);
    editingWordIndex = i;
  }

  // Commit an inline chip edit into the working transcript (the gold). Empty or unchanged = cancel,
  // never a silent delete. replaceWordToken is STRICT: it rewrites only the confidently-located
  // token (position when the counts corroborate it, else a UNIQUE exact match), else returns null
  // and we fall back to selecting the word in the transcript editor — never guess-rewrite a repeated
  // Sorani word. editText is the only thing persisted; the chip fix is shown via editedChips, so
  // alignment_json never diverges from the gold.
  // Returns true when the caller should restore focus to the chip (clean commit or unchanged cancel);
  // false when it fell back to the transcript editor (editWord moved focus there — leave it).
  function commitWordEdit(i: number, w: WordTimestamp, value: string, viaBlur = false): boolean {
    if (editingWordIndex !== i) return false; // stale blur after Escape/commit already closed it
    editingWordIndex = null;
    const current = chipText(w, i);
    const fix = value.trim();
    if (!fix || fix === current) return true; // unchanged / empty = cancel, no rewrite
    const replaced = replaceWordToken(editText, i, current, fix, words.length);
    if (replaced === null) {
      // Ambiguous / diverged — can't place the fix confidently. On an explicit Enter, jump the human
      // into the transcript editor to place it; on a passive blur, just cancel (don't yank focus).
      if (!viaBlur) editWord(w, i);
      return false;
    }
    editText = replaced;
    editedChips = { ...editedChips, [i]: fix };
    return true;
  }

  function cancelWordEdit(i: number) {
    if (editingWordIndex === i) editingWordIndex = null;
  }

  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  // editWord: select THAT word in the transcript textarea (never rewrites it) — the safe fallback
  // when an inline edit can't be located confidently. Prefer the i-th editor token when it still
  // matches, else the first exact match, else just focus.
  function editWord(w: WordTimestamp, i: number) {
    if (!editEl) return;
    editEl.focus();
    const text = editText;
    const tokens: Array<{ start: number; len: number; word: string }> = [];
    const re = /\S+/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) tokens.push({ start: m.index, len: m[0].length, word: m[0] });
    const wanted = chipText(w, i);
    const target = tokens[i] && tokens[i].word === wanted ? tokens[i] : tokens.find((t) => t.word === wanted);
    if (target) editEl.setSelectionRange(target.start, target.start + target.len);
  }
  function replay() {
    clearWordOverride(); // replay the WHOLE spoken span, not a stale tapped-word window
    currentTime = playStart;
    playing = true;
  }

  // 3-bin confidence → style class (research: discrete bins scan faster than a gradient).
  function confClass(c: number | undefined | null): string {
    if (c == null) return '';
    if (c < 0.6) return 'conf-low';
    if (c < 0.85) return 'conf-mid';
    return '';
  }

  // The transcript editor, so single-key shortcuts can focus it (`e`) and Escape can leave it.
  let editEl = $state<HTMLTextAreaElement | undefined>();

  // Keyboard-first review flow (parity with the Review Inbox): a=accept, e=edit, x=mark-bad,
  // space=play/pause, r=replay, n/→=next, p/←=prev, Ctrl/Cmd+Enter=save & next. Single-key actions
  // NEVER fire while typing in the transcript (or any input), and never hijack space/Enter from a
  // focused button/link — so nothing corrupts the edit text or breaks native control activation.
  function onKeydown(e: KeyboardEvent) {
    // The Review Inbox overlays this surface with its OWN window keydown handler. While it is open,
    // every keystroke belongs to the inbox — firing review-mode actions (accept/reject/undo/save) on
    // the HIDDEN clip behind it silently records human decisions the owner never made, corrupting gold
    // labels. Guard FIRST, before the Ctrl+Enter save, or an inbox edit's Ctrl+Enter also saves+verifies
    // the invisible ReviewMode clip.
    if (get(showReviewInbox)) return;
    // Save & next: works everywhere, including mid-edit.
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      e.preventDefault();
      submit(false);
      return;
    }
    const el = e.target as HTMLElement | null;
    const typing =
      !!el && (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT' || el.isContentEditable);
    if (typing) {
      // Escape drops focus so the single-key review shortcuts resume; otherwise let typing through.
      if (e.key === 'Escape') {
        e.preventDefault();
        editEl?.blur();
      }
      return;
    }
    // Bare single keys only — never steal a browser/OS chord (Ctrl+A, etc.).
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    // Let a focused button/link keep its native space/Enter activation.
    if ((el?.tagName === 'BUTTON' || el?.tagName === 'A') && (e.key === ' ' || e.key === 'Enter')) {
      return;
    }
    // Match on the PHYSICAL key (layout-independent): the owner types Sorani corrections with a
    // Central Kurdish layout active, where e.key is 'ا'/'ب'/… — every advertised letter shortcut
    // (A/E/X/R/N/P) went dead after Escape-ing the textarea until the OS layout was toggled back,
    // once per edited clip. physicalKey maps KeyA→'a' and falls back to e.key for Space/arrows/⌫.
    switch (physicalKey(e)) {
      case 'a':
        e.preventDefault();
        submit(true);
        break;
      case 'e':
        e.preventDefault();
        editEl?.focus();
        break;
      case 'x':
        e.preventDefault();
        void markBad();
        break;
      case ' ':
        e.preventDefault();
        playing = !playing;
        break;
      case 'r':
        e.preventDefault();
        replay();
        break;
      case 'n':
      case 'ArrowRight':
        e.preventDefault();
        void go(1);
        break;
      case 'p':
      case 'ArrowLeft':
        e.preventDefault();
        void go(-1);
        break;
      case 'Backspace':
        e.preventDefault();
        void undoLast();
        break;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if !current}
  <div class="flex h-full items-center justify-center p-6">
    <EmptyState
      variant="empty"
      title={$t('review.allDone')}
      description={searchScoped ? $t('review.searchScopeEmpty') : $t('review.allDoneHint')}
    />
  </div>
{:else}
  {@const isVerified = current.verified}
  <div class="h-full overflow-y-auto">
    <div class="mx-auto flex max-w-3xl flex-col gap-5 px-4 py-6">
      <!-- Completion banner: every clip verified → surface the next steps (export / done). The clips
           stay below so the reviewer can still scrub back and re-check any of them. -->
      {#if progress.allReviewed}
        <div class="card border border-emerald-700/40 bg-emerald-950/20 p-5 text-center" data-testid="review-complete">
          <div class="text-lg font-semibold text-emerald-300">
            {$t('review.completeTitle').replace('{n}', String($segments.length))}
          </div>
          <p class="mt-1 text-sm text-subtle">{$t('review.completeHint')}</p>
          <div class="mt-4 flex flex-wrap justify-center gap-2">
            {#if onExport}
              <button type="button" class="btn btn-primary !text-sm" data-testid="review-complete-export" onclick={onExport}>
                {$t('review.exportDataset')}
              </button>
            {/if}
            <button type="button" class="btn btn-secondary !text-sm" onclick={() => (index = 0)}>
              {$t('review.reviewAgain')}
            </button>
            {#if onDone}
              <button type="button" class="btn btn-secondary !text-sm" onclick={onDone}>
                {$t('review.backToLibrary')}
              </button>
            {/if}
          </div>
        </div>
      {/if}

      {#if searchScoped}
        <!-- Scope is never silent: the reviewer always sees they're on a subset. -->
        <div
          class="rounded-lg border border-amber-600/40 bg-amber-950/30 px-3 py-2 text-xs text-amber-300"
          data-testid="review-scope-banner"
        >
          {$t('review.searchScope')
            .replace('{n}', String(queue.length))
            .replace('{m}', String($segments.length))}
        </div>
      {/if}

      <!-- Progress -->
      <div>
        <div class="flex items-center justify-between gap-3">
          <span class="text-sm font-medium text-muted">
            {$t('review.progress')
              .replace('{n}', String(index + 1))
              .replace('{total}', String(queue.length))}
          </span>
          <div class="flex items-center gap-2">
            <button
              type="button"
              data-testid="suspect-first-toggle"
              onclick={toggleSuspectFirst}
              title={$t('review.suspectFirstHint')}
              aria-pressed={suspectFirst}
              class="rounded-md border px-2 py-1 text-xs transition-colors {suspectFirst
                ? 'border-accent bg-accent/15 text-accent'
                : 'border-surface-3 text-subtle hover:text-muted'}"
            >
              {$t('review.suspectFirst')}
            </button>
            <span class="badge {isVerified ? 'badge-verified' : 'badge-pending'}">
              {isVerified ? $t('verified') : $t('pending')}
            </span>
          </div>
        </div>
        <div class="mt-2 h-1.5 overflow-hidden rounded-full bg-surface-3">
          <div
            class="h-full rounded-full bg-accent transition-all duration-300"
            style="width: {progress.percent}%"
          ></div>
        </div>
        <div class="mt-1 flex items-center justify-between gap-3 text-xs text-subtle">
          <!-- True-10 audit: source-file orientation — in a hundreds-of-clips sitting the reviewer
               had no idea WHICH recording the current clip came from.
               2026-08-05: the chunk position is its own span with a NOUN. It used to render as a bare
               "61/144" glued to the filename, one line under "Clip 1 of 144" and opposite
               "67 of 144 reviewed" — three unlabelled fractions sharing a denominator by coincidence.
               dir="ltr" stays on the FILENAME only; the CKB noun must not be forced into an LTR run. -->
          <span class="flex min-w-0 items-center gap-1.5">
            <span class="truncate" dir="ltr" title={current.audioPath} data-testid="review-source-file">
              {segmentSourceFilename(current.audioPath)}
            </span>
            {#if chunkLabel}
              <span class="shrink-0" data-testid="review-chunk-label"
                >{$t('chunk')} <span dir="ltr">{chunkLabel}</span></span
              >
            {/if}
          </span>
          <span class="shrink-0">
            {$t('review.reviewedCount')
              .replace('{done}', String(progress.done))
              .replace('{total}', String(progress.total))}
          </span>
        </div>
      </div>

      <!-- Waveform -->
      <div class="card overflow-hidden">
        {#if waveformError}
          <!-- A flat strip and a failed decode look identical, and the reviewer would read the flat
               strip as "quiet audio". Say which one it is, in place, and offer the retry. -->
          <div
            class="flex items-center justify-between gap-3 p-3 text-xs text-amber-300"
            data-testid="review-waveform-error"
            role="status"
          >
            <span class="min-w-0 truncate">{$t('review.waveformFailed')}</span>
            <button
              type="button"
              class="btn btn-secondary shrink-0 !text-xs"
              onclick={() => current && loadWaveform(current)}
            >
              {$t('retry')}
            </button>
          </div>
        {:else}
          <Waveform
            waveform={waveformData}
            currentTime={clipPosition}
            duration={clipLength}
            {playing}
            wordTimestamps={words}
            onSeek={(time) => {
              clearWordOverride(); // a manual scrub leaves word-playback mode; play on to the span end
              currentTime = range.startTime + time;
            }}
          />
        {/if}
      </div>

      <!-- Honest playback-scope hint: are we playing just the words, or the whole clip? -->
      <div class="flex items-center gap-2 px-1 text-xs text-subtle" aria-live="polite">
        {#if aligning}
          <span class="inline-block h-3 w-3 animate-spin rounded-full border-2 border-accent border-t-transparent"
          ></span>
          <span>{$t('review.aligningWords')}</span>
        {:else if hasWords}
          <span class="text-accent">●</span>
          <span>{$t('review.playingWordsOnly').replace('{sec}', (playEnd - playStart).toFixed(1))}</span>
        {:else}
          <span>{$t('review.playingWholeClip').replace('{sec}', clipLength.toFixed(1))}</span>
        {/if}
      </div>

      <!-- Audio player — bounded to the SPOKEN SPAN (first→last word) when word timings exist, so Play
           hears the exact words and not the silence/music the VAD chunk padded around them; otherwise
           the whole clip. -->
      <!-- True-10 audit: honor the autoplay setting (it was hardcoded off here while honored in
           curate mode) — with it on, advancing to the next clip auto-plays the bounded spoken span,
           removing one keypress + wait per clip, hundreds of times per review sitting. -->
      <!-- start/endTime honour the transient tap-a-word override so a tapped word plays (and loops)
           exactly itself; otherwise the full spoken span. -->
      <AudioPlayer
        audioPath={current.audioPath}
        clipKey={current.id}
        startTime={wordStartOverride ?? playStart}
        endTime={wordEndOverride ?? playEnd}
        displayStart={playStart}
        displayEnd={playEnd}
        bind:currentTime
        bind:duration={playerDuration}
        bind:playing
        autoplay={$settings.autoplaySegments}
      />

      <!-- Listen-strip: tap a word to hear it; low-confidence words are highlighted -->
      {#if words.length > 0}
        <div class="card p-4">
          <div class="flex items-center justify-between gap-3">
            <div>
              <div class="text-xs font-semibold uppercase tracking-wider text-muted">
                {$t('review.listen')}
              </div>
              <p class="mt-0.5 text-xs text-subtle">{$t('review.listenHint')}</p>
            </div>
            <button
              type="button"
              class="ring-focus shrink-0 rounded-token px-2 py-1 text-xs text-subtle transition-colors hover:text-default"
              onclick={replay}
            >
              ↻ {$t('review.replay')}
            </button>
          </div>
          <div
            bind:this={stripEl}
            dir="rtl"
            class="font-kurdish mt-3 flex flex-wrap items-center gap-x-1 gap-y-2 text-2xl leading-loose"
          >
            {#each words as w, i (i)}
              {#if editingWordIndex === i}
                <!-- Inline chip editor: opened by a DELIBERATE double-tap / F2 (never a plain listen
                     tap). Enter/blur commits (empty or unchanged = cancel), Escape cancels.
                     stopPropagation so Enter/Ctrl+Enter can't leak to the window review shortcuts and
                     save-verify the clip out from under a half-typed fix. -->
                <input
                  type="text"
                  dir="rtl"
                  lang="ckb"
                  class="review-word-input"
                  style={`width: ${Math.max(5, chipText(w, i).length + 3)}ch`}
                  value={chipText(w, i)}
                  aria-label={$t('review.editWordAria')}
                  onblur={(e) => commitWordEdit(i, w, (e.target as HTMLInputElement).value, true)}
                  onkeydown={(e) => {
                    e.stopPropagation();
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      if (commitWordEdit(i, w, (e.target as HTMLInputElement).value)) refocusChip(i);
                    } else if (e.key === 'Escape') {
                      cancelWordEdit(i);
                      refocusChip(i);
                    }
                  }}
                  use:focusOnMount
                />
              {:else}
                <button
                  type="button"
                  data-chip={i}
                  class="review-word {editedChips[i] ? 'word-edited' : confClass(w.confidence)} {i ===
                  activeWordIndex
                    ? 'word-active'
                    : ''}"
                  onclick={() => playWord(w)}
                  ondblclick={() => startWordEdit(w, i)}
                  onkeydown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault();
                      playWord(w);
                    } else if (e.key === 'F2') {
                      e.preventDefault();
                      startWordEdit(w, i);
                    }
                  }}
                  title={`${w.start.toFixed(2)}s · ${Math.round((w.confidence ?? 1) * 100)}% — ${$t('review.wordChipHint')}`}
                >
                  {chipText(w, i)}
                </button>
              {/if}
            {/each}
          </div>
        </div>
      {/if}

      <!-- Consensus draft: an offline best-of-N vote across this clip's ASR models. Contested words
           (the models disagreed) are highlighted so the eye lands on likely errors first; "Use draft"
           starts the edit from a transcript better than any single model. -->
      {#if consensus && consensus.words.length > 0}
        <div class="card space-y-2 p-4">
          <div class="flex items-center justify-between gap-3">
            <div class="text-xs font-semibold uppercase tracking-wider text-muted">
              {$t('review.consensusDraft')}
              <span class="ms-1 font-normal normal-case text-subtle">
                {$t('review.consensusAgree')
                  .replace('{n}', String(consensus.modelCount))
                  .replace('{pct}', String(Math.round(consensus.meanAgreement * 100)))}
              </span>
            </div>
            <button
              type="button"
              class="btn btn-secondary shrink-0 !text-xs"
              onclick={() => {
                if (consensus) {
                  editText = consensus.draft;
                  editedChips = {}; // the draft replaces the transcript — drop stale chip fixes
                }
              }}
            >
              {$t('review.useDraft')}
            </button>
          </div>
          <div class="font-kurdish flex flex-wrap gap-1 text-lg leading-loose" dir="rtl">
            {#each consensus.words as w, i (i)}
              <span
                class="rounded-token px-1.5 {w.agreement < CONTESTED
                  ? 'bg-amber-500/20 text-amber-200'
                  : 'text-default'}"
                title={w.alternatives.length
                  ? $t('review.modelsAlsoSaid').replace('{alts}', w.alternatives.join('  /  '))
                  : ''}>{w.text}</span
              >
            {/each}
          </div>
          <p class="text-[11px] text-subtle">{$t('review.consensusHint')}</p>
        </div>
      {/if}

      <!-- Transcript: big, directly editable -->
      <div class="card p-5">
        <div class="flex items-center justify-between gap-3">
          <div>
            <div class="text-xs font-semibold uppercase tracking-wider text-muted">
              {$t('transcript')}
            </div>
            <p class="mt-0.5 text-xs text-subtle">{$t('review.editHint')}</p>
            {#if draftModels.length > 0}
              <p class="mt-1 text-[11px] text-subtle" dir="ltr">
                {$t('review.draftBy')}
                <span class="font-medium text-muted">{draftModels.map((m) => api.engineLabel(m)).join(', ')}</span>
                {$t('review.notHumanVerified')}
              </p>
            {/if}
          </div>
          {#if dirty}
            <button
              type="button"
              class="ring-focus shrink-0 rounded-token px-2 py-1 text-xs text-subtle transition-colors hover:text-default"
              onclick={resetToOriginal}
            >
              {$t('review.reset')}
            </button>
          {/if}
        </div>
        <textarea
          bind:value={editText}
          bind:this={editEl}
          dir="rtl"
          spellcheck="false"
          class="input font-kurdish mt-3 min-h-[150px] w-full resize-none text-2xl leading-loose"
          placeholder={$t('editTranscript')}
        ></textarea>
      </div>

      <!-- Fix-the-draft tools: the current transcript is wrong -> re-transcribe with a better engine,
           or flag the clip bad (excluded from export, kept + reversible). -->
      <div class="flex flex-wrap items-center gap-2">
        <span class="text-[11px] uppercase tracking-wider text-subtle">{$t('review.retranscribe')}</span>
        <button
          type="button"
          class="btn btn-secondary !text-xs"
          onclick={() => retranscribe('champion')}
          disabled={retranscribing || saving}
          title={$t('review.retranscribeChampionTitle')}
        >
          {retranscribing ? $t('review.retranscribing') : $t('review.retranscribeChampion')}
        </button>
        <button
          type="button"
          class="btn btn-secondary !text-xs"
          onclick={() => retranscribe('finetuned')}
          disabled={retranscribing || saving}
          title={$t('review.retranscribeFinetunedTitle')}
        >
          {$t('review.retranscribeFinetuned')}
        </button>
        {#if $settings.juryCloudOptIn}
          <button
            type="button"
            class="btn btn-secondary !text-xs"
            onclick={() => void runCloudCheck()}
            disabled={cloudChecking || retranscribing || saving}
            title={$t('review.cloudCheckTitle')}
          >
            {cloudChecking ? $t('review.cloudChecking') : $t('review.cloudCheck')}
          </button>
        {/if}
      </div>

      <!-- Cloud watcher verdict: Gemini's audio-grounded reading of THIS clip. Advisory only — the
           reviewer decides; "use this text" fills the editor and still requires Save & next. -->
      {#if cloudCheck && current && cloudCheck.id === current.id}
        {@const verdict = cloudCheck.result.verdict}
        <div class="rounded-md border border-cortex-700/40 bg-cortex-900/40 p-3 space-y-2">
          {#if verdict}
            {#if verdict.transcript.trim() === editText.trim()}
              <p class="text-xs text-emerald-300">
                ✓ {$t('review.cloudCheckAgrees')} ({Math.round(verdict.confidence * 100)}%)
              </p>
            {:else}
              <p dir="rtl" lang="ckb" class="font-mono text-sm text-end">{verdict.transcript}</p>
              <p class="text-[11px] text-subtle">
                {verdict.reason} · {Math.round(verdict.confidence * 100)}% · {verdict.votes}×
              </p>
              <button
                type="button"
                class="btn btn-secondary !text-xs"
                onclick={() => (editText = verdict.transcript)}
              >
                {$t('review.cloudCheckUse')}
              </button>
            {/if}
          {:else}
            <p class="text-xs text-amber-300">
              {$t('review.cloudCheckEscalated')}{cloudCheck.result.error
                ? ` — ${cloudCheck.result.error}`
                : ''}
            </p>
          {/if}
        </div>
      {/if}

      <!-- Decisive actions. STICKY (external review 2026-08-06, P2.2): at 1280x720 this row sat below
           the fold behind the waveform + word-diff + retranscribe tools, so the reviewer had to scroll
           to every single decision. ReviewInbox's .verb-bar already proved the pattern; this reuses it.
           Mark-bad lives here rather than up with the retranscribe tools because it is one of the four
           decisions a reviewer makes about a clip (accept / save / bad audio / undo), not a tool. -->
      <div
        class="review-action-bar flex flex-wrap items-center gap-2"
        role="group"
        aria-label={$t('review.actionsLabel')}
        data-testid="review-action-bar"
      >
        <button
          type="button"
          class="btn btn-secondary"
          onclick={() => go(-1)}
          disabled={index === 0}
          aria-label={$t('prevSegment')}
        >
          {$t('review.prev')}
        </button>
        <button
          type="button"
          class="btn btn-secondary"
          onclick={() => void undoLast()}
          disabled={undoHistory.length === 0 || saving || retranscribing}
          title={$t('review.undoLastTitle')}
        >
          ↩ {$t('review.undoLast')}
        </button>
        <button
          type="button"
          class="btn btn-secondary !text-rose-300 hover:!text-rose-200"
          onclick={markBad}
          disabled={saving || retranscribing}
          title={$t('review.markBadTitle')}
        >
          {$t('review.markBad')}
        </button>
        <div class="flex flex-1 flex-wrap justify-end gap-2">
          <button
            type="button"
            class="btn btn-secondary !py-2.5"
            onclick={() => submit(true)}
            disabled={saving}
          >
            ✓ {$t('review.acceptAsIs')}
          </button>
          <button
            type="button"
            class="btn btn-primary !py-2.5 !text-sm"
            onclick={() => submit(false)}
            disabled={saving || !editText.trim()}
          >
            {$t('review.saveNext')}
          </button>
        </div>
        <!-- Inside the bar, not after it: a sticky element overlays whatever follows it, so a hint left
             outside would be the one thing the bar covers. -->
        <p class="w-full text-center text-[11px] text-subtle">
          {$t('review.kbdHint')}
        </p>
      </div>
    </div>
  </div>
{/if}

<style>
  /* The four decisions (accept / save / bad audio / undo) stay on screen at every viewport.
     `sticky`, not `fixed`: it keeps its space in the flow, so it can never sit on top of the waveform,
     the transcript, or a 200%-zoom reflow — it only pins itself once it would scroll out of view.
     Opaque background + top border so the content scrolling beneath it stays readable, and
     safe-area padding so Windows scaling / an inset-bottom display cannot clip the buttons. */
  .review-action-bar {
    position: sticky;
    bottom: 0;
    z-index: 5;
    background: var(--surface-1);
    border-top: 1px solid var(--border);
    padding-top: 12px;
    padding-bottom: max(8px, env(safe-area-inset-bottom));
    margin-inline: -1rem; /* bleed to the container's px-4 so the backdrop covers the full width */
    padding-inline: 1rem;
  }
  .review-word {
    border-radius: 0.375rem;
    padding: 0.05rem 0.4rem;
    color: var(--text);
    cursor: pointer;
    transition:
      background-color 120ms ease,
      color 120ms ease;
  }
  .review-word:hover {
    background: var(--surface-3);
  }
  /* Low confidence = likely ASR error → draw the eye. Mid = worth a glance. */
  .conf-mid {
    background: color-mix(in srgb, var(--warning) 18%, transparent);
  }
  .conf-low {
    background: color-mix(in srgb, var(--danger) 20%, transparent);
  }
  /* Karaoke highlight of the word currently being heard. Two classes so it always
     wins over the single-class confidence tints. Uses an accent ring + faint tint +
     bold (not an inverted fill) so the word keeps its high-contrast text colour in
     both themes — a solid-accent fill would put light text on a light accent. */
  .review-word.word-active {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    box-shadow: inset 0 0 0 2px var(--accent);
    font-weight: 700;
  }
  /* A chip carrying a committed inline fix (display overlay) — faint success tint so the reviewer
     sees which words they corrected without it competing with the active-word highlight. */
  .review-word.word-edited {
    background: color-mix(in srgb, var(--success, #22c55e) 16%, transparent);
  }
  /* Inline chip editor: same footprint + type as the chip it replaces, accent ring = "editing". */
  .review-word-input {
    font: inherit;
    border-radius: 0.375rem;
    padding: 0.05rem 0.4rem;
    color: var(--text);
    background: var(--surface-3);
    border: none;
    outline: none;
    box-shadow: inset 0 0 0 2px var(--accent);
    text-align: right;
  }
</style>
