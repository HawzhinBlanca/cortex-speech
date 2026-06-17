<script lang="ts">
  import { onMount } from 'svelte';
  import { focusTrap } from './actions/focusTrap';
  import * as api from './commands';
  import type { ValidationIssue } from './commands';
  import type { SpeechSegment } from './types';
  import { showValidationPanel } from './stores/uiStore';
  import { notifications } from './stores/notificationStore';
  import { selectedSegmentId } from './stores/segmentStore';
  import { t } from './i18n';

  let loading = $state(true);
  let summary = $state('');
  let errors = $state<ValidationIssue[]>([]);
  let warnings = $state<ValidationIssue[]>([]);
  let totalSegments = $state(0);
  let passed = $state(0);
  let showErrors = $state(true);
  let showWarnings = $state(true);

  // Tabs: 'dataset' | 'active' | 'ood'
  let activeTab = $state<'dataset' | 'active' | 'ood'>('dataset');

  // Active learning parameters & queue
  let targetError = $state(0.05);
  let confidenceLevel = $state(0.95);
  let queueLimit = $state(20);
  let activeQueueLoading = $state(false);
  let activeQueue = $state<SpeechSegment[]>([]);

  // OOD status, running status, and flagged segments list
  let oodRunning = $state(false);
  let oodSegments = $state<SpeechSegment[]>([]);
  let showAllSegmentsForOod = $state(false);

  let displayedOodSegments = $derived.by(() => {
    if (showAllSegmentsForOod) {
      return oodSegments;
    } else {
      return oodSegments.filter(s => (s.oodScore || 0) > 0.35);
    }
  });

  async function runValidation() {
    loading = true;
    try {
      const result = await api.validateDataset();
      summary = result.summary;
      errors = result.errors;
      warnings = result.warnings;
      totalSegments = result.totalSegments;
      passed = result.passed;
    } catch (e) {
      notifications.error($t('validation.failed'), { detail: String(e) });
      showValidationPanel.set(false);
    } finally {
      loading = false;
    }
  }

  async function loadActiveLearningQueue() {
    activeQueueLoading = true;
    // Clamp user inputs — HTML min/max don't prevent typed values
    const clampedTarget = Math.max(0.01, Math.min(0.50, targetError));
    const clampedConf = Math.max(0.50, Math.min(0.99, confidenceLevel));
    const clampedLimit = Math.max(1, Math.min(100, Math.floor(queueLimit)));
    try {
      const queue = await api.getActiveLearningQueue(clampedTarget, clampedConf, clampedLimit);
      activeQueue = queue;
    } catch (e) {
      notifications.error($t('validation.failed'), { detail: String(e) });
    } finally {
      activeQueueLoading = false;
    }
  }

  async function runOodDetection() {
    oodRunning = true;
    try {
      const count = await api.computeOodScores();
      notifications.success($t('validation.ood.success').replace('{n}', String(count)));
      await reloadOodSegments();
    } catch (e) {
      notifications.error($t('validation.failed'), { detail: String(e) });
    } finally {
      oodRunning = false;
    }
  }

  async function reloadOodSegments() {
    try {
      const allSegments = await api.getSegments();
      oodSegments = allSegments
        .filter(s => s.oodScore !== undefined && s.oodScore !== null)
        .sort((a, b) => (b.oodScore || 0) - (a.oodScore || 0));
    } catch (e) {
      console.error("Failed to load OOD segments", e);
      notifications.error($t('validation.failed'), { detail: String(e) });
    }
  }

  function close() {
    showValidationPanel.set(false);
  }

  function jumpToSegment(issue: ValidationIssue) {
    if (issue.segmentId) {
      selectedSegmentId.set(issue.segmentId);
      close();
    }
  }

  function severityClass(severity: string): string {
    return severity === 'Error'
      ? 'border-red-500/40 bg-red-950/30 text-red-200'
      : 'border-amber-500/40 bg-amber-950/30 text-amber-200';
  }

  function categoryLabel(category: string): string {
    const key = `validation.category.${category}`;
    const label = $t(key);
    return label === key ? category : label;
  }

  onMount(() => {
    runValidation();
  });
