<script lang="ts">
  import ChartNoAxesColumnIncreasing from '@lucide/svelte/icons/chart-no-axes-column-increasing';
  import CircleCheckBig from '@lucide/svelte/icons/circle-check-big';
  import Database from '@lucide/svelte/icons/database';
  import Download from '@lucide/svelte/icons/download';
  import FileAudio from '@lucide/svelte/icons/file-audio';
  import FileText from '@lucide/svelte/icons/file-text';
  import FolderInput from '@lucide/svelte/icons/folder-input';
  import List from '@lucide/svelte/icons/list';
  import Music2 from '@lucide/svelte/icons/music-2';
  import SettingsIcon from '@lucide/svelte/icons/settings';
  import SquarePen from '@lucide/svelte/icons/square-pen';
  import SquareTerminal from '@lucide/svelte/icons/square-terminal';
  import { locale, setLocale, t } from './i18n';
  import { notifications } from './stores/notificationStore';
  import { segmentStats } from './stores/segmentStore';
  import { isProcessing } from './stores/uiStore';

  interface Props {
    tauriAvailable: boolean;
    sidebarOpen: boolean;
    statsOpen: boolean;
    showHotkeyOverlay: boolean;
    trainingExportBlocked: boolean;
    trainingExportTitle: string;
    modKey: string;
    onOpenSidebar: () => void;
    onOpenStats: () => void;
    onSelectWorkspace: (workspace: 'curate' | 'insights') => void;
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

  let {
    tauriAvailable,
    sidebarOpen,
    statsOpen,
    showHotkeyOverlay,
    trainingExportBlocked,
    trainingExportTitle,
    modKey,
    onOpenSidebar,
    onOpenStats,
    onSelectWorkspace,
    onOpenFile,
    onImport,
    onExport,
    onExportTranscript,
    onExportHuggingface,
    onExportAudio,
    onOpenWsl,
    onEnterReview,
    onValidate,
    onOpenInbox,
    onOpenSettings,
  }: Props = $props();

  let localeChanging = $state(false);

  async function toggleLocale() {
    if (localeChanging) return;
    localeChanging = true;
    const changed = await setLocale($locale === 'en' ? 'ckb' : 'en');
    if (!changed) notifications.error($t('localeLoadFailed'));
    localeChanging = false;
  }
</script>

<button
  type="button"
  data-testid="overflow-curate-btn"
  class="btn btn-secondary !text-xs"
  onclick={() => onSelectWorkspace('curate')}>{$t('nav.curate')}</button
>
<button
  type="button"
  data-testid="overflow-insights-btn"
  class="btn btn-secondary !text-xs"
  onclick={() => onSelectWorkspace('insights')}>{$t('nav.insights')}</button
>
{#if !sidebarOpen}
  <button
    class="btn btn-secondary !text-xs relative"
    onclick={onOpenSidebar}
    title={$t('showSegments')}
    aria-label={$t('showSegments')}
  >
    <List class="inline h-3.5 w-3.5" aria-hidden="true" />
    {#if showHotkeyOverlay}<span
        class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
        >⇧S</span
      >{/if}
  </button>
{/if}
{#if !statsOpen}
  <button
    class="btn btn-secondary !text-xs relative"
    onclick={onOpenStats}
    title={$t('showStats')}
    aria-label={$t('showStats')}
  >
    <ChartNoAxesColumnIncreasing class="inline h-3.5 w-3.5" aria-hidden="true" />
    {#if showHotkeyOverlay}<span
        class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
        >⇧D</span
      >{/if}
  </button>
{/if}
<button
  class="btn btn-secondary !text-xs relative"
  onclick={onOpenFile}
  disabled={!tauriAvailable || $isProcessing}
  title={tauriAvailable ? 'Ctrl+O' : $t('desktopRuntimeRequired')}
  aria-label={$t('openAudioFile')}
>
  <FileAudio class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('open')}
  {#if showHotkeyOverlay}<span
      class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
      >^O</span
    >{/if}
</button>
<button
  class="btn btn-secondary !text-xs relative"
  onclick={onImport}
  disabled={!tauriAvailable || $isProcessing}
  title={tauriAvailable ? 'Ctrl+I' : $t('desktopRuntimeRequired')}
  aria-label={$t('importDirectory')}
>
  <FolderInput class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('import')}
  {#if showHotkeyOverlay}<span
      class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
      >^I</span
    >{/if}
</button>
<button
  class="btn btn-secondary !text-xs"
  onclick={onExport}
  disabled={!tauriAvailable || $isProcessing || $segmentStats.total === 0}
  title={!tauriAvailable ? $t('desktopRuntimeRequired') : $t('export')}
  aria-label={$t('export')}
>
  <Download class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('export')}
</button>
<button
  data-testid="export-transcript-btn"
  class="btn btn-secondary !text-xs"
  onclick={onExportTranscript}
  disabled={!tauriAvailable || $isProcessing || $segmentStats.total === 0}
  title={!tauriAvailable ? $t('desktopRuntimeRequired') : $t('exportTranscript.title')}
  aria-label={$t('exportTranscript')}
>
  <FileText class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('exportTranscript')}
</button>
<button
  data-testid="hf-export-btn"
  class="btn btn-secondary !text-xs"
  onclick={onExportHuggingface}
  disabled={!tauriAvailable || $isProcessing || $segmentStats.total === 0 || trainingExportBlocked}
  aria-label={$t('exportHuggingface.label')}
  title={trainingExportTitle}
>
  <Database class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('exportHuggingface.label')}
</button>
<button
  class="btn btn-secondary !text-xs"
  onclick={onExportAudio}
  disabled={!tauriAvailable || $isProcessing || $segmentStats.verified === 0}
  title={!tauriAvailable ? $t('desktopRuntimeRequired') : $t('exportAudio.label')}
  aria-label={$t('exportAudio.label')}
>
  <Music2 class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('exportAudio.label')}
</button>
<button
  data-testid="wsl-btn"
  class="btn btn-secondary !text-xs relative"
  onclick={onOpenWsl}
  disabled={!tauriAvailable || $isProcessing}
  title={tauriAvailable
    ? $t('wsl.title', { model: 'Meta OmniASR 7B v2' })
    : $t('desktopRuntimeRequired')}
>
  <SquareTerminal class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('wsl.title', { model: 'Meta OmniASR 7B v2' })}
