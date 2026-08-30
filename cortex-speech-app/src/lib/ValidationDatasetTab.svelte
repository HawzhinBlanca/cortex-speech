<script lang="ts">
  import type { ValidationIssue } from './commands';
  import { isTranslationKey, t } from './i18n';

  let {
    loading,
    totalSegments,
    passed,
    errors,
    warnings,
    showErrors = $bindable(true),
    showWarnings = $bindable(true),
    onJump,
  }: {
    loading: boolean;
    totalSegments: number;
    passed: number;
    errors: ValidationIssue[];
    warnings: ValidationIssue[];
    showErrors?: boolean;
    showWarnings?: boolean;
    onJump: (issue: ValidationIssue) => void;
  } = $props();

  const severityClass = (severity: string): string =>
    severity === 'Error'
      ? 'border-red-500/40 bg-red-950/30 text-red-200'
      : 'border-amber-500/40 bg-amber-950/30 text-amber-200';
  const categoryLabel = (category: string): string => {
    const key = `validation.category.${category}`;
    return isTranslationKey(key) ? $t(key) : category;
  };
</script>

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
        class="w-full flex items-center justify-between text-xs font-semibold text-red-300 uppercase tracking-wider mb-2 bg-transparent border-0 text-start p-0 cursor-pointer"
        onclick={() => (showErrors = !showErrors)}
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
                  {#if issue.details}<p class="opacity-70">{issue.details}</p>{/if}
                  {#if issue.field}<p class="opacity-60 font-mono">{issue.field}</p>{/if}
                </div>
                {#if issue.segmentId}
                  <button
                    class="btn-secondary !text-[10px] !px-2 !py-1 shrink-0"
                    onclick={() => onJump(issue)}>{$t('validation.goToSegment')}</button
                  >
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
        class="w-full flex items-center justify-between text-xs font-semibold text-amber-300 uppercase tracking-wider mb-2 bg-transparent border-0 text-start p-0 cursor-pointer"
        onclick={() => (showWarnings = !showWarnings)}
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
                  {#if issue.details}<p class="opacity-70">{issue.details}</p>{/if}
                </div>
                {#if issue.segmentId}
                  <button
                    class="btn-secondary !text-[10px] !px-2 !py-1 shrink-0"
                    onclick={() => onJump(issue)}>{$t('validation.goToSegment')}</button
                  >
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}

  {#if errors.length === 0 && warnings.length === 0}
    <div class="flex flex-col items-center py-8 text-emerald-400 space-y-2" role="status">
      <p class="text-sm">{$t('validation.allClear')}</p>
    </div>
  {/if}
{/if}
