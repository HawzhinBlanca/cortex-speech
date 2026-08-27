<script lang="ts">
  import { focusTrap } from './actions/focusTrap';
  import ErrorBoundary from './ErrorBoundary.svelte';
  import LazyComponent from './LazyComponent.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import Toast from './Toast.svelte';
  import { showSettings } from './stores/settingsStore';
  import {
    showDatasetMerge,
    showKeyboardHelp,
    showReviewInbox,
    showSpeakerPanel,
    showValidationPanel,
    showWslConsole,
  } from './stores/uiStore';

  interface Props {
    showCommandPalette?: boolean;
    reviewActive: boolean;
    loadingLabel: string;
    failedLabel: string;
    retryLabel: string;
    closeLabel: string;
    loadSegments: () => Promise<void>;
    loadSettingsPanel: () => Promise<typeof import('./SettingsPanel.svelte')>;
    loadKeyboardShortcuts: () => Promise<typeof import('./KeyboardShortcuts.svelte')>;
    loadCommandPalette: () => Promise<typeof import('./CommandPalette.svelte')>;
    loadValidationPanel: () => Promise<typeof import('./ValidationPanel.svelte')>;
    loadReviewInbox: () => Promise<typeof import('./ReviewInbox.svelte')>;
    loadSpeakerPanel: () => Promise<typeof import('./SpeakerPanel.svelte')>;
    loadDatasetMerge: () => Promise<typeof import('./DatasetMerge.svelte')>;
    loadWslConsolePanel: () => Promise<typeof import('./WslConsolePanel.svelte')>;
  }

  let {
    showCommandPalette = $bindable(false),
    reviewActive,
    loadingLabel,
    failedLabel,
    retryLabel,
    closeLabel,
    loadSegments,
    loadSettingsPanel,
    loadKeyboardShortcuts,
    loadCommandPalette,
    loadValidationPanel,
    loadReviewInbox,
    loadSpeakerPanel,
    loadDatasetMerge,
    loadWslConsolePanel,
  }: Props = $props();

  const lazyLabels = $derived({ loadingLabel, failedLabel, retryLabel, closeLabel });
</script>

{#if $showSettings}
  <ErrorBoundary>
    <LazyComponent
      load={loadSettingsPanel}
      {...lazyLabels}
      onClose={() => showSettings.set(false)}
      overlay
    />
  </ErrorBoundary>
{/if}

{#if $showKeyboardHelp}
  <LazyComponent
    load={loadKeyboardShortcuts}
    {...lazyLabels}
    onClose={() => showKeyboardHelp.set(false)}
    overlay
  />
{/if}

{#if showCommandPalette}
  <LazyComponent
    load={loadCommandPalette}
    componentProps={{ open: true, reviewActive, onClose: () => (showCommandPalette = false) }}
    {...lazyLabels}
    onClose={() => (showCommandPalette = false)}
    overlay
  />
{/if}

<ConfirmDialog />

{#if $showValidationPanel}
  <ErrorBoundary>
    <LazyComponent
      load={loadValidationPanel}
      {...lazyLabels}
      onClose={() => showValidationPanel.set(false)}
      overlay
    />
  </ErrorBoundary>
{/if}

{#if $showReviewInbox}
  <div class="fixed inset-0 z-[100] flex items-stretch justify-center p-6 glass" use:focusTrap>
    <ErrorBoundary>
      <LazyComponent
        load={loadReviewInbox}
        componentProps={{
          onClose: () => {
            showReviewInbox.set(false);
            void loadSegments();
          },
        }}
        {...lazyLabels}
        onClose={() => showReviewInbox.set(false)}
      />
    </ErrorBoundary>
  </div>
{/if}

{#if $showSpeakerPanel}
  <ErrorBoundary>
    <LazyComponent
      load={loadSpeakerPanel}
      {...lazyLabels}
      onClose={() => showSpeakerPanel.set(false)}
      overlay
    />
  </ErrorBoundary>
{/if}

{#if $showDatasetMerge}
  <ErrorBoundary>
    <LazyComponent
      load={loadDatasetMerge}
      {...lazyLabels}
      onClose={() => showDatasetMerge.set(false)}
      overlay
    />
  </ErrorBoundary>
{/if}

{#if $showWslConsole}
  <ErrorBoundary>
    <LazyComponent
      load={loadWslConsolePanel}
      {...lazyLabels}
      onClose={() => showWslConsole.set(false)}
      overlay
    />
  </ErrorBoundary>
{/if}

<Toast />