</script>

<div
  data-testid="validation-panel"
  class="fixed inset-0 z-50 flex items-start justify-center pt-16 px-4 bg-black/60 backdrop-blur-sm"
  role="dialog"
  aria-modal="true"
  aria-labelledby="validation-title"
  tabindex="-1"
  use:focusTrap
  onclick={(e) => { if (e.target === e.currentTarget) close(); }}
  onkeydown={(e) => { if (e.key === 'Escape') close(); }}
>
  <div class="card w-full max-w-2xl max-h-[80vh] flex flex-col shadow-2xl">
    <header class="flex items-center justify-between px-4 py-3 border-b border-cortex-800/50 shrink-0">
      <div>
        <h2 id="validation-title" class="text-sm font-semibold text-cortex-100">{$t('validation.title')}</h2>
        {#if !loading && summary && activeTab === 'dataset'}
          <p class="text-xs text-cortex-400 mt-0.5">{summary}</p>
        {/if}
      </div>
      <button class="btn-secondary !text-xs !px-2 !py-1" onclick={close} aria-label={$t('close')}>✕</button>
    </header>

    <!-- Navigation Tabs -->
    <div class="flex border-b border-cortex-800/50 bg-cortex-900/40 p-1 shrink-0 gap-1">
      <button
        type="button"
        class="flex-1 py-1.5 text-xs font-semibold rounded-md transition-all duration-200 border-0 cursor-pointer
          {activeTab === 'dataset' ? 'bg-cortex-700 text-white shadow-sm font-bold' : 'bg-transparent text-cortex-400 hover:text-cortex-200 hover:bg-cortex-800/30'}"
        onclick={() => activeTab = 'dataset'}
      >
        {$t('validation.tab.dataset')}
      </button>
      <button
        type="button"
        class="flex-1 py-1.5 text-xs font-semibold rounded-md transition-all duration-200 border-0 cursor-pointer
          {activeTab === 'active' ? 'bg-cortex-700 text-white shadow-sm font-bold' : 'bg-transparent text-cortex-400 hover:text-cortex-200 hover:bg-cortex-800/30'}"
        onclick={() => { activeTab = 'active'; if (activeQueue.length === 0) loadActiveLearningQueue(); }}
      >
        {$t('validation.tab.activeLearning')}
      </button>
      <button
        type="button"
        class="flex-1 py-1.5 text-xs font-semibold rounded-md transition-all duration-200 border-0 cursor-pointer
          {activeTab === 'ood' ? 'bg-cortex-700 text-white shadow-sm font-bold' : 'bg-transparent text-cortex-400 hover:text-cortex-200 hover:bg-cortex-800/30'}"
        onclick={() => { const wasOod = activeTab === 'ood'; activeTab = 'ood'; if (!wasOod) reloadOodSegments(); }}
      >
        {$t('validation.tab.ood')}
      </button>
    </div>

    <div class="flex-1 overflow-y-auto p-4 space-y-4">
      <!-- Dataset Validation Tab -->
      {#if activeTab === 'dataset'}
        {#if loading}
          <div class="space-y-3">
            {#each [1, 2, 3, 4] as _}
              <div class="h-12 bg-cortex-800/30 rounded-lg animate-pulse"></div>
            {/each}
          </div>
        {:else}
          <div class="grid grid-cols-3 gap-2 text-center">
            <div class="bg-cortex-800/30 rounded-lg p-3">
              <div class="text-xl font-bold text-cortex-200">{totalSegments}</div>
              <div class="text-[10px] text-cortex-400">{$t('validation.total')}</div>
            </div>
            <div class="bg-emerald-950/30 rounded-lg p-3 border border-emerald-800/30">
              <div class="text-xl font-bold text-emerald-400">{passed}</div>
              <div class="text-[10px] text-cortex-400">{$t('validation.passed')}</div>
            </div>
            <div class="bg-red-950/30 rounded-lg p-3 border border-red-800/30">
              <div class="text-xl font-bold text-red-400">{errors.length}</div>
              <div class="text-[10px] text-cortex-400">{$t('validation.errors')}</div>
            </div>
          </div>

          {#if errors.length > 0}
            <section>
              <button
                type="button"
                class="w-full flex items-center justify-between text-xs font-semibold text-red-300 uppercase tracking-wider mb-2 bg-transparent border-0 text-left p-0 cursor-pointer"
                onclick={() => showErrors = !showErrors}
              >
                <span>{$t('validation.errors')} ({errors.length})</span>
                <span>{showErrors ? '−' : '+'}</span>
              </button>
              {#if showErrors}
                <ul class="space-y-2 p-0 list-none m-0">
                  {#each errors as issue}
                    <li class="rounded-lg border p-3 text-xs {severityClass(issue.severity)}">
                      <div class="flex items-start justify-between gap-2">
                        <div class="space-y-1 min-w-0">
                          <span class="font-medium">{categoryLabel(issue.category)}</span>
                          <p>{issue.message}</p>
                          {#if issue.details}
                            <p class="opacity-70">{issue.details}</p>
                          {/if}
                          {#if issue.field}
                            <p class="opacity-60 font-mono">{issue.field}</p>
                          {/if}
                        </div>
                        {#if issue.segmentId}
                          <button
                            class="btn-secondary !text-[10px] !px-2 !py-1 shrink-0"
                            onclick={() => jumpToSegment(issue)}
                          >{$t('validation.goToSegment')}</button>
                        {/if}
                      </div>
                    </li>
                  {/each}
                </ul>
              {/if}
            </section>
          {/if}

          {#if warnings.length > 0}
            <section>
              <button
                type="button"
                class="w-full flex items-center justify-between text-xs font-semibold text-amber-300 uppercase tracking-wider mb-2 bg-transparent border-0 text-left p-0 cursor-pointer"
                onclick={() => showWarnings = !showWarnings}
              >
                <span>{$t('validation.warnings')} ({warnings.length})</span>
                <span>{showWarnings ? '−' : '+'}</span>
              </button>
              {#if showWarnings}
                <ul class="space-y-2 p-0 list-none m-0">
                  {#each warnings as issue}
                    <li class="rounded-lg border p-3 text-xs {severityClass(issue.severity)}">
                      <div class="flex items-start justify-between gap-2">
                        <div class="space-y-1 min-w-0">
                          <span class="font-medium">{categoryLabel(issue.category)}</span>
                          <p>{issue.message}</p>
                          {#if issue.details}
                            <p class="opacity-70">{issue.details}</p>
                          {/if}
                        </div>
                        {#if issue.segmentId}
                          <button
                            class="btn-secondary !text-[10px] !px-2 !py-1 shrink-0"
                            onclick={() => jumpToSegment(issue)}
                          >{$t('validation.goToSegment')}</button>
                        {/if}
                      </div>
                    </li>
                  {/each}
                </ul>
              {/if}
            </section>
          {/if}

          {#if errors.length === 0 && warnings.length === 0}
            <div class="flex flex-col items-center py-8 text-emerald-400 space-y-2">
              <svg class="w-12 h-12" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"/>
              </svg>
              <p class="text-sm">{$t('validation.allClear')}</p>
            </div>
          {/if}
        {/if}
      {/if}

      <!-- Active Learning Tab -->
      {#if activeTab === 'active'}
        <div class="space-y-4">
          <div class="bg-cortex-900/30 p-3 rounded-lg border border-cortex-800/50 space-y-2">
            <h3 class="text-xs font-semibold text-cortex-200">{$t('validation.activeLearning.title')}</h3>
            <p class="text-xs text-cortex-400 leading-relaxed">{$t('validation.activeLearning.description')}</p>
            
            <div class="grid grid-cols-3 gap-3 pt-2">
              <div class="space-y-1">
                <label for="target-error" class="text-[10px] text-cortex-400">{$t('validation.activeLearning.targetError')}</label>
                <input
                  id="target-error"
                  type="number"
                  step="0.01"
                  min="0.01"
                  max="0.50"
                  class="input !text-xs !py-1 px-2 font-mono"
                  bind:value={targetError}
                />
              </div>
              <div class="space-y-1">
                <label for="confidence-level" class="text-[10px] text-cortex-400">{$t('validation.activeLearning.confidence')}</label>
                <input
                  id="confidence-level"
                  type="number"
                  step="0.01"
                  min="0.50"
                  max="0.99"
                  class="input !text-xs !py-1 px-2 font-mono"
                  bind:value={confidenceLevel}
                />
              </div>
              <div class="space-y-1">
                <label for="queue-limit" class="text-[10px] text-cortex-400">{$t('validation.activeLearning.limit')}</label>
                <input
                  id="queue-limit"
                  type="number"
                  step="1"
                  min="5"
                  max="100"
                  class="input !text-xs !py-1 px-2 font-mono"
                  bind:value={queueLimit}
                />
              </div>
            </div>
          </div>

          {#if activeQueueLoading}
            <div class="space-y-3">
              {#each [1, 2, 3] as _}
                <div class="h-12 bg-cortex-800/30 rounded-lg animate-pulse"></div>
              {/each}
            </div>
          {:else if activeQueue.length > 0}
            <ul class="space-y-2 p-0 list-none m-0">
              {#each activeQueue as segment}
                <li class="rounded-lg border border-cortex-800/40 bg-cortex-900/20 p-3 text-xs text-cortex-200">
                  <div class="flex items-start justify-between gap-4">
                    <div class="space-y-1 min-w-0">
                      <div class="flex items-center gap-2">
                        <span class="font-mono font-medium truncate text-cortex-100">{segment.audioPath.split(/[/\\]/).pop()}</span>
                        {#if segment.verified}
                          <span class="badge-verified shrink-0">{$t('verified')}</span>
                        {:else}
                          <span class="badge-pending shrink-0">{$t('pending')}</span>
                        {/if}
                      </div>
                      <p class="text-cortex-350 italic truncate mt-1">"{segment.annotatedTranscript || segment.rawTranscript || ''}"</p>
                      <div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-[10px] text-cortex-400 pt-1">
                        <span>{$t('duration')}: {(segment.durationMs / 1000).toFixed(2)}s</span>
                        {#if segment.confidence !== undefined}
                          <span>Confidence: {segment.confidence.toFixed(2)}</span>
                        {/if}
                        {#if segment.ctcScore !== undefined}
                          <span>CTC Match: {segment.ctcScore.toFixed(2)}</span>
                        {/if}
                        {#if segment.oodScore !== undefined && segment.oodScore !== null}
                          <span>OOD: {segment.oodScore.toFixed(2)}</span>
                        {/if}
                      </div>
                    </div>
                    <button
                      class="btn-secondary !text-[10px] !px-2 !py-1 shrink-0"
                      onclick={() => { selectedSegmentId.set(segment.id); close(); }}
                    >{$t('validation.goToSegment')}</button>
                  </div>
                </li>
              {/each}
            </ul>
          {:else}
            <div class="text-center py-8 text-cortex-500 text-xs italic">
              {$t('validation.activeLearning.noSegments')}
            </div>
          {/if}
        </div>
      {/if}

      <!-- OOD Audio Quality Tab -->
      {#if activeTab === 'ood'}
        <div class="space-y-4">
          <div class="bg-cortex-900/30 p-3 rounded-lg border border-cortex-800/50 space-y-2">
            <h3 class="text-xs font-semibold text-cortex-200">{$t('validation.ood.title')}</h3>
            <p class="text-xs text-cortex-400 leading-relaxed">{$t('validation.ood.description')}</p>
            
            <div class="flex items-center justify-between pt-1">
              <label class="flex items-center gap-2 text-[11px] text-cortex-300 cursor-pointer select-none">
                <input
                  type="checkbox"
                  class="rounded border-cortex-800 bg-cortex-950 text-cortex-500 focus:ring-0"
                  bind:checked={showAllSegmentsForOod}
                />
                Show all segments (not just OOD flagged)
              </label>
            </div>
          </div>

          {#if oodRunning}
            <div class="space-y-3">
              {#each [1, 2, 3] as _}
                <div class="h-12 bg-cortex-800/30 rounded-lg animate-pulse"></div>
              {/each}
            </div>
          {:else if displayedOodSegments.length > 0}
            <ul class="space-y-2 p-0 list-none m-0">
              {#each displayedOodSegments as segment}
                {@const isFlagged = (segment.oodScore || 0) > 0.35}
                <li class="rounded-lg border p-3 text-xs {isFlagged ? 'border-red-500/40 bg-red-950/20 text-red-200' : 'border-cortex-800/40 bg-cortex-900/20 text-cortex-200'}">
                  <div class="flex items-start justify-between gap-4">
                    <div class="space-y-1 min-w-0">
                      <div class="flex items-center gap-2">
                        <span class="font-mono font-medium truncate {isFlagged ? 'text-red-100' : 'text-cortex-100'}">{segment.audioPath.split(/[/\\]/).pop()}</span>
                        {#if isFlagged}
                          <span class="px-1.5 py-0.5 rounded bg-red-500/20 text-red-300 text-[9px] font-bold border border-red-500/30 uppercase shrink-0">{$t('validation.ood.isOod')}</span>
                        {/if}
                      </div>
                      <p class="opacity-80 italic truncate mt-1">"{segment.annotatedTranscript || segment.rawTranscript || ''}"</p>
                      <div class="flex items-center gap-4 text-[10px] opacity-70 pt-1">
                        <span class="font-semibold text-amber-400">{$t('validation.ood.score')}: {segment.oodScore?.toFixed(3)}</span>
                        <span>{$t('duration')}: {(segment.durationMs / 1000).toFixed(2)}s</span>
                      </div>
                    </div>
                    <button
                      class="btn-secondary !text-[10px] !px-2 !py-1 shrink-0"
                      onclick={() => { selectedSegmentId.set(segment.id); close(); }}
                    >{$t('validation.goToSegment')}</button>
                  </div>
                </li>
              {/each}
            </ul>
          {:else}
            <div class="text-center py-8 text-cortex-500 text-xs italic">
              {$t('validation.ood.noOod')}
            </div>
          {/if}
        </div>
      {/if}
    </div>

    <footer class="flex justify-end gap-2 px-4 py-3 border-t border-cortex-800/50 shrink-0">
      {#if activeTab === 'dataset'}
        <button class="btn-secondary !text-xs" onclick={runValidation} disabled={loading}>
          {$t('validation.rerun')}
        </button>
      {:else if activeTab === 'active'}
        <button class="btn-secondary !text-xs" onclick={loadActiveLearningQueue} disabled={activeQueueLoading}>
          {$t('validation.activeLearning.run')}
        </button>
      {:else if activeTab === 'ood'}
        <button class="btn-secondary !text-xs" onclick={runOodDetection} disabled={oodRunning}>
          {#if oodRunning}
            {$t('validation.ood.running')}
          {:else}
            {$t('validation.ood.run')}
          {/if}
        </button>
      {/if}
      <button class="btn-primary !text-xs" onclick={close}>{$t('close')}</button>
    </footer>
  </div>
</div>

