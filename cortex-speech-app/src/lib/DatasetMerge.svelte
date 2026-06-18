<script lang="ts">
  import * as api from './commands';
  import { focusTrap } from './actions/focusTrap';
  import { showDatasetMerge } from './stores/uiStore';
  import { notifications } from './stores/notificationStore';
  import { segments } from './stores/segmentStore';
  import { t } from './i18n';

  let jsonContent = $state('');
  let merging = $state(false);

  async function handleMerge() {
    if (!jsonContent.trim()) {
      notifications.error($t('merge.pasteRequired'));
      return;
    }

    merging = true;
    try {
      const result = await api.mergeDatasetJson(jsonContent.trim());
      notifications.success(
        $t('merge.complete', {
          created: String(result.created),
          updated: String(result.updated),
        }),
      );
      jsonContent = '';
      showDatasetMerge.set(false);
      segments.load();
    } catch (e) {
      notifications.error($t('merge.failed'), { detail: String(e) });
    } finally {
      merging = false;
    }
  }

  function close() {
    showDatasetMerge.set(false);
  }
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
  <div class="card w-full max-w-2xl shadow-2xl flex flex-col max-h-[80vh]">
    <header class="flex items-center justify-between p-4 border-b border-cortex-800/50">
      <h2 class="text-sm font-bold text-cortex-200 uppercase tracking-widest">{$t('merge.title')}</h2>
      <button class="text-cortex-500 hover:text-cortex-300" onclick={close}>✕</button>
    </header>

    <div class="flex-1 p-4 space-y-4 flex flex-col overflow-hidden">
      <p class="text-xs text-cortex-400">
        {$t('merge.bodyHint')}
      </p>

      <textarea
        class="flex-1 bg-cortex-900 border border-cortex-800 rounded-lg p-3 text-[10px] font-mono text-cortex-300 focus:outline-none focus:border-cortex-600 resize-none"
        placeholder={'[{"id": "...", "rawTranscript": "..." }, ...]'}
        bind:value={jsonContent}
        spellcheck="false"
      ></textarea>
    </div>

    <footer class="p-4 border-t border-cortex-800/50 flex justify-end gap-2">
      <button class="btn btn-secondary !text-xs" onclick={close} disabled={merging}
        >{$t('cancel')}</button
      >
      <button class="btn btn-primary !text-xs" onclick={handleMerge} disabled={merging}>
        {merging ? $t('merge.merging') : $t('merge.title')}
      </button>
    </footer>
  </div>
</div>
