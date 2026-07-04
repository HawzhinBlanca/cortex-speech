<script lang="ts">
  import { get } from 'svelte/store';
  import { segments, selectedSegmentId } from './stores/segmentStore';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { settings } from './stores/settingsStore';
  import { t } from './i18n';
  import Waveform from './Waveform.svelte';
  import AudioPlayer from './AudioPlayer.svelte';
  import EmptyState from './EmptyState.svelte';
  import { parseWordTimestamps, parseSourceMeta, chunkPlaybackRange } from './alignment';
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

  // Simple, focused review queue: one clip at a time. Pending (unverified) first,
  // then the rest — so a reviewer always lands on work that needs doing.
  const queue = $derived.by<SpeechSegment[]>(() => {
    const all = $segments;
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
      if (pos >= 0) index = pos;
    }
    cursorRestored = true;
  });
  const current = $derived(queue[index] ?? null);
  const reviewedCount = $derived($segments.filter((s) => s.verified).length);
  // Every clip verified — surface the "you're done, here's what's next" completion banner.
  const allReviewed = $derived($segments.length > 0 && reviewedCount === $segments.length);

  let editText = $state('');
  let waveformData = $state<number[]>([]);
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
    const map: Record<string, string> = {
      'wsl-7b': 'omniasr-wsl-7b',
      'ctc-1b': 'omniasr-ctc-1b',
      'ctc-300m': 'omniasr-ctc-300m',
    };
    return map[get(settings).asrModel] ?? 'omniasr-wsl-7b';
  }

  // Re-transcribe THIS clip with a chosen engine when the current draft is wrong. 'champion' routes
  // through the configured primary engine (the OmniASR-7B Champion by default — needs its server up);
  // 'finetuned' runs the embedded fine-tuned MMS-1B (CPU/ONNX, always available). A re-transcription is
  // machine output, so verified is reset (it must never be kept as if a human confirmed it).
  let retranscribing = $state(false);
  async function retranscribe(engine: 'champion' | 'finetuned') {
    const seg = current;
    if (!seg || retranscribing || saving) return;
    retranscribing = true;
    try {
      const result =
        engine === 'finetuned'
          ? await api.transcribeSegmentFinetuned(seg.audioPath, seg.alignmentJson)
          : await api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id);
      const text = result.text;
      const updated: SpeechSegment = {
        ...seg,
        rawTranscript: result.rawTranscript,
        annotatedTranscript: text,
        verified: false,
      };
      await api.updateSegment(updated);
      segments.update((list) => list.map((s) => (s.id === seg.id ? updated : s)));
      editText = text;
      // The re-transcribed draft is the new baseline (not a "dirty" edit). Do NOT reset lastLoadedId —
      // the clip id is unchanged, so the load effect must stay a no-op; resetting it would re-run
      // loadConsensus and wipe the provenance badge we set just below.
      lastLoadedOriginal = text;
      notifications.success($t('review.retranscribed'));
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
      notifications.error($t('review.retranscribeFailed'), { detail: String(e) });
    } finally {
      retranscribing = false;
    }
  }

  // "This draft is wrong" — mark the clip bad so it leaves the review queue and is excluded from
  // dataset export, but the audio + draft are KEPT (reversible: re-review to accept, or re-transcribe).
  // Records a human 'reject' decision (verdict = human_reject) which the export path already honors.
  async function markBad() {
    const seg = current;
    if (!seg || saving || retranscribing) return;
    // No blocking confirm (true-10 audit): 'x' is undoable via Backspace now, so a native
    // window.confirm per press only broke the keyboard flow.
    saving = true;
    try {
      undoHistory = [...undoHistory, { id: seg.id, prev: { ...seg } }];
      // recordHumanDecision FIRST so a validation failure aborts before updateSegment commits (same
      // ordering rationale as submit()).
      await api.recordHumanDecision(seg.id, 'reject', null);
      const updated: SpeechSegment = { ...seg, verified: true };
      await api.updateSegment(updated);
      segments.update((list) => list.map((s) => (s.id === seg.id ? updated : s)));
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
    if (!last || saving) return;
    saving = true;
    undoHistory = undoHistory.slice(0, -1);
    try {
      await api.clearHumanDecision(last.id);
      await api.updateSegment(last.prev);
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
    if (parseWordTimestamps(seg.alignmentJson).length > 0 || alignAttempted.has(seg.id)) return;
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
    } catch {
      if (seq === waveformLoadSeq) waveformData = [];
    }
  }

  const dirty = $derived(current ? editText.trim() !== originalText(current).trim() : false);
  const pct = $derived($segments.length ? Math.round((reviewedCount / $segments.length) * 100) : 0);

  async function submit(acceptAsIs: boolean) {
    const seg = current;
    if (!seg || saving) return;
    const original = originalText(seg).trim();
    const text = acceptAsIs ? original : editText.trim();
    // Never save an empty edit (mirrors the Save button's disabled guard — the Ctrl+Enter shortcut
    // would otherwise bypass it, blank the transcript, and split the segment's state).
    if (!acceptAsIs && !text) return;
    saving = true;
    // Map to a real human decision: an actual change is an "edit" (the typed text becomes gold), a
    // no-change save is an "accept".
    const isEdit = !acceptAsIs && text !== original;
    const updated: SpeechSegment = { ...seg, annotatedTranscript: text, verified: true };
    try {
      undoHistory = [...undoHistory, { id: seg.id, prev: { ...seg } }];
      // BOTH calls are required, recordHumanDecision FIRST: it validates the decision (an empty edit
      // throws here) so a failure aborts BEFORE updateSegment commits — no split state where the clip
      // is `verified` with no human_decision (or a blanked transcript). recordHumanDecision records the
      // human_decision (so the jury never re-adjudicates) + feeds the learning flywheel; updateSegment
      // marks `verified` (queue/progress advance) and writes annotated_transcript where the editor +
      // FTS search read it. If updateSegment fails after recordHumanDecision, the clip stays unverified
      // (in the queue) and re-review self-heals it.
      await api.recordHumanDecision(seg.id, isEdit ? 'edit' : 'accept', isEdit ? text : null);
      await api.updateSegment(updated);
      segments.update((list) => list.map((s) => (s.id === seg.id ? updated : s)));
      editCache.delete(seg.id); // persisted — drop the in-progress copy
      lastLoadedOriginal = text; // the saved text is now the baseline for dirty-tracking
      editText = text;
      notifications.success($t('saved'));
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
    if (dirty && current && !saving) {
      const seg = current;
      const draft: SpeechSegment = { ...seg, annotatedTranscript: editText.trim() };
      saving = true;
      try {
        await api.updateSegment(draft);
        segments.update((list) => list.map((s) => (s.id === seg.id ? draft : s)));
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
  }

  // Tap a word → seek there and play, so a reviewer can verify it by ear instantly. Word times are
  // clip-relative; add the clip's start offset to land at the right place in the source file.
  function playFromWord(w: WordTimestamp) {
    currentTime = range.startTime + w.start;
    playing = true;
  }
  // Double-tap a word → jump into the editor with THAT word selected, so the reviewer types the fix
  // immediately. This only SELECTS text (never rewrites it), so it can't corrupt an edited transcript:
  // prefer the i-th editor token when the alignment is intact, else the first exact match, else just
  // focus. The single-click play still fires (you hear the word, then correct it).
  function editWord(w: WordTimestamp, i: number) {
    if (!editEl) return;
    editEl.focus();
    const text = editText;
    // Offsets of each non-whitespace token, in order.
    const tokens: Array<{ start: number; len: number; word: string }> = [];
    const re = /\S+/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) tokens.push({ start: m.index, len: m[0].length, word: m[0] });
    let target = tokens[i] && tokens[i].word === w.word ? tokens[i] : tokens.find((t) => t.word === w.word);
    if (target) {
      editEl.setSelectionRange(target.start, target.start + target.len);
    }
  }
  function replay() {
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
    switch (e.key) {
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
    <EmptyState variant="empty" title={$t('review.allDone')} description={$t('review.allDoneHint')} />
  </div>
{:else}
  {@const isVerified = current.verified}
  <div class="h-full overflow-y-auto">
    <div class="mx-auto flex max-w-3xl flex-col gap-5 px-4 py-6">
      <!-- Completion banner: every clip verified → surface the next steps (export / done). The clips
           stay below so the reviewer can still scrub back and re-check any of them. -->
      {#if allReviewed}
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
          <div class="h-full rounded-full bg-accent transition-all duration-300" style="width: {pct}%"></div>
        </div>
        <div class="mt-1 text-end text-xs text-subtle">
          {$t('review.reviewedCount')
            .replace('{done}', String(reviewedCount))
            .replace('{total}', String($segments.length))}
        </div>
      </div>

      <!-- Waveform -->
      <div class="card overflow-hidden">
        <Waveform
          waveform={waveformData}
          currentTime={clipPosition}
          duration={clipLength}
          {playing}
          wordTimestamps={words}
          onSeek={(time) => (currentTime = range.startTime + time)}
        />
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
      <AudioPlayer
        audioPath={current.audioPath}
        startTime={playStart}
        endTime={playEnd}
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
            dir="rtl"
            class="font-kurdish mt-3 flex flex-wrap items-center gap-x-1 gap-y-2 text-2xl leading-loose"
          >
            {#each words as w, i (i)}
              <button
                type="button"
                class="review-word {confClass(w.confidence)} {i === activeWordIndex
                  ? 'word-active'
                  : ''}"
                onclick={() => playFromWord(w)}
                ondblclick={() => editWord(w, i)}
                title={`${w.start.toFixed(2)}s · ${Math.round((w.confidence ?? 1) * 100)}% — tap to hear, double-tap to edit`}
              >
                {w.word}
              </button>
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
                if (consensus) editText = consensus.draft;
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
        <button
          type="button"
          class="btn btn-secondary ms-auto !text-xs !text-rose-300 hover:!text-rose-200"
          onclick={markBad}
          disabled={saving || retranscribing}
          title={$t('review.markBadTitle')}
        >
          {$t('review.markBad')}
        </button>
      </div>

      <!-- Actions -->
      <div class="flex flex-wrap items-center gap-2">
        <button
          type="button"
          class="btn btn-secondary"
          onclick={() => go(-1)}
          disabled={index === 0}
          aria-label={$t('prevSegment')}
        >
          {$t('review.prev')}
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
      </div>
      <p class="text-center text-[11px] text-subtle">
        {$t('review.kbdHint')}
      </p>
    </div>
  </div>
{/if}

<style>
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
</style>
