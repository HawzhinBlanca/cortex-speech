<script lang="ts">
  import { onMount } from 'svelte';
  import { focusTrap } from './actions/focusTrap';
  import * as api from './commands';
  import { showSpeakerPanel } from './stores/uiStore';
  import { notifications } from './stores/notificationStore';
  import { segments } from './stores/segmentStore';
  import { t } from './i18n';
  import type { SpeakerInventoryItemV1 } from './commands';
  import { historyStore } from './stores/historyStore';

  let speakers = $state<SpeakerInventoryItemV1[]>([]);
  let loading = $state(true);
  // `undefined` means no active editor; `null` is the real SQL NULL/unassigned speaker group.
  let renamingId = $state<string | null | undefined>(undefined);
  let newName = $state('');

  async function loadSpeakers() {
    loading = true;
    try {
      // The COMPLETE speaker list — not stats.topSpeakers, which is truncated to 10, so speakers
      // beyond the top ten were invisible here and could never be renamed.
      speakers = (await api.getSpeakerInventoryV1()) ?? [];
    } catch (e) {
      notifications.error($t('speaker.loadFailed'), { cause: e });
    } finally {
      loading = false;
    }
  }

  function speakerLabel(speakerId: string | null): string {
    return speakerId ?? $t('speaker.unassigned');
  }

  async function handleRename(source: SpeakerInventoryItemV1) {
    const trimmed = newName.trim();
    if (!trimmed) return;
    // A semantic no-op must not update timestamps or create a phantom mutation.
    if (source.speakerId === trimmed) {
      renamingId = undefined;
      newName = '';
      return;
    }
    // A rename whose target ALREADY belongs to another speaker MERGES the two groups (rename is a blanket
    // speaker_id UPDATE). The exact inverse is retained by the backend Undo history, but the box is
    // prefilled free-text, so a typo matching an existing id can still collapse a diarization split.
    // Confirm before the merge, matching the destructive-action pattern elsewhere.
    const mergeTarget = speakers.find(
      (speaker) => speaker.speakerId === trimmed && speaker.speakerId !== source.speakerId,
    );
    if (mergeTarget) {
      const message = $t('speaker.mergeConfirm', {
        source: speakerLabel(source.speakerId),
        target: trimmed,
        n: String(mergeTarget.segmentCount),
      });
      if (!window.confirm(message)) return;
    }
    try {
      const result = await api.renameSpeakerV1({
        sourceSpeakerId: source.speakerId,
        targetSpeakerId: trimmed,
        expectedSourceCount: source.segmentCount,
        expectedTargetCount: mergeTarget?.segmentCount ?? 0,
      });
      notifications.success($t('speaker.renameSuccess', { n: String(result.renamedCount) }));
      renamingId = undefined;
      newName = '';
      await historyStore.refresh();
      await loadSpeakers();
      // Force segments reload
      segments.load();
    } catch (e) {
      notifications.error($t('speaker.renameFailed'), { cause: e });
      // Keep the editor and proposed name intact, but refresh the authoritative counts so the user
      // must explicitly reconfirm any merge against current server truth.
      if (api.isCommandErrorV1(e, 'STALE_SPEAKER_INVENTORY')) await loadSpeakers();
    }
  }

  function close() {
    showSpeakerPanel.set(false);
  }

  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  onMount(loadSpeakers);
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4"
  role="dialog"
  aria-modal="true"
  aria-labelledby="speaker-panel-title"
  tabindex="-1"
  use:focusTrap
  onkeydown={(e) => {
    if (e.key === 'Escape') close();
  }}
  onclick={(e) => {
    if (e.target === e.currentTarget) close();
  }}
>
  <div class="card w-full max-w-lg shadow-2xl flex flex-col max-h-[80vh]">
    <header class="flex items-center justify-between p-4 border-b border-cortex-800/50">
      <h2
        id="speaker-panel-title"
        class="text-sm font-bold text-cortex-200 uppercase tracking-widest"
      >
        {$t('speaker.title')}
      </h2>
      <button class="text-cortex-500 hover:text-cortex-300 text-xs" onclick={close}>
        {$t('close')}
      </button>
    </header>

    <div class="flex-1 overflow-y-auto p-4 space-y-3">
      {#if loading}
        <div class="animate-pulse space-y-2" role="status" aria-label={$t('loading')}>
          <div class="h-10 bg-cortex-800 rounded"></div>
          <div class="h-10 bg-cortex-800 rounded"></div>
        </div>
      {:else if speakers.length === 0}
        <p class="text-center py-8 text-cortex-500">{$t('speaker.noneIdentified')}</p>
      {:else}
        {#each speakers as speaker}
          <div
            class="p-3 bg-cortex-900/50 rounded-lg border border-cortex-800/30 flex flex-col gap-2"
          >
            <div class="flex items-center justify-between">
              <div class="flex items-center gap-2">
                <span
                  class="w-8 h-8 rounded-full bg-cortex-700 flex items-center justify-center text-[10px] font-bold text-cortex-200"
                >
                  {speakerLabel(speaker.speakerId).slice(-2)}
                </span>
                <div>
                  <div class="text-sm font-medium text-cortex-100">
                    {speakerLabel(speaker.speakerId)}
                  </div>
                  <div class="text-[10px] text-cortex-500">
                    {$t('speaker.segmentsMinutes', {
                      count: String(speaker.segmentCount),
                      minutes: (speaker.totalDurationSeconds / 60).toFixed(1),
                    })}
                  </div>
                </div>
              </div>
              <button
                class="text-[10px] text-cortex-400 hover:text-cortex-200 underline"
                onclick={() => {
                  renamingId = speaker.speakerId;
                  newName = speaker.speakerId ?? '';
                }}
              >
                {$t('speaker.rename')}
              </button>
            </div>

            {#if renamingId === speaker.speakerId}
              <div class="flex gap-2 mt-2">
                <input
                  type="text"
                  class="input !text-xs flex-1"
                  bind:value={newName}
                  placeholder={$t('speaker.newNamePlaceholder')}
                  use:focusOnMount
                />
                <button
                  class="btn btn-primary !text-[10px] !px-2"
                  onclick={() => handleRename(speaker)}>{$t('save')}</button
                >
                <button
                  class="btn btn-secondary !text-[10px] !px-2"
                  onclick={() => (renamingId = undefined)}>{$t('cancel')}</button
                >
              </div>
            {/if}
          </div>
        {/each}
      {/if}
    </div>

    <footer class="p-4 border-t border-cortex-800/50 flex justify-end">
      <button class="btn btn-secondary !text-xs" onclick={close}>{$t('close')}</button>
    </footer>
  </div>
</div>
