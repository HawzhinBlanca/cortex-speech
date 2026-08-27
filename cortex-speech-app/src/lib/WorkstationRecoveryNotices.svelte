<script lang="ts">
  import { t } from './i18n';
  import type { ImportJob, QuarantineNotice } from './commands';

  interface Props {
    quarantineNotice: QuarantineNotice | null;
    interruptedImport: ImportJob | null;
    onAcknowledgeQuarantine: () => void;
    onDismissQuarantine: () => void;
    onResumeImport: () => void;
    onDismissImport: () => void;
  }

  let {
    quarantineNotice,
    interruptedImport,
    onAcknowledgeQuarantine,
    onDismissQuarantine,
    onResumeImport,
    onDismissImport,
  }: Props = $props();
</script>

{#if quarantineNotice}
  <div
    class="flex items-center justify-between gap-3 border-b border-red-600/50 bg-red-950/50 px-4 py-2"
    data-testid="quarantine-banner"
  >
    <span class="text-sm text-red-200">
      {$t('db.quarantined')
        .replace('{files}', String(quarantineNotice.quarantinedFileCount))
        .replace('{snapshots}', String(quarantineNotice.snapshotCount))}
    </span>
    <div class="flex items-center gap-2">
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

{#if interruptedImport}
  <div
    class="flex items-center justify-between gap-3 border-b border-amber-600/40 bg-amber-950/40 px-4 py-2"
    data-testid="resume-import-banner"
  >
    <span class="text-sm text-amber-200">
      {$t('import.interrupted')
        .replace('{done}', String(interruptedImport.completedPaths.length))
        .replace('{total}', String(interruptedImport.totalFiles))
        .replace('{dir}', interruptedImport.dir)}
    </span>
    <div class="flex items-center gap-2">
      <button
        type="button"
        class="btn btn-primary !text-xs"
        data-testid="resume-import-btn"
        onclick={onResumeImport}
      >
        {$t('import.resume')}
      </button>
      <button
        type="button"
        class="btn btn-ghost !text-xs"
        data-testid="dismiss-import-btn"
        onclick={onDismissImport}
      >
        {$t('import.discard')}
      </button>
    </div>
  </div>
{/if}
