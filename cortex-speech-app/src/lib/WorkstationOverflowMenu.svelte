<script lang="ts">
  import Ellipsis from '@lucide/svelte/icons/ellipsis';
  import LazyComponent from './LazyComponent.svelte';
  import { t } from './i18n';

  interface Props {
    tauriAvailable: boolean;
    sidebarOpen?: boolean;
    statsOpen?: boolean;
    showHotkeyOverlay: boolean;
    trainingExportBlocked: boolean;
    trainingExportTitle: string;
    modKey: string;
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

  let { sidebarOpen = $bindable(), statsOpen = $bindable(), ...rest }: Props = $props();

  let detailsElement: HTMLDetailsElement | undefined;
  let menuOpen = $state(false);
  const loadOverflowContent = () => import('./WorkstationOverflowMenuContent.svelte');
  const contentProps = $derived({
    ...rest,
    sidebarOpen,
    statsOpen,
    onOpenSidebar: () => (sidebarOpen = true),
    onOpenStats: () => (statsOpen = true),
  });
  const lazyLabels = $derived({
    loadingLabel: $t('loading'),
    failedLabel: $t('workspace.loadFailed'),
    retryLabel: $t('retry'),
    closeLabel: $t('close'),
  });

  function handleToggle() {
    menuOpen = detailsElement?.open ?? false;
  }

  function closeOnAction(node: HTMLElement) {
    const handleClick = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof HTMLElement) || !target.closest('button')) return;
      queueMicrotask(() => {
        detailsElement?.removeAttribute('open');
        menuOpen = false;
      });
    };
    node.addEventListener('click', handleClick);
    return { destroy: () => node.removeEventListener('click', handleClick) };
  }
</script>

<details
  bind:this={detailsElement}
  data-testid="header-overflow"
  class="relative"
  ontoggle={handleToggle}
>
  <summary
    data-testid="header-overflow-btn"
    class="btn btn-secondary shrink-0 list-none !p-2 cursor-pointer"
    title={$t('header.moreActions')}
    aria-label={$t('header.moreActions')}
  >
    <Ellipsis class="h-4 w-4" aria-hidden="true" />
  </summary>
  {#if menuOpen}
    <div
      data-testid="header-overflow-menu"
      class="absolute end-0 top-[calc(100%+0.5rem)] z-[80] flex max-h-[min(70vh,36rem)] w-[min(34rem,calc(100vw-1rem))] flex-wrap items-center justify-end gap-2 overflow-y-auto rounded-xl border border-line bg-surface-1 p-3 shadow-2xl"
      use:closeOnAction
    >
      <LazyComponent load={loadOverflowContent} componentProps={contentProps} {...lazyLabels} />
    </div>
  {/if}
</details>
