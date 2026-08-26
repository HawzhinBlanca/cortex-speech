<script lang="ts">
  import { autonomyLabelKey, autonomyValues, t, type AutonomyLevel } from './i18n';

  interface Props {
    pendingCount: number;
    isRunningJury: boolean;
    localOnly: boolean;
    autonomyLevel: AutonomyLevel;
    closePending: boolean;
    onRunJury: () => void;
    onSetAutonomy: (level: AutonomyLevel) => void;
    onClose: () => void;
  }

  let {
    pendingCount,
    isRunningJury,
    localOnly,
    autonomyLevel,
    closePending,
    onRunJury,
    onSetAutonomy,
    onClose,
  }: Props = $props();
</script>

<div class="inbox-header">
  <div class="inbox-title">
    <h2 id="review-inbox-title">{$t('reviewInbox')}</h2>
    {#if pendingCount > 0}
      <span class="inbox-badge">{pendingCount}</span>
    {/if}
  </div>

  <button
    class="btn btn-primary btn-sm"
    onclick={onRunJury}
    disabled={isRunningJury}
    aria-describedby={isRunningJury ? 'inbox-run-jury-disabled-reason' : undefined}
    title={$t('inbox.runJuryTitle')}
  >
    {#if isRunningJury}
      <span class="spinner inline-block" style="width:10px;height:10px;"></span>
      {$t('inbox.runningJury')}
    {:else}
      {$t('inbox.runJury')}
    {/if}
  </button>
  {#if isRunningJury}
    <span id="inbox-run-jury-disabled-reason" class="sr-only">
      {$t('inbox.disabled.juryRunning')}
    </span>
  {/if}
  {#if localOnly}
    <span class="local-only-badge" data-testid="jury-local-only" title={$t('inbox.localOnlyTitle')}>
      {$t('inbox.localOnly')}
    </span>
  {/if}

  <div class="autonomy-dial" role="group" aria-label={$t('inbox.autonomyLevel')}>
    {#each autonomyValues as level (level)}
      <button
        type="button"
        class="dial-btn"
        class:active={autonomyLevel === level}
        aria-pressed={autonomyLevel === level}
        onclick={() => onSetAutonomy(level)}
        title={$t(autonomyLabelKey(level))}>{$t(autonomyLabelKey(level))}</button
      >
    {/each}
  </div>

  <button
    class="close-btn"
    onclick={onClose}
    disabled={closePending}
    aria-label={$t('inbox.close')}
  >
    {$t('inbox.close')}
  </button>
</div>

<style>
  .inbox-header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    background: var(--surface-1);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .inbox-title {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1 1 auto;
    min-width: 0;
  }
  .inbox-title h2 {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--accent);
  }
  .inbox-badge {
    background: var(--accent);
    color: var(--text-on-accent);
    font-size: 0.7rem;
    font-weight: 700;
    padding: 1px 7px;
    border-radius: 999px;
  }
  .close-btn {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.75rem;
    padding: 6px 10px;
  }
  .close-btn:hover {
    color: var(--text);
  }
  .autonomy-dial {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .dial-btn {
    background: var(--surface-2);
    border: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.65rem;
    padding: 3px 8px;
    border-radius: 6px;
    cursor: pointer;
    transition: all 0.15s;
    white-space: nowrap;
  }
  .dial-btn:hover {
    border-color: var(--accent);
    color: var(--text);
  }
  .dial-btn.active {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--text-on-accent);
  }
  .local-only-badge {
    margin-inline-start: 6px;
    font-size: 0.68rem;
    opacity: 0.75;
    white-space: nowrap;
  }
  .spinner {
    display: inline-block;
    width: 18px;
    height: 18px;
    border: 2px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (max-width: 480px) {
    .inbox-header {
      align-items: center;
      gap: 8px;
      padding: 10px;
    }
    .inbox-title {
      order: 1;
      flex: 1 1 calc(100% - 2.5rem);
    }
    .close-btn {
      order: 2;
      flex: 0 0 auto;
    }
    .inbox-header > :global(.btn) {
      order: 3;
      flex: 1 1 auto;
      min-width: 0;
      white-space: normal;
    }
    .local-only-badge {
      order: 3;
      flex: 0 1 auto;
      min-width: 0;
      margin-inline-start: 0;
      white-space: normal;
    }
    .autonomy-dial {
      order: 4;
      display: grid;
      flex: 1 1 100%;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      width: 100%;
    }
    .dial-btn {
      min-width: 0;
      padding: 5px 6px;
      white-space: normal;
    }
  }
</style>
