<script lang="ts">
  import { t } from './i18n';
  import type { AppSettings } from './stores/settingsStore';

  let { settings = $bindable() }: { settings: AppSettings } = $props();
</script>

<div class="flex items-center gap-3" data-testid="production-asr-model">
  <span class="text-sm text-muted w-32">{$t('asrModel')}</span>
  <bdi class="input flex-1" dir="ltr">Meta OmniASR 7B Champion + LoRA</bdi>
</div>
<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('threads')}</span>
  <input
    type="range"
    min="1"
    max="16"
    bind:value={settings.numThreads}
    class="flex-1 accent-cortex-500"
  />
  <span class="text-xs font-mono text-cortex-300 w-6 text-end">{settings.numThreads}</span>
</label>
<label class="flex items-center gap-3 cursor-pointer">
  <input type="checkbox" bind:checked={settings.enableGpu} class="accent-cortex-500" />
  <span class="text-sm text-muted">{$t('gpuAcceleration')}</span>
</label>
<p class="text-xs text-subtle ms-7 -mt-1">{$t('gpuAccelerationNote')}</p>
