<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './commands';
  import type { DatasetStats, SpeechSegment } from './types';
  import { segments } from './stores/segmentStore';
  import { notifications } from './stores/notificationStore';
  import {
    isVerifiedGood,
    isHumanRejected,
    isPlaceholderTranscript,
    effectiveTranscript,
  } from './segmentQuality';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';

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
  let inferenceStats = $state<{
    vad: { calls: number; failures: number; p50_ms: number; p99_ms: number };
    asr: { calls: number; failures: number; p50_ms: number; p99_ms: number };
    model_load_ms: number;
  } | null>(null);
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

  // ── P0 #7: decisions, not density ──────────────────────────────────────────────────────────────
  // Insights leads with one question — can I export a training set? — and, when the answer is no,
  // what is stopping it. Every blocker below is a REAL count from an existing computation; none is
  // estimated, and none is invented for the panel.
  type Blocker = { id: string; count?: number; detail?: string; action?: 'relink' | 'review' };

  const blockers = $derived.by<Blocker[]>(() => {
    const out: Blocker[] = [];
    // Ordered by what to do first: broken input, then unreviewed work, then data faults.
    if (audioHealth && audioHealth.missingFiles > 0) {
      out.push({ id: 'audioMissing', count: audioHealth.missingFiles, action: 'relink' });
    }
    if (stats && stats.pendingCount > 0) {
      out.push({ id: 'pendingReview', count: stats.pendingCount, action: 'review' });
    }
    if (quality && quality.emptyTranscriptCount > 0) {
      out.push({ id: 'emptyTranscripts', count: quality.emptyTranscriptCount });
    }
    if (quality && !quality.qualityGatePassed) {
      out.push({
        id: 'qualityGate',
        count: quality.segmentsAboveWerThreshold + quality.segmentsAboveCerThreshold,
      });
    }
    // Nothing exportable AND the library is non-empty: name the dominant grade reason. Without this
    // the owner sees "0 ready" on a fully-reviewed library and has no way to learn that the cause is
    // a missing word aligner rather than anything more reviewing could fix.
    if (
      breakdown &&
      breakdown.summary.totalSegments > 0 &&
      breakdown.summary.trainingReadySegments === 0
    ) {
      const top = Object.entries(breakdown.reasonCounts)
        .filter(([reason]) => reason !== 'human_verified')
        .sort((a, b) => b[1] - a[1])[0];
      out.push({ id: 'nothingTrainingReady', detail: top?.[0], count: top?.[1] });
    }
    if (evalRuns !== null && evalRuns.length === 0) out.push({ id: 'noAccuracyRecord' });
    return out;
  });

  // 'unknown' is a real state, not a fallback to green: it means the readiness inputs did not load.
  const verdict = $derived<'ready' | 'notReady' | 'unknown'>(
    breakdown === null
      ? 'unknown'
      : blockers.length === 0 && breakdown.summary.trainingReadySegments > 0
        ? 'ready'
        : 'notReady',
  );

  const nextAction = $derived(blockers[0] ?? null);

  // ONE canonical accuracy record: the most recent gold eval, with the interval it was measured with.
  // A point estimate with no N, no date and no interval is the "Champion" claim the audit called out.
  const accuracy = $derived.by(() => {
    const run = evalRuns && evalRuns.length ? evalRuns[0] : null;
    if (!run) return null;
    let cerLow: number | null = null;
    let cerHigh: number | null = null;
    let werLow: number | null = null;
    let werHigh: number | null = null;
    try {
      const meta = run.metaJson ? JSON.parse(run.metaJson) : null;
      if (meta && typeof meta === 'object') {
        // ABSENT (not null) when there was nothing to resample — so a missing key means "no interval",
        // and the card shows the point estimate alone rather than inventing a range.
        cerLow = typeof meta.micro_cer_ci_low === 'number' ? meta.micro_cer_ci_low : null;
        cerHigh = typeof meta.micro_cer_ci_high === 'number' ? meta.micro_cer_ci_high : null;
        werLow = typeof meta.micro_wer_ci_low === 'number' ? meta.micro_wer_ci_low : null;
        werHigh = typeof meta.micro_wer_ci_high === 'number' ? meta.micro_wer_ci_high : null;
      }
    } catch {
      // A run whose meta will not parse still has real point estimates; show those, claim no interval.
    }
    return { run, cerLow, cerHigh, werLow, werHigh };
  });

  function buildLocalStats(items: SpeechSegment[]): DatasetStats {
    const durationSeconds = items.map((segment) => Math.max(0, segment.durationMs || 0) / 1000);
    const totalDurationSeconds = durationSeconds.reduce((sum, value) => sum + value, 0);
    // "Verified" = human-confirmed GOOD. Exclude human-rejected clips ("mark bad"), which carry
    // verified=true only to leave the review queue — counting them here would inflate the metric.
    const verifiedCount = items.filter((segment) => isVerifiedGood(segment)).length;
    // Mirrors compute_stats (stats.rs): the char total describes the SAME population as verifiedCount
    // and pendingCount above — rejected and placeholder clips excluded — and uses the same effective
    // transcript. Measured 2026-08-06, the old version overstated the live corpus by 7994 characters
    // (18.7%) by counting clips marked bad, and read a THIRD field order (normalized first) that
    // disagreed with both the backend and the effective-transcript rule.
    const countedForChars = items.filter(
      (segment) => !isHumanRejected(segment) && !isPlaceholderTranscript(effectiveTranscript(segment)),
    );
    const totalChars = countedForChars.reduce(
      (sum, segment) => sum + effectiveTranscript(segment).length,
      0,
    );
    const speakerDurations = new Map<
      string,
      { segmentCount: number; totalDurationSeconds: number }
    >();

    for (const segment of items) {
      const speakerId = segment.speakerId || 'unknown';
      const current = speakerDurations.get(speakerId) ?? {
        segmentCount: 0,
        totalDurationSeconds: 0,
      };
      current.segmentCount += 1;
      current.totalDurationSeconds += Math.max(0, segment.durationMs || 0) / 1000;
      speakerDurations.set(speakerId, current);
    }

    return {
      totalSegments: items.length,
      totalDurationSeconds,
      avgDurationSeconds: items.length ? totalDurationSeconds / items.length : 0,
      verifiedCount,
      // Pending = awaiting review. A human-rejected clip has ALREADY been reviewed (and discarded), so it
      // is neither verified nor pending — mirror the backend compute_stats so `items.length - verifiedCount`
      // does not silently bucket rejected clips into pending.
      pendingCount: items.filter((segment) => !isVerifiedGood(segment) && !isHumanRejected(segment))
        .length,
      verificationRate: items.length ? (verifiedCount / items.length) * 100 : 0,
      uniqueSpeakers: speakerDurations.size,
      totalChars,
      // Divided by the rows the sum covers, not by all items — see compute_stats.
      avgCharsPerSegment: countedForChars.length ? totalChars / countedForChars.length : 0,
      durationHistogram: {
        under5s: durationSeconds.filter((duration) => duration < 5).length,
        under10s: durationSeconds.filter((duration) => duration >= 5 && duration < 10).length,
        under15s: durationSeconds.filter((duration) => duration >= 10 && duration < 15).length,
        under30s: durationSeconds.filter((duration) => duration >= 15 && duration < 30).length,
        over30s: durationSeconds.filter((duration) => duration >= 30).length,
      },
      topSpeakers: Array.from(speakerDurations.entries())
        .map(([speakerId, value]) => ({ speakerId, ...value }))
        .sort(
          (a, b) =>
            b.segmentCount - a.segmentCount || b.totalDurationSeconds - a.totalDurationSeconds,
        )
        .slice(0, 5),
      // Review timing is backend-only (derived from decision_log); the non-Tauri fallback has none.
      reviewTiming: { decisionsLogged: 0, medianSeconds: null, samples: 0 },
      dbSizeBytes: 0, // backend-only (PRAGMA page_count × page_size)
    };
  }

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
      errorMessage = String(e);
      notifications.error($t('stats.failed'), { detail: String(e) });
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
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({ directory: true, multiple: false });
      if (typeof dir !== 'string') return;
      const result = await api.relinkAudio(dir);
      notifications.success(
        $t('stats.relinkDone')
          .replace('{n}', String(result.relinked))
          .replace('{m}', String(result.stillMissing)),
      );
      await fetchStats();
    } catch (e) {
      notifications.error($t('stats.relinkFailed'), { detail: String(e) });
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
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({ directory: true, multiple: false });
      if (typeof dir !== 'string') return null;
      return await run(dir);
    } catch (e) {
      notifications.error($t('stats.toolFailed'), { detail: String(e) });
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
          .replace('{h}', String(r.excludedHoldout))
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
      notifications.error($t('stats.toolFailed'), { detail: String(e) });
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
      notifications.error($t('stats.toolFailed'), { detail: String(e) });
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
      notifications.error($t('stats.toolFailed'), { detail: String(e) });
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
      window.location.reload();
    } catch (e) {
      notifications.error($t('stats.restoreFailed'), { detail: String(e) });
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
      const { open } = await import('@tauri-apps/plugin-dialog');
      const picked = await open({
        directory: false,
        multiple: false,
        filters: [{ name: 'Cortex backup', extensions: ['db'] }],
      });
      if (typeof picked !== 'string') return;
      src = picked;
    } catch (e) {
      notifications.error($t('stats.toolFailed'), { detail: String(e) });
      return;
    }
    if (!window.confirm($t('stats.restoreFileConfirm'))) return;
    toolBusy = 'restoreFile';
    try {
      await api.dbRestore(src);
      window.location.reload();
    } catch (e) {
      notifications.error($t('stats.restoreFailed'), { detail: String(e) });
      toolBusy = null;
    }
  }

  // P3.4: full-SHA integrity check of the bundled fine-tuned model (a few seconds — hashes 970 MB).
  // Surfaces the on-demand backend guard so the owner can confirm the champion is intact/uncorrupted.
  async function verifyModelIntegrity() {
    if (!tauriAvailable || toolBusy) return;
    toolBusy = 'verify';
    try {
      const message = await api.verifyFinetunedModelIntegrity();
      notifications.success($t('stats.verifyModelOk'), { detail: message });
    } catch (e) {
      notifications.error($t('stats.verifyModelFailed'), { detail: String(e) });
    } finally {
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

  // All three are fed values straight off the backend DatasetStats/cert. A null/undefined/NaN field
  // (e.g. an empty DB, or a field the backend omitted) would make `.toFixed` throw and drop the WHOLE
  // stats card into its error branch. Return a placeholder for non-finite input instead.
  function fmt(s: number) {
    if (!Number.isFinite(s)) return '\u2014';
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = Math.floor(s % 60);
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${sec}s`;
    return `${sec}s`;
  }

  function pct(v: number) {
    if (!Number.isFinite(v)) return '\u2014';
    return `${v.toFixed(1)}%`;
  }

  // EvalRun.cer/.wer are FRACTIONS (0..1). The pct() above takes an ALREADY-scaled percentage
  // (verificationRate is 0..100). Two scales behind one name is how a wrong number reaches the
  // screen, so fractions get their own formatter with its own name.
  function ratePct(v: number) {
    if (!Number.isFinite(v)) return '—';
    return `${(v * 100).toFixed(2)}%`;
  }

  function blockerText(b: Blocker): string {
    const n = String(b.count ?? 0);
    switch (b.id) {
      case 'audioMissing':
        return $t('stats.blockerAudioMissing', { n });
      case 'pendingReview':
        return $t('stats.blockerPendingReview', { n });
      case 'emptyTranscripts':
        return $t('stats.blockerEmptyTranscripts', { n });
      case 'qualityGate':
        return $t('stats.blockerQualityGate', { n });
      case 'nothingTrainingReady':
        return $t('stats.blockerNothingReady', { n });
      case 'noAccuracyRecord':
        return $t('stats.blockerNoAccuracyRecord');
      default:
        return b.id;
    }
  }

  function fmtMs(v: number) {
    if (!Number.isFinite(v)) return '\u2014';
    if (v < 1) return `${(v * 1000).toFixed(0)}\u00b5s`;
    if (v < 1000) return `${v.toFixed(1)}ms`;
    return `${(v / 1000).toFixed(2)}s`;
  }
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
    {#if audioHealth && audioHealth.missingFiles > 0}
      <div
        class="mb-3 flex items-center justify-between gap-3 rounded-lg border border-amber-600/40 bg-amber-950/30 p-3"
        data-testid="audio-missing-banner"
      >
        <span class="text-sm text-amber-300">
          {$t('stats.audioMissing').replace('{n}', String(audioHealth.missingFiles))}
        </span>
        <button
          type="button"
          class="btn btn-primary !text-xs"
          data-testid="relink-audio-btn"
          disabled={relinking}
          onclick={relinkMissingAudio}
        >
          {relinking ? $t('stats.relinking') : $t('stats.relink')}
        </button>
      </div>
    {/if}
    <!-- P0 #7: decisions before density. Insights answers ONE question first — can I export a
         training set? — then names what blocks it and what to do next. Diagnostics move below the
         Advanced disclosure. The verdict comes from get_training_grade_breakdown, the same rule the
         export gates on, so it can never disagree with what an export would actually write. -->
    <div class="space-y-3" data-testid="readiness-verdict">
      <div
        class="rounded-lg border p-3 {verdict === 'ready'
          ? 'border-emerald-600/40 bg-emerald-950/20'
          : verdict === 'notReady'
            ? 'border-amber-600/40 bg-amber-950/20'
            : 'border-cortex-700/40 bg-cortex-900/30'}"
      >
        <div class="flex items-baseline justify-between gap-3">
          <span
            class="text-lg font-bold {verdict === 'ready'
              ? 'text-emerald-300'
              : verdict === 'notReady'
                ? 'text-amber-300'
                : 'text-cortex-400'}"
            data-testid="readiness-headline"
          >
            {verdict === 'ready'
              ? $t('stats.ready')
              : verdict === 'notReady'
                ? $t('stats.notReady')
                : $t('stats.readinessUnknown')}
          </span>
          {#if breakdown}
            <span class="text-xs text-cortex-400" data-testid="readiness-count">
              <bdi dir="ltr"
                >{breakdown.summary.trainingReadySegments}/{breakdown.summary.totalSegments}</bdi
              >
            </span>
          {/if}
        </div>
        <p class="mt-1 text-[11px] text-cortex-400 leading-tight">{$t('stats.readyExplain')}</p>
      </div>

      {#if blockers.length > 0}
        <div class="space-y-1.5" data-testid="readiness-blockers">
          <span class="text-xs text-cortex-400 uppercase tracking-wider"
            >{$t('stats.blockers')}</span
          >
          {#each blockers as b (b.id)}
            <div class="flex items-center justify-between gap-2">
              <span class="text-xs text-cortex-200">
                {blockerText(b)}
                {#if b.detail}
                  <!-- The dominant grade reason, verbatim from the backend. Untranslated on purpose:
                       it is the exact token the export and the ledger use, so it stays greppable. -->
                  <bdi class="text-[10px] font-mono text-cortex-500">{b.detail}</bdi>
                {/if}
              </span>
              {#if b.action === 'relink'}
                <button
                  type="button"
                  class="btn btn-secondary !text-[11px] shrink-0"
                  data-testid="blocker-relink-btn"
                  disabled={relinking}
                  onclick={relinkMissingAudio}
                >
                  {relinking ? $t('stats.relinking') : $t('stats.relink')}
                </button>
              {:else if b.action === 'review' && onOpenReview}
                <button
                  type="button"
                  class="btn btn-secondary !text-[11px] shrink-0"
                  data-testid="blocker-review-btn"
                  onclick={onOpenReview}
                >
                  {$t('stats.blockerOpenReview')}
                </button>
              {/if}
            </div>
          {/each}
          {#if nextAction}
            <p class="text-[11px] text-cortex-300 pt-1" data-testid="readiness-next-action">
              <span class="text-cortex-500">{$t('stats.nextAction')}</span>
              {blockerText(nextAction)}
            </p>
          {/if}
        </div>
      {/if}

      <!-- ONE canonical accuracy record. A point estimate with no N, no date and no interval is
           precisely the unsupported "Champion" claim the audit called out. -->
      {#if accuracy}
        <div
          class="rounded-lg border border-cortex-700/40 bg-cortex-900/30 p-3 space-y-1"
          data-testid="accuracy-record"
        >
          <span class="text-xs text-cortex-400 uppercase tracking-wider">
            {$t('stats.accuracyTitle')}
          </span>
          <div class="flex flex-wrap items-baseline gap-x-4 gap-y-1">
            <span class="text-sm">
              <span class="text-cortex-500 text-[11px]">{$t('stats.cerLabel')}</span>
              <bdi dir="ltr" class="font-bold text-cortex-100">{ratePct(accuracy.run.cer)}</bdi>
              {#if accuracy.cerLow !== null && accuracy.cerHigh !== null}
                <bdi dir="ltr" class="text-[10px] text-cortex-500"
                  >[{ratePct(accuracy.cerLow)}–{ratePct(accuracy.cerHigh)}]</bdi
                >
              {/if}
            </span>
            <span class="text-sm">
              <span class="text-cortex-500 text-[11px]">{$t('stats.werLabel')}</span>
              <bdi dir="ltr" class="font-bold text-cortex-100">{ratePct(accuracy.run.wer)}</bdi>
              {#if accuracy.werLow !== null && accuracy.werHigh !== null}
                <bdi dir="ltr" class="text-[10px] text-cortex-500"
                  >[{ratePct(accuracy.werLow)}–{ratePct(accuracy.werHigh)}]</bdi
                >
              {/if}
            </span>
          </div>
          <div class="text-[10px] text-cortex-500 font-mono break-all">
            <bdi dir="ltr"
              >{accuracy.run.modelId} · N={accuracy.run.numSegs} · {accuracy.run.runAt} · {accuracy.run.id.slice(
                0,
                8,
              )}</bdi
            >
          </div>
        </div>
      {:else if evalRuns !== null}
        <p class="text-[11px] text-amber-400/90" data-testid="accuracy-none">
          {$t('stats.accuracyNone')}
        </p>
      {/if}
    </div>

    <div class="grid grid-cols-2 gap-3">
      <div class="bg-cortex-800/30 rounded-lg p-3">
        <div class="text-2xl font-bold text-cortex-200">{stats.totalSegments}</div>
        <div class="text-xs text-cortex-400">{$t('stats.totalSegments')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-3">
        <div class="text-2xl font-bold text-emerald-400">{fmt(stats.totalDurationSeconds)}</div>
        <div class="text-xs text-cortex-400">{$t('stats.totalDuration')}</div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-3">
        <div class="text-2xl font-bold text-amber-400">{pct(stats.verificationRate)}</div>
        <div class="text-xs text-cortex-400">
          {$t('stats.verified')} ({stats.verifiedCount}/{stats.totalSegments})
        </div>
      </div>
      <div class="bg-cortex-800/30 rounded-lg p-3">
        <div class="text-2xl font-bold text-cortex-300">{stats.uniqueSpeakers}</div>
        <div class="text-xs text-cortex-400">{$t('stats.uniqueSpeakers')}</div>
      </div>
      {#if stats.reviewTiming && stats.reviewTiming.medianSeconds !== null}
        <div class="bg-cortex-800/30 rounded-lg p-3" data-testid="stat-review-speed">
          <div class="text-2xl font-bold text-cortex-300">
            {stats.reviewTiming.medianSeconds.toFixed(1)}s
          </div>
          <div class="text-xs text-cortex-400">
            {$t('stats.reviewSpeed')} ({stats.reviewTiming.samples})
          </div>
        </div>
      {/if}
      {#if stats.dbSizeBytes > 0}
        <div class="bg-cortex-800/30 rounded-lg p-3" data-testid="stat-db-size">
          <div class="text-2xl font-bold text-cortex-300">
            {(stats.dbSizeBytes / 1048576).toFixed(1)} MB
          </div>
          <div class="text-xs text-cortex-400">{$t('stats.dbSize')}</div>
        </div>
      {/if}
      {#if fingerprintCount !== null}
        <div class="bg-cortex-800/30 rounded-lg p-3" data-testid="stat-fingerprints">
          <div class="text-2xl font-bold text-cortex-300">{fingerprintCount}</div>
          <div class="text-xs text-cortex-400">{$t('stats.fingerprints')}</div>
        </div>
      {/if}
    </div>

    {#if quality && quality.totalSegments > 0}
      <div class="space-y-2">
        <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
          {$t('stats.qualityTitle')}
        </h3>
        <div class="grid grid-cols-2 gap-2">
          <div class="bg-cortex-800/30 rounded-lg p-2">
            <div class="text-sm font-bold text-red-300">{quality.emptyTranscriptCount}</div>
            <div class="text-[10px] text-cortex-400">{$t('stats.emptyTranscripts')}</div>
          </div>
          <div class="bg-cortex-800/30 rounded-lg p-2">
            <div class="text-sm font-bold text-amber-300">{quality.lowConfidenceCount}</div>
            <div class="text-[10px] text-cortex-400">{$t('stats.lowConfidence')}</div>
          </div>
          <div class="bg-cortex-800/30 rounded-lg p-2">
            <div class="text-sm font-bold text-orange-300">{quality.duplicateTranscriptGroups}</div>
            <div class="text-[10px] text-cortex-400">
              {$t('stats.duplicateGroups')} ({quality.duplicateTranscriptSegments}
              {$t('stats.segShort')})
            </div>
          </div>
          <div class="bg-cortex-800/30 rounded-lg p-2">
            <div class="text-sm font-bold text-purple-300">{quality.durationOutlierCount}</div>
            <div class="text-[10px] text-cortex-400">{$t('stats.durationOutliers')}</div>
          </div>
        </div>
        {#if quality.annotatedSegmentCount > 0}
          <div class="grid grid-cols-2 gap-2">
            <!-- Round-24 #9: color each metric by ITS OWN real threshold-exceedance count, not the
                 quality-gate flag. The gate flag is true whenever enforce_quality_gates is off (the
                 DEFAULT), so it painted bad WER/CER green and used the SAME flag for both cells. Green
                 here means "no annotated segment exceeds this metric's threshold" — an honest measure. -->
            <div class="bg-cortex-800/30 rounded-lg p-2">
              <div
                class="text-sm font-bold {quality.segmentsAboveWerThreshold === 0
                  ? 'text-emerald-300'
                  : 'text-red-300'}"
              >
                {quality.meanWer != null ? `${(quality.meanWer * 100).toFixed(1)}%` : '—'}
              </div>
              <div class="text-[10px] text-cortex-400">
                {$t('stats.meanWer')} ({quality.annotatedSegmentCount}
                {$t('stats.annotated')})
              </div>
            </div>
            <div class="bg-cortex-800/30 rounded-lg p-2">
              <div
                class="text-sm font-bold {quality.segmentsAboveCerThreshold === 0
                  ? 'text-emerald-300'
                  : 'text-red-300'}"
              >
                {quality.meanCer != null ? `${(quality.meanCer * 100).toFixed(1)}%` : '—'}
              </div>
              <div class="text-[10px] text-cortex-400">{$t('stats.meanCer')}</div>
            </div>
            <div class="bg-cortex-800/30 rounded-lg p-2">
              <div class="text-sm font-bold text-orange-300">
                {quality.segmentsAboveWerThreshold}
              </div>
              <div class="text-[10px] text-cortex-400">{$t('stats.aboveWer')}</div>
            </div>
            <div class="bg-cortex-800/30 rounded-lg p-2">
              <div class="text-sm font-bold text-orange-300">
                {quality.segmentsAboveCerThreshold}
              </div>
              <div class="text-[10px] text-cortex-400">{$t('stats.aboveCer')}</div>
            </div>
          </div>
          {#if !quality.qualityGatePassed}
            <p class="text-[10px] text-red-400">{$t('stats.qualityGateFailed')}</p>
          {/if}
        {/if}
        {#if quality.duplicateGroups.length > 0}
          <div class="text-[10px] text-cortex-500 space-y-1 max-h-20 overflow-y-auto">
            {#each quality.duplicateGroups.slice(0, 3) as group}
              <div class="truncate" dir="rtl" lang="ckb">
                "<bdi>{group.normalizedPreview}</bdi>" —
                <bdi
                  >{group.segmentIds.length}
                  {$t('stats.segShort')}</bdi
                >
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    <!-- P0 #7: everything below is diagnostics — inference internals, model coverage, conformal
         details and the research tooling. Collapsed by default so the panel leads with the decision.
         Native <details> rather than a custom accordion: it is keyboard-operable and exposed to
         screen readers without any JS, which the accessibility gate needs anyway. -->
    <details class="group border-t border-cortex-800/50 pt-2" data-testid="stats-advanced">
      <summary
        class="cursor-pointer list-none text-xs font-semibold uppercase tracking-wider text-cortex-400 hover:text-cortex-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
      >
        <span class="inline-block transition-transform group-open:rotate-90">▸</span>
        {$t('stats.advanced')}
      </summary>
      <div class="space-y-4 pt-3">
        <div class="space-y-1">
          <span class="text-xs text-cortex-400">{$t('stats.durationDistribution')}</span>
          <div class="flex gap-1 h-16 items-end">
            {#each durationBuckets as bar}
              {#if stats.totalSegments > 0}
                <div class="flex-1 flex flex-col items-center gap-1">
                  <div
                    class="w-full bg-cortex-600 rounded-t transition-all duration-500"
                    style="height: {(bar.value / maxBucket) * 100}%"
                    title="{bar.value} ({stats.totalSegments > 0
                      ? ((bar.value / stats.totalSegments) * 100).toFixed(0)
                      : 0}%)"
                  ></div>
                  <span class="text-[10px] text-cortex-400">{bar.label}</span>
                </div>
              {/if}
            {/each}
          </div>
        </div>

        {#if stats.topSpeakers.length > 0}
          <div class="space-y-1">
            <span class="text-xs text-cortex-400">{$t('stats.topSpeakers')}</span>
            <div class="space-y-1 max-h-32 overflow-y-auto">
              {#each stats.topSpeakers as speaker, i}
                <div class="flex items-center gap-2 text-xs">
                  <span class="text-cortex-500 w-4">{i + 1}.</span>
                  <span class="text-cortex-200 flex-1 truncate">{speaker.speakerId}</span>
                  <span class="text-cortex-400">{speaker.segmentCount} {$t('stats.segShort')}</span>
                  <span class="text-cortex-500">{fmt(speaker.totalDurationSeconds)}</span>
                </div>
              {/each}
            </div>
          </div>
        {/if}

        {#if cert}
          <div class="space-y-2 pt-2 border-t border-cortex-800/50" data-testid="conformal-cert">
            <h3
              class="text-xs font-semibold text-cortex-300 uppercase tracking-wider flex items-center justify-between"
            >
              <span>{$t('stats.conformalTitle')}</span>
              {#if cert.isCalibrated}
                <span
                  class="text-[9px] px-1.5 py-0.5 rounded bg-emerald-950/50 text-emerald-400 border border-emerald-800/40 font-mono"
                  >{$t('stats.calibrated')}</span
                >
              {:else}
                <span
                  class="text-[9px] px-1.5 py-0.5 rounded bg-amber-950/50 text-amber-400 border border-amber-800/40 font-mono"
                  >{$t('stats.heuristic')}</span
                >
              {/if}
            </h3>

            <div class="bg-cortex-900/40 border border-cortex-800/40 rounded-xl p-3 space-y-2">
              <div class="grid grid-cols-2 gap-2 text-center">
                <div class="bg-cortex-950/40 p-2 rounded-lg border border-cortex-800/20">
                  <!-- An UNCALIBRATED certificate has no defensible certified count, so it shows the same
                   "—" this panel already uses for the threshold and RefineryPanel uses for a metric over
                   zero segments (undefined, never 0). Measured on the live library 2026-08-03:
                   `confidence` and `ctc_score` are NULL on all 144 rows, so every segment scores
                   nonconformity (1-0.5) + 0.1*5 = 1.0 from the two defaults alone. No cut point beats
                   the target, the fallback threshold becomes that same 1.0, and EVERY non-rejected
                   segment passes it — 144 - 27 rejects = the 117 that was rendering here in
                   success-cyan as "Certified Segments". A tautology, not a certification. -->
                  <div class="text-lg font-bold" class:text-cyan-400={cert.isCalibrated}>
                    {cert.isCalibrated ? cert.totalCertified : '—'}
                  </div>
                  <div class="text-[9px] text-cortex-400">{$t('stats.certifiedSegments')}</div>
                </div>
                <div class="bg-cortex-950/40 p-2 rounded-lg border border-cortex-800/20">
                  <div class="text-lg font-bold text-cortex-200">
                    {Number.isFinite(cert.threshold) ? cert.threshold.toFixed(3) : '—'}
                  </div>
                  <div class="text-[9px] text-cortex-400">Decision Threshold (τ)</div>
                </div>
              </div>

              <div class="text-[10px] text-cortex-400 space-y-1">
                <div class="flex justify-between">
                  <span>Target Error Bound (CER):</span>
                  <span class="font-semibold text-cortex-200"
                    >{(cert.targetError * 100).toFixed(0)}%</span
                  >
                </div>
                <div class="flex justify-between">
                  <span>Confidence Level:</span>
                  <span class="font-semibold text-cortex-200"
                    >{(cert.confidenceLevel * 100).toFixed(0)}%</span
                  >
                </div>
                <div class="flex justify-between">
                  <span>Expected Error Bound:</span>
                  {#if cert.isCalibrated}
                    <span class="font-semibold text-emerald-400"
                      >{(cert.expectedErrorBound * 100).toFixed(1)}%</span
                    >
                  {:else}
                    <!-- Uncalibrated: expectedErrorBound is just the requested target echoed back,
                     not a measured/achieved bound — never show it in success-green. -->
                    <span
                      class="font-semibold text-amber-400/90"
                      title="Uncalibrated — this is the requested target, not a measured bound"
                      >n/a (uncalibrated)</span
                    >
                  {/if}
                </div>
              </div>

              {#if cert.calibrationNoConfidence > 0 && cert.calibrationRealPosterior + cert.calibrationHeuristic === 0}
                <!-- The reason nothing certified is that NOTHING HAS A CONFIDENCE, not that too few clips are
                 verified. Saying "verify at least 10 segments" here sends the owner to do work that cannot
                 help: measured 2026-08-04, his library has 67 verified clips and 0/144 carry a confidence
                 or a ctc_score, because the cloud engine returns none (pipeline.rs: "Scribe returns no
                 per-segment confidence"). Naming the real cause is the difference between an honest empty
                 state and a wild goose chase. -->
                <p
                  class="text-[9px] text-amber-400/90 leading-tight"
                  data-testid="conformal-no-confidence"
                >
                  {$t('stats.conformalNoConfidence')}
                </p>
              {:else if !cert.isCalibrated}
                <p class="text-[9px] text-amber-400/90 leading-tight">
                  ⚠️ Uncalibrated fallback. Verify at least 10 segments to enable statistical risk
                  bounds.
                </p>
              {/if}

              {#if cert.calibrationRealPosterior === 0 && cert.calibrationHeuristic > 0}
                <!-- Honesty: even a statistically-calibrated cert can be built entirely on the heuristic
                 confidence fallback (the default offline CTC engine emits no token posteriors). Disclose
                 the provenance so the number is never over-trusted as a calibrated posterior. -->
                <p
                  class="text-[9px] text-amber-400/90 leading-tight"
                  data-testid="conformal-heuristic-basis"
                >
                  {$t('stats.conformalHeuristicBasis')}
                </p>
              {/if}
            </div>
          </div>
        {/if}

        {#if inferenceStats}
          <div class="space-y-2">
            <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
              {$t('stats.inferenceTitle')}
            </h3>

            <div class="grid grid-cols-2 gap-2">
              <div class="bg-cortex-800/30 rounded-lg p-2">
                <div class="text-[10px] text-cortex-400 mb-1">{$t('inference.vad')}</div>
                <div class="text-sm font-bold text-cortex-200">
                  {inferenceStats.vad.calls}
                  {$t('inference.calls')}
                </div>
                <div class="text-[10px] text-cortex-500">
                  {inferenceStats.vad.failures}
                  {$t('inference.failures')}
                  {#if inferenceStats.vad.calls > 0}
                    &middot; {(
                      (1 - inferenceStats.vad.failures / inferenceStats.vad.calls) *
                      100
                    ).toFixed(1)}% ok
                  {/if}
                </div>
                <div class="text-[10px] text-cortex-500 mt-0.5">
                  {$t('inference.p50')}
                  {fmtMs(inferenceStats.vad.p50_ms)} &middot; {$t('inference.p99')}
                  {fmtMs(inferenceStats.vad.p99_ms)}
                </div>
              </div>

              <div class="bg-cortex-800/30 rounded-lg p-2">
                <div class="text-[10px] text-cortex-400 mb-1">{$t('inference.asr')}</div>
                <div class="text-sm font-bold text-cortex-200">
                  {inferenceStats.asr.calls}
                  {$t('inference.calls')}
                </div>
                <div class="text-[10px] text-cortex-500">
                  {inferenceStats.asr.failures}
                  {$t('inference.failures')}
                  {#if inferenceStats.asr.calls > 0}
                    &middot; {(
                      (1 - inferenceStats.asr.failures / inferenceStats.asr.calls) *
                      100
                    ).toFixed(1)}% ok
                  {/if}
                </div>
                <div class="text-[10px] text-cortex-500 mt-0.5">
                  {$t('inference.p50')}
                  {fmtMs(inferenceStats.asr.p50_ms)} &middot; {$t('inference.p99')}
                  {fmtMs(inferenceStats.asr.p99_ms)}
                </div>
              </div>
            </div>

            {#if inferenceStats.model_load_ms > 0}
              <div class="text-[10px] text-cortex-500">
                {$t('inference.modelLoad')}: {fmtMs(inferenceStats.model_load_ms)}
              </div>
            {/if}
          </div>
        {/if}

        {#if intel && (intel.loop0Shadow.totalObservations > 0 || intel.autoAcceptPrecision.t0Accepts + intel.autoAcceptPrecision.t1Escalations > 0)}
          <div
            class="space-y-2 pt-2 border-t border-cortex-800/50"
            data-testid="intelligence-report"
          >
            <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
              {$t('stats.intelTitle')}
            </h3>
            <div class="grid grid-cols-2 gap-2">
              <div class="bg-cortex-800/30 rounded-lg p-2">
                <!-- Emerald ONLY when the gate has actually been exercised (some would-fire evidence
                 exists): an evidence-free 0 rendered green visually asserted the C5 "over-triggers
                 must be 0" go-live gate was passing when there was no evidence at all (true-10
                 audit 2026-07-09). Neutral until wouldFire > 0. -->
                <div
                  class="text-sm font-bold {intel.loop0Shadow.wouldFire === 0
                    ? 'text-cortex-400'
                    : intel.loop0Shadow.firedButHumanAcceptedOriginal === 0
                      ? 'text-emerald-300'
                      : 'text-red-300'}"
                  data-testid="loop0-overtriggers"
                >
                  {intel.loop0Shadow.wouldFire === 0
                    ? '—'
                    : intel.loop0Shadow.firedButHumanAcceptedOriginal}
                </div>
                <div class="text-[10px] text-cortex-400">
                  {$t('stats.loop0OverTriggers')} ({intel.loop0Shadow.wouldFire}/{intel.loop0Shadow
                    .totalObservations}
                  {$t('stats.loop0WouldFire')}{intel.loop0Shadow.wouldFire === 0
                    ? ` · ${$t('stats.noEvidenceYet')}`
                    : ''})
                </div>
              </div>
              <div class="bg-cortex-800/30 rounded-lg p-2">
                <div class="text-sm font-bold text-cortex-200" data-testid="c4-precision">
                  {intel.autoAcceptPrecision.t0HumanConfirmed +
                    intel.autoAcceptPrecision.t0HumanContradicted >
                  0
                    ? `${(
                        (100 * intel.autoAcceptPrecision.t0HumanConfirmed) /
                        (intel.autoAcceptPrecision.t0HumanConfirmed +
                          intel.autoAcceptPrecision.t0HumanContradicted)
                      ).toFixed(0)}%`
                    : '—'}
                </div>
                <div class="text-[10px] text-cortex-400">
                  {$t('stats.c4Precision')} ({intel.autoAcceptPrecision.t0HumanConfirmed}/{intel
                    .autoAcceptPrecision.t0HumanConfirmed +
                    intel.autoAcceptPrecision.t0HumanContradicted}
                  · T0 {intel.autoAcceptPrecision.t0Accepts} / T1 {intel.autoAcceptPrecision
                    .t1Escalations})
                </div>
              </div>
            </div>
            {#if intel.conformalCalibration}
              <!-- Honest distance-to-calibration: why the jury escalates everything today, and how far
               auto-accept is per acoustic bucket at the shipped 5%-CER gate (true-10 audit C3). -->
              <div class="bg-cortex-800/30 rounded-lg p-2" data-testid="conformal-progress">
                <div class="text-[10px] text-cortex-400 mb-1">
                  {$t('stats.conformalProgress', {
                    n: String(intel.conformalCalibration.minNeededAtZeroCer),
                  })}
                </div>
                <div class="flex flex-wrap gap-x-3 gap-y-0.5">
                  {#each intel.conformalCalibration.buckets as b (b.bucket)}
                    <span class="text-[10px] text-cortex-500">
                      <bdi>{b.bucket}</bdi>: {b.verifiedWithReference}/{b.minNeededAtZeroCer}
                    </span>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
        {/if}

        {#if tauriAvailable}
          <div class="space-y-2 pt-2 border-t border-cortex-800/50" data-testid="dataset-tools">
            <h3 class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
              {$t('stats.tools')}
            </h3>
            <div class="flex flex-col gap-2">
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="import-verified-gold-btn"
                disabled={toolBusy !== null}
                onclick={importVerifiedAsGold}
              >
                {toolBusy === 'importGold'
                  ? $t('stats.toolWorking')
                  : $t('stats.importVerifiedGold')}
              </button>
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="export-gold-eval-btn"
                disabled={toolBusy !== null}
                onclick={exportGoldEvalSet}
              >
                {toolBusy === 'goldEval' ? $t('stats.toolWorking') : $t('stats.exportGoldEvalSet')}
              </button>
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="export-finetune-pack-btn"
                disabled={toolBusy !== null}
                onclick={exportFinetunePack}
              >
                {toolBusy === 'finetunePack'
                  ? $t('stats.toolWorking')
                  : $t('stats.exportFinetunePack')}
              </button>
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="verify-model-btn"
                disabled={toolBusy !== null}
                onclick={verifyModelIntegrity}
              >
                {toolBusy === 'verify' ? $t('stats.toolWorking') : $t('stats.verifyModel')}
              </button>
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="backup-db-btn"
                disabled={toolBusy !== null}
                onclick={backupToFolder}
              >
                {toolBusy === 'backup' ? $t('stats.toolWorking') : $t('stats.backupDb')}
              </button>
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="restore-file-btn"
                disabled={toolBusy !== null}
                onclick={restoreFromFile}
              >
                {toolBusy === 'restoreFile' ? $t('stats.toolWorking') : $t('stats.restoreFile')}
              </button>
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="restore-snapshot-btn"
                disabled={toolBusy !== null}
                onclick={toggleSnapshotList}
              >
                {toolBusy === 'listSnapshots'
                  ? $t('stats.toolWorking')
                  : $t('stats.restoreSnapshot')}
              </button>
              <button
                type="button"
                class="btn btn-secondary !text-xs !justify-start"
                data-testid="compact-db-btn"
                disabled={toolBusy !== null}
                onclick={compactDatabase}
              >
                {toolBusy === 'compact' ? $t('stats.toolWorking') : $t('stats.compactDb')}
              </button>
            </div>
            {#if snapshots}
              <div class="space-y-1 max-h-40 overflow-y-auto" data-testid="snapshot-list">
                {#if snapshots.length === 0}
                  <p class="text-[10px] text-cortex-500">{$t('stats.noSnapshots')}</p>
                {:else}
                  {#each snapshots as snap}
                    <div
                      class="flex items-center gap-2 text-xs bg-cortex-800/30 rounded-lg px-2 py-1"
                    >
                      <span class="text-cortex-300 font-mono flex-1">
                        {new Date(snap.timestamp * 1000).toLocaleString()}
                      </span>
                      <span class="text-cortex-400">
                        {snap.segmentCount === null ? '?' : snap.segmentCount}
                        {$t('stats.segShort')} · {(snap.dbSizeBytes / 1048576).toFixed(1)} MB
                      </span>
                      <button
                        type="button"
                        class="btn btn-primary !text-[10px] !px-2 !py-0.5"
                        disabled={toolBusy !== null}
                        onclick={() => restoreSnapshot(snap.name, snap.segmentCount)}
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
        {/if}
      </div>
    </details>
  {:else if errorMessage}
    <div class="flex flex-col items-center justify-center py-8 text-red-400 space-y-2">
      <svg class="w-10 h-10 opacity-60" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="1.5"
          d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"
        />
      </svg>
      <span class="text-sm font-medium">{$t('stats.failed')}</span>
      <p class="text-xs text-red-500/80 max-w-xs text-center break-words">{errorMessage}</p>
    </div>
  {:else}
    <div class="flex flex-col items-center justify-center py-8 text-cortex-500 space-y-2">
      <svg class="w-10 h-10 opacity-30" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="1.5"
          d="M9 19v-6a2 2 0 00-2-2H5a2 2 0 00-2 2v6a2 2 0 002 2h2a2 2 0 002-2zm0 0V9a2 2 0 012-2h2a2 2 0 012 2v10m-6 0a2 2 0 002 2h2a2 2 0 002-2m0 0V5a2 2 0 012-2h2a2 2 0 012 2v14a2 2 0 01-2 2h-2a2 2 0 01-2-2z"
        />
      </svg>
      <span class="text-sm">{$t('stats.noData')}</span>
      <p class="text-xs text-cortex-600">{$t('stats.loadHint')}</p>
    </div>
  {/if}
</div>
