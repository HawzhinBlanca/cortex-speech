<script lang="ts">
  import type { Snippet } from 'svelte';
  import { parseActionableError, type ActionableError } from './errors';
  import { t } from './i18n';

  let {
    children,
    fallback,
  }: {
    children: Snippet;
    fallback?: Snippet<[{ error: ActionableError; retry: () => void }]>;
  } = $props();

  let hasError = $state(false);
  let actionableError = $state<ActionableError>({ message: '' });

  // Scoped, not global (2026-08-17). This used to listen for window 'error' events, which every
  // mounted instance received — so ONE uncaught error blanked all ten boundaries in App.svelte at
  // once, nine of them wrapping panels that were working fine. `<svelte:boundary>` catches errors
  // thrown while rendering or in effects BELOW THIS POINT and nowhere else, so a failure is shown
  // exactly where it happened. Uncaught async errors, which belong to no subtree, are surfaced as a
  // toast by installGlobalErrorTrap instead of by blanking a panel that did not fail.
  function fail(cause: unknown) {
    actionableError = parseActionableError(cause);
    hasError = true;
  }

  function retry() {
    hasError = false;
    actionableError = { message: '' };
  }
</script>

{#if hasError}
  {#if fallback}
    {@render fallback({ error: actionableError, retry })}
  {:else}
    <div class="p-4 bg-red-900/20 border border-red-500/30 rounded-lg space-y-2">
      <p class="text-sm text-red-300">{actionableError.message}</p>
      {#if actionableError.detail && actionableError.detail !== actionableError.message}
        <bdi class="block text-xs text-red-400/80 font-mono break-words" dir="ltr"
          >{actionableError.detail}</bdi
        >
      {/if}
      <div class="flex items-center gap-3">
        <button class="text-xs text-red-400 underline" onclick={retry}>{$t('retry')}</button>
        {#if actionableError.action}
          <button
            class="text-xs text-cortex-300 underline"
            onclick={actionableError.action.handler}
          >
            {actionableError.action.label}
          </button>
        {/if}
      </div>
    </div>
  {/if}
{:else}
  <svelte:boundary onerror={fail}>{@render children()}</svelte:boundary>
{/if}
