<script lang="ts">
  import { t } from './i18n';
  import type { ImportJob, QuarantineNotice } from './commands';
  import type { ImportRecoveryAuthorityState } from './importRecoveryController';
  import { showConfirmDialog } from './stores/uiStore';

  interface Props {
    quarantineNotice: QuarantineNotice | null;
    interruptedImport: ImportJob | null;
    importRecoveryBusy: boolean;
    importRecoveryAuthority: ImportRecoveryAuthorityState;
    workspaceOperationBusy: boolean;
    onAcknowledgeQuarantine: () => void;
    onDismissQuarantine: () => void;
    onResumeImport: () => void;
    onDismissImport: () => void;
    onRetryRecoveryCheck: () => void;
  }

  let {
    quarantineNotice,
    interruptedImport,
    importRecoveryBusy,
    importRecoveryAuthority,
    workspaceOperationBusy,
    onAcknowledgeQuarantine,
    onDismissQuarantine,
    onResumeImport,
    onDismissImport,
    onRetryRecoveryCheck,
  }: Props = $props();

  function confirmDiscardImport() {
    const expectedJobId = interruptedImport?.id;
    if (!expectedJobId || importRecoveryBusy || importRecoveryAuthority !== 'known') return;
    showConfirmDialog.set({
      title: $t('import.discardConfirmTitle'),
      message: $t('import.discardConfirmMessage'),
      confirmLabel: $t('import.discardConfirmAction'),
      danger: true,
      onConfirm: () => {
        // A reconciliation can replace this banner while the modal is open. An old confirmation
        // must never authorize deleting a newer recovery journal; the backend enforces the same
        // exact-ID comparison independently.
        if (importRecoveryAuthority === 'known' && interruptedImport?.id === expectedJobId)
          onDismissImport();
      },
    });
  }

  const recoveryActionsDisabled = $derived(
    importRecoveryBusy || workspaceOperationBusy || importRecoveryAuthority !== 'known',
  );
</script>

{#if quarantineNotice}
  <div
    class="flex min-w-0 flex-col items-stretch gap-3 border-b border-red-600/50 bg-red-950/50 px-4 py-2 sm:flex-row sm:items-center sm:justify-between"
    data-testid="quarantine-banner"
  >
    <span class="text-sm text-red-200">
      {$t('db.quarantined')
        .replace('{files}', String(quarantineNotice.quarantinedFileCount))
        .replace('{snapshots}', String(quarantineNotice.snapshotCount))}
    </span>
    <div class="flex min-w-0 flex-wrap items-center gap-2">
      <button
        type="button"
        class="btn btn-secondary !text-xs"
        data-testid="acknowledge-quarantine-btn"
        onclick={onAcknowledgeQuarantine}
      >
        {$t('db.quarantineAcknowledge')}
      </button>
      <button
        type="button"
        class="btn btn-ghost !text-xs"
        data-testid="dismiss-quarantine-btn"
        onclick={onDismissQuarantine}
      >
        {$t('db.quarantineDismiss')}
      </button>
    </div>
  </div>
{/if}

{#if interruptedImport || importRecoveryAuthority !== 'known'}
  <div
    class="flex min-w-0 flex-col items-stretch gap-3 border-b border-amber-600/40 bg-amber-950/40 px-4 py-2 sm:flex-row sm:items-center sm:justify-between"
    data-testid="resume-import-banner"
    aria-busy={importRecoveryBusy || importRecoveryAuthority === 'checking'}
  >
    <span
      class="text-sm text-amber-200"
      data-testid="resume-import-status"
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      {#if importRecoveryAuthority === 'checking'}
        {$t('import.recoveryChecking')}
      {:else if importRecoveryAuthority === 'unknown'}
        {$t('import.recoveryAuthorityUnknown')}
      {:else if interruptedImport}
        {$t('import.interrupted')
          .replace('{done}', String(interruptedImport.completedCount))
          .replace('{total}', String(interruptedImport.totalFiles))}
      {/if}
    </span>
    {#if recoveryActionsDisabled}
      <span id="import-recovery-busy-reason" class="sr-only">
        {workspaceOperationBusy
          ? $t('import.workspaceBusy')
          : importRecoveryAuthority !== 'known'
            ? $t('import.recoveryChecking')
            : $t('import.recoveryBusy')}
      </span>
    {/if}
    <div class="flex min-w-0 flex-wrap items-center gap-2">
      {#if importRecoveryAuthority === 'unknown'}
        <button
          type="button"
          class="btn btn-primary !text-xs"
          data-testid="retry-recovery-check-btn"
          disabled={importRecoveryBusy || workspaceOperationBusy}
          aria-describedby={importRecoveryBusy || workspaceOperationBusy
            ? 'import-recovery-busy-reason'
            : undefined}
          onclick={() => {
            if (!importRecoveryBusy && !workspaceOperationBusy) onRetryRecoveryCheck();
          }}
        >
          {$t('import.retryRecoveryCheck')}
        </button>
      {:else if interruptedImport}
        <button
          type="button"
          class="btn btn-primary !text-xs"
          data-testid="resume-import-btn"
          disabled={recoveryActionsDisabled}
          aria-describedby={recoveryActionsDisabled ? 'import-recovery-busy-reason' : undefined}
          onclick={() => {
            if (!recoveryActionsDisabled) onResumeImport();
          }}
        >
          {$t('import.resume')}
        </button>
        <button
          type="button"
          class="btn btn-ghost !text-xs"
          data-testid="dismiss-import-btn"
          disabled={recoveryActionsDisabled}
          aria-describedby={recoveryActionsDisabled ? 'import-recovery-busy-reason' : undefined}
          onclick={confirmDiscardImport}
        >
          {$t('import.discard')}
        </button>
      {/if}
    </div>
  </div>
{/if}
