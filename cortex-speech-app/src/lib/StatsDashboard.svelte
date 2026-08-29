<script lang="ts">
  import ChartNoAxesColumnIncreasing from '@lucide/svelte/icons/chart-no-axes-column-increasing';
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
  import { onMount } from 'svelte';
  import { chooseDirectory, chooseFile } from './fileDialogs';
  import * as api from './commands';
  import type { DatasetStats } from './types';
  import { segments } from './stores/segmentStore';
  import { notifications } from './stores/notificationStore';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';
  import { reloadApp } from './reloadBoundary';
  import { formatPublicErrorReference } from './errorText';
  import StatsAdvancedSection from './StatsAdvancedSection.svelte';
  import StatsQualitySection from './StatsQualitySection.svelte';
  import StatsReadinessSection from './StatsReadinessSection.svelte';
  import {
    buildLocalStats,
    buildStatsBlockers,
    readAccuracyRecord,
    type InferenceStats,
  } from './statsDashboardModel';

  // External review 2026-08-06, P2.3: "every blocker should have a deterministic next action".
  // `pendingReview` has declared `action: 'review'` since it was written, and the template only ever
  // rendered a button for `action: 'relink'` — so the one blocker a reviewer can always act on rendered
  // as a dead sentence, and the readiness card told them what was wrong while offering no way to fix it.
  // The component had no route out (it takes no props), which is why the action was droppable in the
  // first place. This is that route.
  let { onOpenReview }: { onOpenReview?: () => void } = $props();

  let stats = $state<DatasetStats | null>(null);
  let audioHealth = $state<import('./commands').AudioHealth | null>(null);
  let relinking = $state(false);
  let quality = $state<import('./commands').DatasetQuality | null>(null);
  let cert = $state<import('./commands').ConformalCertificate | null>(null);
  let inferenceStats = $state<InferenceStats | null>(null);
  let loading = $state(true);
  let errorMessage = $state<string | null>(null);
  let fingerprintCount = $state<number | null>(null);
  const tauriAvailable = isTauriRuntime();

  // P0 #7: the decision layer. `breakdown` is the ONLY honest source for "ready" — it comes from the
  // same training_grade_for_segment the export gates on, so the verdict can never disagree with what
  // an export would write. `evalRuns` backs the one canonical accuracy card.
  let breakdown = $state<import('./commands').TrainingGradeBreakdown | null>(null);
  let evalRuns = $state<import('./types').EvalRun[] | null>(null);

  // Round-24 #10: histogram bars are normalized to the LARGEST bucket, not the total segment count, so
  // the chart actually uses its vertical range (a typical VAD dataset is mostly short clips, so no
  // single bucket is a large share of the total and the old total-normalized bars looked flat).
  const durationBuckets = $derived(
    stats
      ? [
          { label: '<5s', value: stats.durationHistogram.under5s },
          { label: '<10s', value: stats.durationHistogram.under10s },
          { label: '<15s', value: stats.durationHistogram.under15s },
          { label: '<30s', value: stats.durationHistogram.under30s },
          { label: '30s+', value: stats.durationHistogram.over30s },
        ]
      : [],
  );
  const maxBucket = $derived(Math.max(1, ...durationBuckets.map((b) => b.value)));

  const blockers = $derived(buildStatsBlockers(audioHealth, stats, quality, breakdown, evalRuns));

  // 'unknown' is a real state, not a fallback to green: it means the readiness inputs did not load.
  const verdict = $derived<'ready' | 'notReady' | 'unknown'>(
    breakdown === null
      ? 'unknown'
      : blockers.length === 0 && breakdown.summary.trainingReadySegments > 0
        ? 'ready'
        : 'notReady',
  );

  const accuracy = $derived(readAccuracyRecord(evalRuns));

  async function fetchStats() {
    if (!tauriAvailable) {
      stats = buildLocalStats($segments);
      quality = null;
      cert = null;
      loading = false;
      return;
    }
    loading = true;
    errorMessage = null;
    try {
      [stats, quality] = await Promise.all([api.getDatasetStats(), api.getDatasetQuality()]);
      try {
        fingerprintCount = await api.getFingerprintCount();
      } catch {
        // Non-essential stat — leave it hidden if the backend call fails.
      }
      try {
        cert = await api.getDatasetCertificate(0.05, 0.95);
      } catch (err) {
        console.error('Failed to load conformal certificate', err);
      }
      try {
        audioHealth = await api.getAudioHealth();
      } catch (err) {
        console.error('Failed to load audio health', err);
      }
      // Both stay NULL on failure, and the verdict renders "unknown" rather than "ready". A readiness
      // headline that defaults to green when its own inputs failed to load is worse than no headline.
      try {
        breakdown = await api.getTrainingGradeBreakdown();
      } catch (err) {
        console.error('Failed to load training grade breakdown', err);
      }
      try {
        evalRuns = await api.listEvalRuns();
      } catch (err) {
        console.error('Failed to load eval runs', err);
      }
    } catch (e) {
      errorMessage = formatPublicErrorReference(e) ?? $t('errors.unknown');
      notifications.error($t('stats.failed'), { cause: e });
    } finally {
      loading = false;
    }
  }

  function track(..._args: unknown[]) {}

  // P3.3: relink missing source audio by pointing at the folder the owner moved it to.
  async function relinkMissingAudio() {
    if (!tauriAvailable || relinking) return;
    relinking = true;
    try {
      const dir = await chooseDirectory();
      if (!dir) return;
      const result = await api.relinkAudio(dir);
      notifications.success(
        $t('stats.relinkDone')
          .replace('{n}', String(result.relinked))
          .replace('{m}', String(result.stillMissing)),
      );
      await fetchStats();
    } catch (e) {
      notifications.error($t('stats.relinkFailed'), { cause: e });
    } finally {
      relinking = false;
    }
  }

  // P5.1 / M5: dataset & retrain tools. These surface the previously-unreachable backend export
  // commands so the RETRAIN_RUNBOOK is actually executable from the app (not dev-console only).
  // `toolBusy` holds the id of the running action so exactly one runs at a time and its button shows
  // progress. Each mirrors the proven relinkMissingAudio pattern (dir dialog -> IPC -> toast).
  let toolBusy = $state<string | null>(null);
  let buildSha = $state<string | null>(null);
  // Intelligence read-side (C4/C5 evidence). Non-essential — hidden if the call fails.
  let intel = $state<import('./commands').IntelligenceReport | null>(null);

  async function pickDirAnd<T>(id: string, run: (dir: string) => Promise<T>): Promise<T | null> {
    if (!tauriAvailable || toolBusy) return null;
    toolBusy = id;
    try {
      const dir = await chooseDirectory();
      if (!dir) return null;
      return await run(dir);
    } catch (e) {
      notifications.error($t('stats.toolFailed'), { cause: e });
      return null;
    } finally {
      toolBusy = null;
    }
  }

  async function exportFinetunePack() {
    const r = await pickDirAnd('finetunePack', (dir) => api.exportFinetunePack(dir));
    if (r) {
      notifications.success(
        $t('stats.finetunePackDone')
          .replace('{n}', String(r.emitted))
          .replace('{h}', String(r.excludedUnexportable))
          .replace('{r}', String(r.excludedNotTrainingReady))
          .replace('{s}', String(r.skipped)),
      );
    }
  }

  async function exportGoldEvalSet() {
    const r = await pickDirAnd('goldEval', (dir) => api.exportGoldEvalSet(dir));
    if (r) {
      notifications.success($t('stats.goldEvalDone').replace('{n}', String(r.exported)));
    }
  }

  // 4.5: off-disk backup. The rotating auto-snapshots live in the app data dir alongside the live DB
  // — one disk failure loses both. This copies the whole library to a folder the owner chooses (an
  // external drive, a synced folder), into a timestamped file so successive backups never collide.
  async function backupToFolder() {
    let verifiedCount = 0;
    const r = await pickDirAnd('backup', async (dir) => {
      const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
      const sep = dir.includes('\\') ? '\\' : '/';
      const base = dir.endsWith(sep) ? dir : `${dir}${sep}`;
      const dest = `${base}cortex-speech-backup-${stamp}.db`;
      // The backend verifies the WRITTEN file (integrity + count) — surface that proof in the toast
      // so "backup done" always means "backup verified" (true-10 audit 2026-07-09).
      const verified = await api.dbBackup(dest);
      verifiedCount = verified.segmentCount;
      return dest;
    });
    if (r) {
      notifications.success(
        `${$t('stats.backupDone').replace('{path}', r)} — ${$t('stats.backupVerified', { count: String(verifiedCount) })}`,
      );
    }
  }

  async function importVerifiedAsGold() {
    if (!tauriAvailable || toolBusy) return;
    toolBusy = 'importGold';
    try {
      const created = await api.importVerifiedSegmentsAsGold();
      notifications.success($t('stats.importGoldDone').replace('{n}', String(created)));
    } catch (e) {
      notifications.error($t('stats.toolFailed'), { cause: e });
    } finally {
      toolBusy = null;
    }
  }

  // Reclaim disk from a library bloated by months of deletes / re-transcribes (VACUUM), then rebuild
  // the FTS index the vacuum's rowid-renumbering can desync (handled backend-side in db.vacuum()). The
  // db_vacuum IPC previously had no caller — a long-lived personal DB could only grow.
  async function compactDatabase() {
    if (!tauriAvailable || toolBusy) return;
    toolBusy = 'compact';
    try {
      await api.dbVacuum();
      notifications.success($t('stats.compactDone'));
    } catch (e) {
      notifications.error($t('stats.toolFailed'), { cause: e });
    } finally {
      toolBusy = null;
    }
  }

  // B2: restore-from-snapshot picker. `snapshots` non-null = list expanded. Restoring overwrites the
  // live library, so it demands an explicit confirm; on success the whole app reloads (every store —
  // segments, session cursor, stats — must re-derive from the restored DB).
  let snapshots = $state<import('./commands').SnapshotInfo[] | null>(null);

  async function toggleSnapshotList() {
    if (!tauriAvailable || toolBusy) return;
    if (snapshots) {
      snapshots = null;
      return;
    }
    toolBusy = 'listSnapshots';
    try {
      snapshots = await api.listDbSnapshots();
    } catch (e) {
      notifications.error($t('stats.toolFailed'), { cause: e });
    } finally {
      toolBusy = null;
    }
  }

  async function restoreSnapshot(name: string, segmentCount: number | null) {
    if (!tauriAvailable || toolBusy) return;
    const message = $t('stats.restoreConfirm')
      .replace('{name}', name)
      .replace('{n}', segmentCount === null ? '?' : String(segmentCount));
    if (!window.confirm(message)) return;
    toolBusy = 'restore';
    try {
      await api.restoreDbFromSnapshot(name);
      // Full reload: the restored DB invalidates every in-memory store.
      reloadApp();
    } catch (e) {
      notifications.error($t('stats.restoreFailed'), { cause: e });
      toolBusy = null;
    }
  }

  // 4.5 counterpart to backupToFolder: restore the live library from a backup .db file the owner
  // picks (e.g. the file "Backup to folder…" wrote to an external drive). Destructive — the backend
  // integrity-checks the source first; on success the whole app reloads (every store re-derives).
  async function restoreFromFile() {
    if (!tauriAvailable || toolBusy) return;
    let src: string;
    try {
      const picked = await chooseFile({
        filters: [{ name: 'Cortex backup', extensions: ['db'] }],
      });
      if (!picked) return;
      src = picked;
    } catch (e) {
      notifications.error($t('stats.toolFailed'), { cause: e });
      return;
    }
    if (!window.confirm($t('stats.restoreFileConfirm'))) return;
    toolBusy = 'restoreFile';
    try {
      await api.dbRestore(src);
      reloadApp();
    } catch (e) {
      notifications.error($t('stats.restoreFailed'), { cause: e });
      toolBusy = null;
    }
  }

  async function fetchInferenceStats() {
    if (!tauriAvailable) {
      inferenceStats = null;
      return;
    }
    try {
      inferenceStats = await api.getInferenceStats();
    } catch {
      // Silently ignore inference stats if backend command fails
    }
  }

  onMount(() => {
    fetchStats();
    fetchInferenceStats();
    if (tauriAvailable) {
      // Build-info is a non-essential diagnostic — leave it hidden if the call fails.
      api
        .appGitSha()
        .then((sha) => (buildSha = sha))
        .catch(() => {});
      api
        .getIntelligenceReport()
        .then((r) => (intel = r))
        .catch(() => {});
    }
  });

  let fetchDebounceTimer: ReturnType<typeof setTimeout>;
  $effect(() => {
    track($segments);
    clearTimeout(fetchDebounceTimer);
    fetchDebounceTimer = setTimeout(() => {
      fetchStats();
      fetchInferenceStats();
    }, 500);
    // Cancel the pending timer on teardown so it can't fire backend fetches / write $state after the
    // panel unmounts (navigating away within the 500ms debounce).
    return () => clearTimeout(fetchDebounceTimer);
  });
