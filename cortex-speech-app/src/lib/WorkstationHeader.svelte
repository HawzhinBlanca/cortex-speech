<script lang="ts">
  import LoaderCircle from '@lucide/svelte/icons/loader-circle';
  import Search from '@lucide/svelte/icons/search';
  import EngineStatusPill from './EngineStatusPill.svelte';
  import JobsActivityPill from './JobsActivityPill.svelte';
  import WorkstationOverflowMenu from './WorkstationOverflowMenu.svelte';
  import { t } from './i18n';
  import { segmentStats } from './stores/segmentStore';
  import { isProcessing } from './stores/uiStore';

  interface Props {
    tauriAvailable: boolean;
    sidebarOpen?: boolean;
    statsOpen?: boolean;
    showHotkeyOverlay: boolean;
    trainingExportBlocked: boolean;
    trainingExportTitle: string;
    modKey: string;
    onSelectWorkspace: (workspace: 'curate' | 'insights') => void;
    onOpenCommandPalette: () => void;
    onOpenFile: () => void;
    onImport: () => void;
    onExport: () => void;
    onExportTranscript: () => void;
    onExportHuggingface: () => void;
    onExportAudio: () => void;
    onOpenWsl: () => void;
    onEnterReview: () => void;
    onValidate: () => void;
    onOpenInbox: () => void;
    onOpenSettings: () => void;
  }

  let { sidebarOpen = $bindable(true), statsOpen = $bindable(true), ...rest }: Props = $props();
</script>

<header
  data-testid="top-bar"
  class="flex min-h-12 flex-nowrap items-center justify-between gap-3 px-3 py-2 glass border-b border-line shrink-0 z-30"
>
  <div class="flex min-w-0 items-center gap-2 sm:gap-3">
    <h1 class="min-w-0 shrink text-sm font-bold tracking-tight whitespace-nowrap">
      <span class="text-cortex-400">CORTEX</span>
      <span class="hidden text-cortex-200 ms-1 sm:inline">{$t('app.subtitle')}</span>
    </h1>
    <span
      class="hidden text-[10px] text-cortex-500 bg-cortex-900 px-2 py-0.5 rounded-full border border-cortex-800/50 lg:inline-flex"
      >v2.1.0</span
    >
    {#if rest.tauriAvailable}<EngineStatusPill /><JobsActivityPill />{/if}
    {#if $isProcessing}<span class="flex items-center gap-1 text-xs text-cortex-400"
        ><LoaderCircle class="h-3 w-3 animate-spin" aria-hidden="true" />{$t('processing')}</span
      >{/if}
  </div>
  <div class="flex min-w-0 shrink-0 items-center justify-end gap-2">
    <span class="hidden text-xs text-cortex-500 xl:inline"
      >{$segmentStats.total} {$t('segments')} · {$segmentStats.verified} {$t('verifiedCount')}</span
    >
    <button
      type="button"
      data-testid="command-palette-btn"
      class="btn btn-secondary shrink-0 !p-2"
      onclick={rest.onOpenCommandPalette}
      title={$t('cmdk.title')}
      aria-label={$t('cmdk.title')}><Search class="h-4 w-4" aria-hidden="true" /></button
    >
    <WorkstationOverflowMenu {...rest} bind:sidebarOpen bind:statsOpen />
  </div>
</header>
