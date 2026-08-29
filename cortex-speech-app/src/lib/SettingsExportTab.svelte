<script lang="ts">
  import { t } from './i18n';
  import { isProcessing } from './stores/uiStore';
  import type { AppSettings } from './stores/settingsStore';

  let {
    settings = $bindable(),
    tauriAvailable,
    exportingAudio,
    onExportAudio,
  }: {
    settings: AppSettings;
    tauriAvailable: boolean;
    exportingAudio: boolean;
    onExportAudio: () => void;
  } = $props();
</script>

<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('exportFormat')}</span>
  <select class="input flex-1" bind:value={settings.exportFormat}>
    <option value="json">{$t('settings.exportJson')}</option>
    <option value="jsonl">{$t('settings.exportJsonl')}</option>
    <option value="csv">CSV</option>
    <option value="parquet">Parquet</option>
  </select>
</label>
<div class="pt-2 border-t border-cortex-800/50 space-y-2">
  <p class="text-xs text-cortex-400">{$t('exportAudio.description')}</p>
  <button
    class="btn btn-secondary !text-xs"
    onclick={onExportAudio}
    disabled={!tauriAvailable || exportingAudio || $isProcessing}
    title={tauriAvailable ? $t('exportAudio.label') : $t('desktopRuntimeRequired')}
  >
    {exportingAudio ? $t('exportAudio.progress') : $t('exportAudio.label')}
  </button>
</div>
