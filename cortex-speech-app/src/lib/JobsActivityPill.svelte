<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from './commands';
  import { t } from './i18n';

  // A quiet activity signal for durable background jobs (P0 #3). It surfaces only what needs the
  // owner's attention: work in progress, or a recent failure/interruption. When nothing is running
  // and nothing recently failed, it renders nothing — no idle clutter in the header.
  let jobs = $state<api.Job[]>([]);
  let poll: ReturnType<typeof setInterval> | undefined;

  async function refresh() {
    try {
      const result = await api.getJobs();
      // Trust boundary: a null/undefined/non-array response (a mock, an older backend, a serialization
      // hiccup) must not blow up the derived .filter below — coerce to an empty list.
      jobs = Array.isArray(result) ? result : [];
    } catch {
      // A failed read must never break the header; just keep the last known list.
    }
  }

  onMount(() => {
    void refresh();
    poll = setInterval(refresh, 5000);
  });
  onDestroy(() => {
    if (poll) clearInterval(poll);
  });

  const running = $derived(jobs.filter((j) => j.state === 'running'));
  // Flag a failure only when the most recent TERMINAL job failed (get_jobs is newest-first). This
  // self-clears: once the owner runs anything that succeeds (or cancels), the newest completed outcome
  // is no longer a failure and the pill goes away — an old failure buried under later successes is stale
  // news, not a standing alarm. Since durable failed rows are never pruned, a `find`-for-any-failed
  // would paint a permanent red pill forever (esp. a one-time crash's INTERRUPTED row).
  const newestTerminal = $derived(
    jobs.find((j) => j.state === 'failed' || j.state === 'succeeded' || j.state === 'cancelled'),
  );
  const latestFailed = $derived(newestTerminal?.state === 'failed' ? newestTerminal : undefined);
  const interrupted = $derived(latestFailed?.errorCode === 'INTERRUPTED');

  // Show a pill only when there is running work or a recent failure worth flagging.
  const mode = $derived<'running' | 'interrupted' | 'failed' | 'none'>(
    running.length > 0
      ? 'running'
      : latestFailed
        ? interrupted
          ? 'interrupted'
          : 'failed'
        : 'none',
  );

  const dot = $derived(mode === 'running' ? 'bg-amber-400 animate-pulse' : 'bg-red-400');
  const label = $derived(
    mode === 'running'
      ? $t('jobs.running', { count: String(running.length) })
      : mode === 'interrupted'
        ? $t('jobs.interrupted')
        : $t('jobs.failed'),
  );
</script>

{#if mode !== 'none'}
  <div
    class="flex items-center gap-1.5 text-[11px]"
    data-testid="jobs-activity"
    data-mode={mode}
    title={$t('jobs.tooltip')}
  >
    <span class="inline-block w-2 h-2 rounded-full {dot}" aria-hidden="true"></span>
    <span class="text-cortex-300">{label}</span>
  </div>
{/if}
