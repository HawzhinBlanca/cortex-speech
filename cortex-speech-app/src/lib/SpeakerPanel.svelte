<script lang="ts">
  import { onMount } from 'svelte';
  import { focusTrap } from './actions/focusTrap';
  import * as api from './commands';
  import { showSpeakerPanel } from './stores/uiStore';
  import { notifications } from './stores/notificationStore';
  import { segments } from './stores/segmentStore';
  import { t } from './i18n';

  let speakers = $state<
    { speakerId: string; segmentCount: number; totalDurationSeconds: number }[]
  >([]);
  let loading = $state(true);
  let renamingId = $state<string | null>(null);
  let newName = $state('');

  async function loadSpeakers() {
    loading = true;
    try {
      // The COMPLETE speaker list — not stats.topSpeakers, which is truncated to 10, so speakers
      // beyond the top ten were invisible here and could never be renamed.
      speakers = (await api.getSpeakers()) ?? [];
    } catch (e) {
      notifications.error($t('speaker.loadFailed'), { cause: e });
    } finally {
      loading = false;
    }
  }

  async function handleRename(oldId: string) {
    const trimmed = newName.trim();
    if (!trimmed) return;
    // A rename whose target ALREADY belongs to another speaker MERGES the two groups (rename is a blanket
    // speaker_id UPDATE, and — unlike delete — records nothing on the undo stack, so the merge is
    // irreversible). The box is prefilled free-text, so a typo matching an existing id silently collapses a
    // diarization split. Confirm before the merge, matching the destructive-action pattern elsewhere.
    const mergeTarget = speakers.find((s) => s.speakerId === trimmed && s.speakerId !== oldId);
    if (mergeTarget) {
      const message = $t('speaker.mergeConfirm', {
        source: oldId,
        target: trimmed,
        n: String(mergeTarget.segmentCount),
      });
      if (!window.confirm(message)) return;
    }
    try {
      const count = await api.renameSpeaker(oldId, trimmed);
      notifications.success($t('speaker.renameSuccess', { n: String(count) }));
      renamingId = null;
      newName = '';
      await loadSpeakers();
      // Force segments reload
      segments.load();
    } catch (e) {
      notifications.error($t('speaker.renameFailed'), { cause: e });
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
                  {speaker.speakerId.slice(-2)}
                </span>
                <div>
                  <div class="text-sm font-medium text-cortex-100">{speaker.speakerId}</div>
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
                  newName = speaker.speakerId;
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
                  onclick={() => handleRename(speaker.speakerId)}>{$t('save')}</button
                >
                <button
                  class="btn btn-secondary !text-[10px] !px-2"
                  onclick={() => (renamingId = null)}>{$t('cancel')}</button
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