</script>

<div class="card p-4 space-y-4">
  <h2 class="text-sm font-semibold text-cortex-200 uppercase tracking-wider">
    {$t('stats.title')}
  </h2>

  {#if loading}
    <div class="space-y-3">
      {#each [1, 2, 3, 4] as _}
        <div class="h-10 bg-cortex-800/30 rounded-lg animate-pulse"></div>
      {/each}
    </div>
  {:else if stats}
    <StatsReadinessSection
      {stats}
      {audioHealth}
      {breakdown}
      {blockers}
      {verdict}
      {accuracy}
      evalRunsLoaded={evalRuns !== null}
      {fingerprintCount}
      {relinking}
      onRelink={relinkMissingAudio}
      {onOpenReview}
    />
    <StatsQualitySection {quality} />
    <StatsAdvancedSection
      {stats}
      {durationBuckets}
      {maxBucket}
      {cert}
      {inferenceStats}
      {intel}
      {tauriAvailable}
      {toolBusy}
      {snapshots}
      {buildSha}
      onImportGold={importVerifiedAsGold}
      onExportGold={exportGoldEvalSet}
      onExportFinetune={exportFinetunePack}
      onBackup={backupToFolder}
      onRestoreFile={restoreFromFile}
      onToggleSnapshots={toggleSnapshotList}
      onRestoreSnapshot={restoreSnapshot}
      onCompact={compactDatabase}
    />
  {:else if errorMessage}
    <div class="flex flex-col items-center justify-center py-8 text-red-400 space-y-2">
      <TriangleAlert class="h-10 w-10 opacity-60" strokeWidth={1.5} aria-hidden="true" />
      <span class="text-sm font-medium">{$t('stats.failed')}</span>
      <p class="text-xs text-red-500/80 max-w-xs text-center break-words">{errorMessage}</p>
    </div>
  {:else}
    <div class="flex flex-col items-center justify-center py-8 text-cortex-500 space-y-2">
      <ChartNoAxesColumnIncreasing
        class="h-10 w-10 opacity-30"
        strokeWidth={1.5}
        aria-hidden="true"
      />
      <span class="text-sm">{$t('stats.noData')}</span>
      <p class="text-xs text-cortex-600">{$t('stats.loadHint')}</p>
    </div>
  {/if}
</div>
