<script lang="ts">
  import type { T2Result } from './commands';
  import { t } from './i18n';

  interface Props {
    currentId: string;
    editText: string;
    cloudOptIn: boolean;
    saving: boolean;
    mutationBlocked?: boolean;
    retranscribing: boolean;
    cloudChecking: boolean;
    cloudCheck: { id: string; result: T2Result } | null;
    onRetranscribe: () => void;
    onCloudCheck: () => void;
    onEdit: (text: string) => void;
  }

  let {
    currentId,
    editText,
    cloudOptIn,
    saving,
    mutationBlocked = false,
    retranscribing,
    cloudChecking,
    cloudCheck,
    onRetranscribe,
    onCloudCheck,
    onEdit,
  }: Props = $props();
</script>

<div class="review-secondary flex flex-wrap items-center gap-2">
  <span class="text-[11px] uppercase tracking-wider text-subtle">
    {$t('review.retranscribe')}
  </span>
  <button
    type="button"
    class="btn btn-secondary !text-xs"
    onclick={onRetranscribe}
    disabled={retranscribing || saving || mutationBlocked}
    title={$t('review.retranscribeChampionTitle')}
  >
    {retranscribing ? $t('review.retranscribing') : $t('review.retranscribeChampion')}
  </button>
  {#if cloudOptIn}
    <button
      type="button"
      class="btn btn-secondary !text-xs"
      onclick={onCloudCheck}
      disabled={cloudChecking || retranscribing || saving || mutationBlocked}
      title={$t('review.cloudCheckTitle')}
      >{cloudChecking ? $t('review.cloudChecking') : $t('review.cloudCheck')}</button
    >
  {/if}
</div>

{#if cloudCheck && cloudCheck.id === currentId}
  {@const verdict = cloudCheck.result.verdict}
  <div
    class="review-secondary space-y-2 rounded-md border border-cortex-700/40 bg-cortex-900/40 p-3"
  >
    {#if verdict}
      {#if verdict.transcript.trim() === editText.trim()}
        <p class="text-xs text-emerald-300">
          {$t('review.cloudCheckAgrees')} ({Math.round(verdict.confidence * 100)}%)
        </p>
      {:else}
        <p dir="rtl" lang="ckb" class="font-mono text-end text-sm">{verdict.transcript}</p>
        <p class="text-[11px] text-subtle">
          {verdict.reason} · {Math.round(verdict.confidence * 100)}% · {verdict.votes}×
        </p>
        <button
          type="button"
          class="btn btn-secondary !text-xs"
          onclick={() => {
            if (!mutationBlocked) onEdit(verdict.transcript);
          }}
          disabled={mutationBlocked}>{$t('review.cloudCheckUse')}</button
        >
      {/if}
    {:else}
      <p class="text-xs text-amber-300">
        {$t('review.cloudCheckEscalated')}{cloudCheck.result.error
          ? ` — ${cloudCheck.result.error}`
          : ''}
      </p>
    {/if}
  </div>
{/if}
