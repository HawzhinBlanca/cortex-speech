<script lang="ts">
  import { t } from './i18n';
  import ModelDownload from './ModelDownload.svelte';
  import ModelRegistry from './ModelRegistry.svelte';

  let { tauriAvailable }: { tauriAvailable: boolean } = $props();
</script>

{#if tauriAvailable}
  <ModelDownload />
  <ModelRegistry />
{:else}
  <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 text-xs text-muted">
    {$t('desktopRuntimeRequired')}
  </div>
{/if}
