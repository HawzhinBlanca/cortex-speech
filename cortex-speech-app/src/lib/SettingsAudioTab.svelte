<script lang="ts">
  import { t } from './i18n';
  import type { AppSettings } from './stores/settingsStore';

  let { settings = $bindable() }: { settings: AppSettings } = $props();
</script>

<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('vadThreshold')}</span>
  <input
    type="range"
    min="0"
    max="1"
    step="0.05"
    bind:value={settings.vadThreshold}
    class="flex-1 accent-cortex-500"
  />
  <span class="text-xs font-mono text-cortex-300 w-8 text-end">{settings.vadThreshold}</span>
</label>
<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('minSegment')}</span>
  <input type="number" bind:value={settings.minSegmentSec} class="input w-20" min="1" max="60" />
  <span class="text-xs text-cortex-400">{$t('seconds')}</span>
</label>
<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('maxSegment')}</span>
  <input type="number" bind:value={settings.maxSegmentSec} class="input w-20" min="1" max="300" />
  <span class="text-xs text-cortex-400">{$t('seconds')}</span>
</label>
<label class="flex items-center gap-3 cursor-pointer">
  <input type="checkbox" bind:checked={settings.enableDenoising} class="accent-cortex-500" />
  <span class="text-sm text-muted">{$t('settings.aiAudioCleanup')}</span>
</label>
<label class="flex items-center gap-3 cursor-pointer">
  <input type="checkbox" bind:checked={settings.enableDiarization} class="accent-cortex-500" />
  <span class="text-sm text-muted">{$t('enableDiarization')}</span>
</label>
<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('maxSpeakers')}</span>
  <input type="number" bind:value={settings.maxSpeakers} class="input w-20" min="1" max="32" />
</label>
<label class="flex items-center gap-3 cursor-pointer">
  <input
    type="checkbox"
    bind:checked={settings.assignSpeakerFromFilename}
    class="accent-cortex-500"
  />
  <span class="text-sm text-muted">{$t('assignSpeakerFromFilename')}</span>
</label>
<div class="pt-3 border-t border-cortex-800/50 space-y-3">
  <p class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">{$t('qualityGates')}</p>
  <label class="flex items-center gap-3 cursor-pointer">
    <input type="checkbox" bind:checked={settings.enforceQualityGates} class="accent-cortex-500" />
    <span class="text-sm text-muted">{$t('enforceQualityGates')}</span>
  </label>
  <label class="flex items-center gap-3">
    <span class="text-sm text-muted w-32">{$t('maxWer')}</span>
    <input
      type="range"
      min="0.05"
      max="0.80"
      step="0.05"
      bind:value={settings.maxWerThreshold}
      class="flex-1 accent-cortex-500"
    />
    <span class="text-xs font-mono text-cortex-300 w-10 text-end">
      {Math.round(settings.maxWerThreshold * 100)}%
    </span>
  </label>
  <label class="flex items-center gap-3">
    <span class="text-sm text-muted w-32">{$t('maxCer')}</span>
    <input
      type="range"
      min="0.05"
      max="0.80"
      step="0.05"
      bind:value={settings.maxCerThreshold}
      class="flex-1 accent-cortex-500"
    />
    <span class="text-xs font-mono text-cortex-300 w-10 text-end">
      {Math.round(settings.maxCerThreshold * 100)}%
    </span>
  </label>
</div>
