<script lang="ts">
  import { segments } from './stores/segmentStore';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { t } from './i18n';
  import Waveform from './Waveform.svelte';
  import AudioPlayer from './AudioPlayer.svelte';
  import EmptyState from './EmptyState.svelte';
  import type { SpeechSegment } from './types';

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
    // Re-derive: after marking verified the item moves to the "done" tail, so the
    // next pending naturally surfaces at the same-ish index; clamp to range.
    if (index < queue.length - 1) index = Math.min(index, queue.length - 1);
    else index = Math.max(0, queue.length - 1);
  }
  function go(delta: number) {
    index = Math.max(0, Math.min(queue.length - 1, index + delta));
  }
  function resetToOriginal() {
    if (current) editText = originalText(current);
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
          {currentTime}
          duration={playerDuration}
          wordTimestamps={[]}
          onSeek={(time) => (currentTime = time)}
        />
      </div>

      <!-- Audio player -->
      <AudioPlayer
        audioPath={current.audioPath}
        bind:currentTime
        bind:duration={playerDuration}
        bind:playing
        autoplay={false}
      />

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
