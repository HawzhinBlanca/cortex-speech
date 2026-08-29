<script lang="ts">
  import { t } from './i18n';
  import type { AppSettings } from './stores/settingsStore';

  let { settings = $bindable() }: { settings: AppSettings } = $props();
</script>

<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('theme')}</span>
  <select class="input flex-1" bind:value={settings.theme}>
    <option value="dark">{$t('dark')}</option>
    <option value="light">{$t('light')}</option>
    <option value="system">{$t('system')}</option>
  </select>
</label>
<label class="flex items-center gap-3">
  <span class="text-sm text-muted w-32">{$t('language')}</span>
  <select class="input flex-1" bind:value={settings.language}>
    <option value="ckb">{$t('kurdish')}</option>
  </select>
</label>
<label class="flex items-center gap-3 cursor-pointer">
  <input type="checkbox" bind:checked={settings.autoNormalize} class="accent-cortex-500" />
  <span class="text-sm text-muted">{$t('autoNormalize')}</span>
</label>
{#if settings.autoNormalize}
  <label class="flex items-center gap-3 cursor-pointer ps-6">
    <input type="checkbox" bind:checked={settings.verbalizeNumbers} class="accent-cortex-500" />
    <span class="text-sm text-muted">{$t('verbalizeNumbers')}</span>
  </label>
{/if}
<label class="flex items-center gap-3 cursor-pointer">
  <input type="checkbox" bind:checked={settings.autoAlign} class="accent-cortex-500" />
  <span class="text-sm text-muted">{$t('autoAlign')}</span>
</label>
<label class="flex items-center gap-3 cursor-pointer">
  <input type="checkbox" bind:checked={settings.autoplaySegments} class="accent-cortex-500" />
  <span class="text-sm text-muted">{$t('settings.autoplaySegments')}</span>
</label>
