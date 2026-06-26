<script lang="ts">
  import { segments } from './stores/segmentStore';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { t } from './i18n';
  import Waveform from './Waveform.svelte';
  import AudioPlayer from './AudioPlayer.svelte';
  import EmptyState from './EmptyState.svelte';
  import { parseWordTimestamps, parseSourceMeta, chunkPlaybackRange } from './alignment';
  import type { SpeechSegment, WordTimestamp } from './types';

  // Simple, focused review queue: one clip at a time. Pending (unverified) first,
  // then the rest — so a reviewer always lands on work that needs doing.
  const queue = $derived.by<SpeechSegment[]>(() => {
    const all = $segments;
    const pending = all.filter((s) => !s.verified);
    const done = all.filter((s) => s.verified);
    return [...pending, ...done];
  });

  let index = $state(0);
  const current = $derived(queue[index] ?? null);
  const reviewedCount = $derived($segments.filter((s) => s.verified).length);

  let editText = $state('');
  let waveformData = $state<number[]>([]);
  let currentTime = $state(0);
  let playerDuration = $state(0);
  let playing = $state(false);
  let saving = $state(false);
  let lastLoadedId = $state<string | null>(null);

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

  function originalText(seg: SpeechSegment): string {
    return seg.annotatedTranscript ?? seg.normalizedTranscript ?? seg.rawTranscript ?? '';
  }

  // Load the editable text + waveform whenever the current clip changes.
  $effect(() => {
    const seg = current;
    if (!seg || seg.id === lastLoadedId) return;
    lastLoadedId = seg.id;
    editText = originalText(seg);
    currentTime = 0;
    playing = false;
    loadWaveform(seg);
  });

  async function loadWaveform(seg: SpeechSegment) {
    try {
      waveformData = await api.getWaveform(seg.audioPath, 240, seg.alignmentJson);
    } catch {
      waveformData = [];
    }
  }

  const dirty = $derived(current ? editText.trim() !== originalText(current).trim() : false);
  const pct = $derived($segments.length ? Math.round((reviewedCount / $segments.length) * 100) : 0);

  async function submit(acceptAsIs: boolean) {
    const seg = current;
    if (!seg || saving) return;
    saving = true;
    const text = acceptAsIs ? originalText(seg) : editText.trim();
    const updated: SpeechSegment = { ...seg, annotatedTranscript: text, verified: true };
    try {
      await api.updateSegment(updated);
      segments.update((list) => list.map((s) => (s.id === seg.id ? updated : s)));
      notifications.success($t('saved'));
      advance();
    } catch (e) {
      notifications.error($t('notifications.saveFailed'), { detail: String(e) });
    } finally {
      saving = false;
    }
  }

  function advance() {
    // After a save the just-verified clip drops to the done tail; jump to the next
    // clip that still needs a human (the first remaining pending), else stay put.
    const nextPending = queue.findIndex((s) => !s.verified);
    index = nextPending >= 0 ? nextPending : Math.min(index, Math.max(0, queue.length - 1));
  }
  function go(delta: number) {
    index = Math.max(0, Math.min(queue.length - 1, index + delta));
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
  function replay() {
    currentTime = range.startTime;
    playing = true;
  }

  // 3-bin confidence → style class (research: discrete bins scan faster than a gradient).
  function confClass(c: number | undefined | null): string {
    if (c == null) return '';
    if (c < 0.6) return 'conf-low';
    if (c < 0.85) return 'conf-mid';
    return '';
  }

  function onKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      e.preventDefault();
      submit(false);
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
      <!-- Progress -->
      <div>
        <div class="flex items-center justify-between gap-3">
          <span class="text-sm font-medium text-muted">
            {$t('review.progress')
              .replace('{n}', String(index + 1))
              .replace('{total}', String(queue.length))}
          </span>
          <span class="badge {isVerified ? 'badge-verified' : 'badge-pending'}">
            {isVerified ? $t('verified') : $t('pending')}
          </span>
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

      <!-- Audio player — bounded to this clip so Play hears only this sentence. -->
      <AudioPlayer
        audioPath={current.audioPath}
        startTime={range.startTime}
        endTime={range.endTime}
        bind:currentTime
        bind:duration={playerDuration}
        bind:playing
        autoplay={false}
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
                title={`${w.start.toFixed(2)}s · ${Math.round((w.confidence ?? 1) * 100)}%`}
              >
                {w.word}
              </button>
            {/each}
          </div>
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
          dir="rtl"
          spellcheck="false"
          class="input font-kurdish mt-3 min-h-[150px] w-full resize-none text-2xl leading-loose"
          placeholder={$t('editTranscript')}
        ></textarea>
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