</button>
<button
  data-testid="review-correct-btn"
  class="btn !text-xs relative {$segmentStats.pending > 0 ? 'btn-primary' : 'btn-secondary'}"
  onclick={onEnterReview}
  disabled={!tauriAvailable || $segmentStats.total === 0}
  title={tauriAvailable ? $t('reviewCorrect.tooltip') : $t('desktopRuntimeRequired')}
  aria-label={$t('reviewCorrect.label')}
>
  <SquarePen class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('reviewCorrect.label')}
  {#if $segmentStats.pending > 0}<span
      data-testid="review-pending-badge"
      class="absolute -top-2 -right-2 min-w-[18px] h-[18px] flex items-center justify-center bg-amber-400 text-black text-[10px] font-bold px-1 rounded-full shadow border border-amber-500 select-none z-50 pointer-events-none"
      >{$segmentStats.pending}</span
    >{/if}
</button>
<button
  data-testid="validate-btn"
  class="btn btn-secondary !text-xs relative"
  onclick={onValidate}
  disabled={!tauriAvailable || $isProcessing || $segmentStats.total === 0}
  title={tauriAvailable ? 'Ctrl+Shift+V' : $t('desktopRuntimeRequired')}
  aria-label={$t('validate')}
>
  <CircleCheckBig class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('validate')}
  {#if showHotkeyOverlay}<span
      class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
      >^+V</span
    >{/if}
</button>
<button
  data-testid="review-inbox-btn"
  class="btn btn-secondary !text-xs relative"
  onclick={onOpenInbox}
  disabled={!tauriAvailable || $isProcessing}
  title={tauriAvailable ? $t('reviewInbox') : $t('desktopRuntimeRequired')}
  aria-label={$t('reviewInbox')}
>
  {$t('reviewInbox')}
  {#if showHotkeyOverlay}<span
      class="absolute -top-1.5 -right-1.5 bg-purple-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-purple-500 select-none z-50 pointer-events-none"
      >^+R</span
    >{/if}
</button>
<button
  data-testid="settings-btn"
  class="btn btn-primary !text-xs relative"
  onclick={onOpenSettings}
  title="{modKey}+,"
  aria-label={$t('openSettings')}
>
  <SettingsIcon class="me-1 inline h-3.5 w-3.5" aria-hidden="true" />
  {$t('settings')}
  {#if showHotkeyOverlay}<span
      class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
      >^,</span
    >{/if}
</button>
<button
  data-testid="locale-toggle"
  class="btn btn-secondary !text-xs"
  onclick={toggleLocale}
  disabled={localeChanging}
  aria-busy={localeChanging}
  title={$t('localeToggle')}
  aria-label={$t('localeToggle')}
>
  {localeChanging ? $t('loading') : $locale === 'ckb' ? $t('english') : $t('kurdish')}
</button>
