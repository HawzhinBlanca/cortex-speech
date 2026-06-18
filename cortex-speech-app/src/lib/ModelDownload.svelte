<script lang="ts">
  import { onMount } from 'svelte';
  import { listen, type Event } from '@tauri-apps/api/event';
  import { modelsStatus, modelsDownloadAll } from './commands';
  import { notifications } from './stores/notificationStore';
  import { isTauriRuntime } from './runtime';
  import { t } from './i18n';

  type ModelStatus = Awaited<ReturnType<typeof modelsStatus>>[number];

  type ModelDownloadProgress =
    | { type: 'started'; total: number }
    | { type: 'progress'; current: number; status: string; filename?: string; progress: number }
    | { type: 'completed' };

  let models = $state<ModelStatus[]>([]);
  let downloading = $state(false);
  let loading = $state(true);
  let overallProgress = $state({ current: 0, total: 0, status: '' });
  let modelProgress = $state<Record<string, number>>({});
  const tauriAvailable = isTauriRuntime();

  async function loadStatus() {
    if (!tauriAvailable) {
      models = [];
      loading = false;
      return;
    }
    loading = true;
    try {
      models = await modelsStatus();
    } catch (e: unknown) {
      notifications.error('Failed to check models: ' + e);
    } finally {
      loading = false;
    }
  }

  async function downloadAll() {
    if (!tauriAvailable) {
      notifications.info($t('desktopRuntimeRequired'));
      return;
    }
    downloading = true;
    modelProgress = {};
    try {
      const result = await modelsDownloadAll();
      if (result.total === 0 && result.skipped > 0) {
        notifications.info('No verified model downloads available', {
          detail: `${result.skipped} missing optional model files require a pinned checksum before automatic download.`,
        });
      } else if (result.failed > 0) {
        notifications.warning('Model download completed with failures', {
          detail: `${result.downloaded} downloaded, ${result.failed} failed, ${result.skipped} skipped.`,
        });
      } else {
        notifications.success('Verified model downloads completed', {
          detail:
            result.skipped > 0
              ? `${result.skipped} unavailable model files were skipped.`
              : undefined,
        });
      }
      await loadStatus();
    } catch (e: unknown) {
      notifications.error('Download failed: ' + e);
    } finally {
      downloading = false;
      overallProgress = { current: 0, total: 0, status: '' };
    }
  }

  onMount(() => {
    if (!tauriAvailable) {
      loading = false;
      return;
    }
    loadStatus();

    const unlisten = listen<ModelDownloadProgress>(
      'model-download-progress',
      (event: Event<ModelDownloadProgress>) => {
        const payload = event.payload;
        if (payload.type === 'started') {
          overallProgress.total = payload.total;
          overallProgress.current = 0;
        } else if (payload.type === 'progress') {
          overallProgress.current = payload.current;
          overallProgress.status = payload.status;
          if (payload.filename) {
            modelProgress[payload.filename] = payload.progress;
          }
        } else if (payload.type === 'completed') {
          downloading = false;
        }
      },
    );

    return () => {
      unlisten.then((u) => u());
    };
  });
</script>

<div class="p-4 space-y-3">
  <div class="flex items-center justify-between">
    <h3 class="text-sm font-semibold text-cortex-200">AI Models</h3>
    <button
      class="text-xs px-3 py-1 bg-cortex-700 hover:bg-cortex-600 rounded transition-colors disabled:opacity-50"
      onclick={downloadAll}
      disabled={!tauriAvailable || downloading}
      title={tauriAvailable ? 'Download All' : $t('desktopRuntimeRequired')}
    >
      {downloading ? 'Downloading...' : 'Download All'}
    </button>
  </div>

  {#if downloading && overallProgress.total > 0}
    <div class="p-2 bg-cortex-800/40 rounded-lg border border-cortex-700/50 space-y-1.5">
      <div class="flex justify-between text-[10px]">
        <span class="text-cortex-300">{overallProgress.status || 'Downloading...'}</span>
        <span class="text-cortex-500">{overallProgress.current} / {overallProgress.total}</span>
      </div>
      <div class="h-1 bg-cortex-900 rounded-full overflow-hidden">
        <div
          class="h-full bg-cortex-400 transition-all duration-300"
          style="width: {(overallProgress.current / overallProgress.total) * 100}%"
        ></div>
      </div>
    </div>
  {/if}

  {#if loading}
    <div class="animate-pulse space-y-2">
      <div class="h-8 bg-cortex-800 rounded"></div>
      <div class="h-8 bg-cortex-800 rounded"></div>
    </div>
  {:else}
    {#each models as model}
      <div
        class="flex flex-col gap-1.5 p-2 bg-cortex-900/50 rounded border border-transparent hover:border-cortex-800/50 transition-colors"
      >
        <div class="flex items-center gap-2 text-xs">
          {#if model.downloaded}
            <span class="text-emerald-400">✓</span>
          {:else}
            <span class="text-cortex-500">○</span>
          {/if}
          <span class="flex-1 text-cortex-300">{model.name}</span>
          <span class="text-cortex-500 text-[10px]">
            {#if model.size_bytes}
              {(model.size_bytes / 1048576).toFixed(1)} MB
            {:else}
              Not downloaded
            {/if}
          </span>
        </div>
        {#if modelProgress[model.filename] !== undefined && !model.downloaded}
          <div class="h-0.5 bg-cortex-900 rounded-full overflow-hidden">
            <div
              class="h-full bg-cortex-500 transition-all"
              style="width: {modelProgress[model.filename] * 100}%"
            ></div>
          </div>
        {/if}
      </div>
    {/each}
  {/if}
</div>
