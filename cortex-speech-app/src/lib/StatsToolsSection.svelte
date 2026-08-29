<script lang="ts">
  import type { SnapshotInfo } from './commands';
  import { t } from './i18n';

  let {
    toolBusy,
    snapshots,
    buildSha,
    onImportGold,
    onExportGold,
    onExportFinetune,
    onBackup,
    onRestoreFile,
    onToggleSnapshots,
    onRestoreSnapshot,
    onCompact,
  }: {
    toolBusy: string | null;
    snapshots: SnapshotInfo[] | null;
    buildSha: string | null;
    onImportGold: () => void;
    onExportGold: () => void;
    onExportFinetune: () => void;
    onBackup: () => void;
    onRestoreFile: () => void;
    onToggleSnapshots: () => void;
    onRestoreSnapshot: (name: string, segmentCount: number | null) => void;
    onCompact: () => void;
  } = $props();

  const actions = $derived([
    {
      id: 'importGold',
      testId: 'import-verified-gold-btn',
      label: $t('stats.importVerifiedGold'),
      run: onImportGold,
    },
    {
      id: 'goldEval',
      testId: 'export-gold-eval-btn',
      label: $t('stats.exportGoldEvalSet'),
      run: onExportGold,
    },
    {
      id: 'finetunePack',
      testId: 'export-finetune-pack-btn',
      label: $t('stats.exportFinetunePack'),
      run: onExportFinetune,
    },
    { id: 'backup', testId: 'backup-db-btn', label: $t('stats.backupDb'), run: onBackup },
    {
      id: 'restoreFile',
      testId: 'restore-file-btn',
      label: $t('stats.restoreFile'),
      run: onRestoreFile,
    },
    {
      id: 'listSnapshots',
      testId: 'restore-snapshot-btn',
      label: $t('stats.restoreSnapshot'),
      run: onToggleSnapshots,
    },
    { id: 'compact', testId: 'compact-db-btn', label: $t('stats.compactDb'), run: onCompact },
  ]);
</script>

<div class="space-y-2 pt-2 border-t border-cortex-800/50" data-testid="dataset-tools">
  <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
    {$t('stats.tools')}
  </h3>
  <div class="flex flex-col gap-2">
    {#each actions as action (action.id)}
      <button
        type="button"
        class="btn btn-secondary !text-xs !justify-start"
        data-testid={action.testId}
        disabled={toolBusy !== null}
        onclick={action.run}
      >
        {toolBusy === action.id ? $t('stats.toolWorking') : action.label}
      </button>
    {/each}
  </div>
  {#if snapshots}
    <div class="space-y-1 max-h-40 overflow-y-auto" data-testid="snapshot-list">
      {#if snapshots.length === 0}
        <p class="text-[10px] text-cortex-500">{$t('stats.noSnapshots')}</p>
      {:else}
        {#each snapshots as snapshot}
          <div class="flex items-center gap-2 text-xs bg-cortex-800/30 rounded-lg px-2 py-1">
            <span class="text-cortex-300 font-mono flex-1 min-w-0">
              <span class="block">{new Date(snapshot.timestamp * 1000).toLocaleString()}</span>
              <span class="block text-[9px] text-cortex-500 truncate" title={snapshot.name}>
                {snapshot.name}
              </span>
            </span>
            <span class="text-cortex-400">
              {snapshot.segmentCount === null ? '?' : snapshot.segmentCount}
              {$t('stats.segShort')} · {(snapshot.dbSizeBytes / 1048576).toFixed(1)} MB
            </span>
            <button
              type="button"
              class="btn btn-primary !text-[10px] !px-2 !py-0.5"
              disabled={toolBusy !== null}
              onclick={() => onRestoreSnapshot(snapshot.name, snapshot.segmentCount)}
            >
              {toolBusy === 'restore' ? $t('stats.toolWorking') : $t('stats.restore')}
            </button>
          </div>
        {/each}
      {/if}
    </div>
  {/if}
  {#if buildSha}
    <div class="text-[10px] text-cortex-600 font-mono" data-testid="build-sha">
      {$t('stats.buildSha')}: {buildSha.slice(0, 12)}
    </div>
  {/if}
</div>
