<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './commands';
  import type { DatasetStats, SpeechSegment } from './types';
  import { segments } from './stores/segmentStore';
  import { notifications } from './stores/notificationStore';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';

  let stats = $state<DatasetStats | null>(null);
  let quality = $state<import('./commands').DatasetQuality | null>(null);
  let cert = $state<import('./commands').ConformalCertificate | null>(null);
  let inferenceStats = $state<{
    vad: { calls: number; failures: number; p50_ms: number; p99_ms: number };
    asr: { calls: number; failures: number; p50_ms: number; p99_ms: number };
    model_load_ms: number;
  } | null>(null);
  let loading = $state(true);
  let errorMessage = $state<string | null>(null);
  const tauriAvailable = isTauriRuntime();

  function buildLocalStats(items: SpeechSegment[]): DatasetStats {
    const durationSeconds = items.map((segment) => Math.max(0, segment.durationMs || 0) / 1000);
    const totalDurationSeconds = durationSeconds.reduce((sum, value) => sum + value, 0);
    const verifiedCount = items.filter((segment) => segment.verified).length;
    const totalChars = items.reduce((sum, segment) => {
      const text =
        segment.normalizedTranscript || segment.annotatedTranscript || segment.rawTranscript || '';
      return sum + text.length;
    }, 0);
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
      pendingCount: items.length - verifiedCount,
      verificationRate: items.length ? (verifiedCount / items.length) * 100 : 0,
      uniqueSpeakers: speakerDurations.size,
      totalChars,
      avgCharsPerSegment: items.length ? totalChars / items.length : 0,
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
        cert = await api.getDatasetCertificate(0.05, 0.95);
      } catch (err) {
        console.error('Failed to load conformal certificate', err);
      }
    } catch (e) {
      errorMessage = String(e);
      notifications.error($t('stats.failed'), { detail: String(e) });
    } finally {
      loading = false;
    }
  }

  function track(..._args: unknown[]) {}

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

  function fmt(s: number) {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = Math.floor(s % 60);
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${sec}s`;
    return `${sec}s`;
  }

  function pct(v: number) {
    return `${v.toFixed(1)}%`;
  }

  function fmtMs(v: number) {
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
            <div class="bg-cortex-800/30 rounded-lg p-2">
              <div
                class="text-sm font-bold {quality.qualityGatePassed
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
                class="text-sm font-bold {quality.qualityGatePassed
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
                "<bdi>{group.normalizedPreview}</bdi>" — <bdi>{group.segmentIds.length}
                  {$t('stats.segShort')}</bdi>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/if}

    <div class="space-y-1">
      <span class="text-xs text-cortex-400">{$t('stats.durationDistribution')}</span>
      <div class="flex gap-1 h-16 items-end">
        {#each [{ label: '<5s', value: stats.durationHistogram.under5s }, { label: '<10s', value: stats.durationHistogram.under10s }, { label: '<15s', value: stats.durationHistogram.under15s }, { label: '<30s', value: stats.durationHistogram.under30s }, { label: '30s+', value: stats.durationHistogram.over30s }] as bar}
          {#if stats.totalSegments > 0}
            <div class="flex-1 flex flex-col items-center gap-1">
              <div
                class="w-full bg-cortex-600 rounded-t transition-all duration-500"
                style="height: {(bar.value / stats.totalSegments) * 100}%"
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
              <div class="text-lg font-bold text-cyan-400">{cert.totalCertified}</div>
              <div class="text-[9px] text-cortex-400">{$t('stats.certifiedSegments')}</div>
            </div>
            <div class="bg-cortex-950/40 p-2 rounded-lg border border-cortex-800/20">
              <div class="text-lg font-bold text-cortex-200">{cert.threshold.toFixed(3)}</div>
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
              <span class="font-semibold text-emerald-400"
                >{(cert.expectedErrorBound * 100).toFixed(1)}%</span
              >
            </div>
          </div>

          {#if !cert.isCalibrated}
            <p class="text-[9px] text-amber-400/90 leading-tight">
              ⚠️ Uncalibrated fallback. Verify at least 10 segments to enable statistical risk
              bounds.
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
