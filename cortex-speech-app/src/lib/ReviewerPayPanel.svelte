<script lang="ts">
  // The owner's money view: exact per-reviewer balances and the canon-mandated settlement record.
  // Until 2026-08-30 a real cash payout could not be recorded anywhere — the settlements table had
  // zero production writers — so the phone showed the full balance forever and a dispute had no
  // ledger anchor. Amounts arrive as micro-IQD DECIMAL STRINGS and are formatted with BigInt,
  // never parsed into a float.
  import { t } from './i18n';
  import { notifications } from './stores/notificationStore';
  import * as api from './commands';
  import type { ReviewerCompensationOverviewV1 } from './commands';

  let { tauriAvailable = true }: { tauriAvailable?: boolean } = $props();

  let rows = $state<ReviewerCompensationOverviewV1[]>([]);
  let loaded = $state(false);
  let busyReviewer = $state<string | null>(null);
  let references = $state<Record<string, string>>({});

  function fmtIqd(micro: string): string {
    try {
      const amount = BigInt(micro);
      const whole = (amount < 0n ? -amount : amount) / 1000000n;
      const grouped = whole.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
      return `${amount < 0n ? '-' : ''}${grouped}`;
    } catch {
      return '—';
    }
  }

  async function load() {
    if (!tauriAvailable) return;
    try {
      rows = await api.getReviewCompensationOverview();
    } catch (error) {
      notifications.error($t('settings.payLoadFailed'), { cause: error });
    } finally {
      loaded = true;
    }
  }

  async function recordPayout(row: ReviewerCompensationOverviewV1) {
    const reference = (references[row.reviewer] ?? '').trim();
    if (!reference || busyReviewer) return;
    busyReviewer = row.reviewer;
    try {
      const settlement = await api.recordReviewCompensationSettlement(
        row.reviewer,
        // The exact boundary this screen showed: credits landing after this read stay outstanding
        // instead of being silently swept into a payout that never covered them.
        row.maxLedgerId,
        reference,
      );
      notifications.success(
        $t('settings.payRecorded', {
          amount: fmtIqd(settlement.allocatedMicroIqd),
          reviewer: settlement.reviewer,
        }),
      );
      references[row.reviewer] = '';
      await load();
    } catch (error) {
      notifications.error($t('settings.payFailed'), { cause: error });
    } finally {
      busyReviewer = null;
    }
  }

  void load();
</script>

<div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 space-y-2">
  <span class="text-sm text-default">{$t('settings.payTitle')}</span>
  {#if loaded && rows.length === 0}
    <span class="text-[10px] text-subtle block">{$t('settings.payNone')}</span>
  {/if}
  {#each rows as row (row.reviewer)}
    <div class="space-y-1 border-t border-cortex-700/30 pt-2 first:border-t-0 first:pt-0">
      <div class="flex items-center justify-between gap-2">
        <bdi class="text-xs text-default font-semibold" dir="auto">{row.reviewer}</bdi>
        <span class="text-xs text-default font-mono" dir="ltr">
          {$t('settings.payOutstanding')}: {fmtIqd(row.outstandingMicroIqd)} IQD
        </span>
      </div>
      <span class="text-[10px] text-subtle block" dir="auto">
        {$t('settings.payEarned')}: {fmtIqd(row.earnedMicroIqd)} · {$t('settings.paySettled')}: {fmtIqd(
          row.settledMicroIqd,
        )}
      </span>
      {#if BigInt(row.outstandingMicroIqd) > 0n}
        <div class="flex items-center gap-2">
          <input
            class="input flex-1 !text-xs"
            placeholder={$t('settings.payReference')}
            bind:value={references[row.reviewer]}
            dir="ltr"
          />
          <button
            type="button"
            class="btn-secondary text-xs px-3 whitespace-nowrap"
            disabled={busyReviewer !== null || !(references[row.reviewer] ?? '').trim()}
            onclick={() => void recordPayout(row)}
          >
            {$t('settings.payRecord')}
          </button>
        </div>
      {/if}
    </div>
  {/each}
</div>
