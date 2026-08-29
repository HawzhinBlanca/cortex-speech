<script lang="ts">
  import DiagnosticsPanel from './DiagnosticsPanel.svelte';
  import { t } from './i18n';

  let { tauriAvailable }: { tauriAvailable: boolean } = $props();
</script>

{#if tauriAvailable}
  <DiagnosticsPanel />
{:else}
  <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 text-xs text-muted">
    {$t('desktopRuntimeRequired')}
  </div>
{/if}
