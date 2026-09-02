<script lang="ts">
  import type { ConformalCertificate, IntelligenceReport, SnapshotInfo } from './commands';
  import { t } from './i18n';
  import StatsDatasetEvidence from './StatsDatasetEvidence.svelte';
  import StatsRuntimeEvidence from './StatsRuntimeEvidence.svelte';
  import StatsToolsSection from './StatsToolsSection.svelte';
  import type { InferenceStats } from './statsDashboardModel';
  import type { DatasetStats } from './types';

  let {
    stats,
    durationBuckets,
    maxBucket,
    cert,
    inferenceStats,
    intel,
    tauriAvailable,
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
    stats: DatasetStats;
    durationBuckets: { label: string; value: number }[];
    maxBucket: number;
    cert: ConformalCertificate | null;
    inferenceStats: InferenceStats | null;
    intel: IntelligenceReport | null;
    tauriAvailable: boolean;
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
</script>

<details class="group border-t border-cortex-800/50 pt-2" data-testid="stats-advanced">
  <summary
    class="cursor-pointer list-none text-xs font-semibold uppercase tracking-wider text-cortex-400 hover:text-cortex-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
  >
    <span class="inline-block transition-transform group-open:rotate-90">▸</span>
    {$t('stats.advanced')}
  </summary>
  <div class="space-y-4 pt-3">
    <StatsDatasetEvidence {stats} {durationBuckets} {maxBucket} {cert} />
    <StatsRuntimeEvidence {inferenceStats} {intel} />
    {#if tauriAvailable}
      <StatsToolsSection
        {toolBusy}
        {snapshots}
        {buildSha}
        {onImportGold}
        {onExportGold}
        {onExportFinetune}
        {onBackup}
        {onRestoreFile}
        {onToggleSnapshots}
        {onRestoreSnapshot}
        {onCompact}
      />
    {/if}
  </div>
</details>
