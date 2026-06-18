<script lang="ts">
  import { onMount } from 'svelte';
  import { focusTrap } from './actions/focusTrap';
  import * as api from './commands';
  import { showSpeakerPanel } from './stores/uiStore';
  import { notifications } from './stores/notificationStore';
  import { segments } from './stores/segmentStore';

  let speakers = $state<
    { speakerId: string; segmentCount: number; totalDurationSeconds: number }[]
  >([]);
  let loading = $state(true);
  let renamingId = $state<string | null>(null);
  let newName = $state('');

  async function loadSpeakers() {
    loading = true;
    try {
      const stats = await api.getDatasetStats();
      speakers = stats?.topSpeakers ?? [];
    } catch (e) {
      notifications.error('Failed to load speakers: ' + e);
    } finally {
      loading = false;
    }
  }

  async function handleRename(oldId: string) {
    if (!newName.trim()) return;
    try {
      const count = await api.renameSpeaker(oldId, newName.trim());
      notifications.success(`Updated ${count} segments`);
      renamingId = null;
      newName = '';
      await loadSpeakers();
      // Force segments reload
      segments.load();
    } catch (e) {
      notifications.error('Rename failed: ' + e);
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
      <h2 class="text-sm font-bold text-cortex-200 uppercase tracking-widest">
        Speaker Management
      </h2>
      <button class="text-cortex-500 hover:text-cortex-300" onclick={close}>✕</button>
    </header>

    <div class="flex-1 overflow-y-auto p-4 space-y-3">
      {#if loading}
        <div class="animate-pulse space-y-2">
          <div class="h-10 bg-cortex-800 rounded"></div>
          <div class="h-10 bg-cortex-800 rounded"></div>
        </div>
      {:else if speakers.length === 0}
        <p class="text-center py-8 text-cortex-500">No speakers identified yet.</p>
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
                    {speaker.segmentCount} segments &middot; {(
                      speaker.totalDurationSeconds / 60
                    ).toFixed(1)} min
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
                Rename
              </button>
            </div>

            {#if renamingId === speaker.speakerId}
              <div class="flex gap-2 mt-2">
                <input
                  type="text"
                  class="input !text-xs flex-1"
                  bind:value={newName}
                  placeholder="New speaker name..."
                  use:focusOnMount
                />
                <button
                  class="btn btn-primary !text-[10px] !px-2"
                  onclick={() => handleRename(speaker.speakerId)}>Save</button
                >
                <button
                  class="btn btn-secondary !text-[10px] !px-2"
                  onclick={() => (renamingId = null)}>Cancel</button
                >
              </div>
            {/if}
          </div>
        {/each}
      {/if}
    </div>

    <footer class="p-4 border-t border-cortex-800/50 flex justify-end">
      <button class="btn btn-secondary !text-xs" onclick={close}>Close</button>
    </footer>
  </div>
</div>
