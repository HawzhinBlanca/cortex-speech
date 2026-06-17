<script lang="ts">
import type { Snippet } from 'svelte';
import { parseActionableError, type ActionableError } from './errors';

let { children, fallback }: {
  children: Snippet;
  fallback?: Snippet<[{ error: ActionableError; retry: () => void }]>;
} = $props();

let hasError = $state(false);
let actionableError = $state<ActionableError>({ message: 'Unknown error' });

function handleError(e: ErrorEvent) {
  const message = e.message || '';
  if (message.includes('ResizeObserver') || message.includes('Resize observer')) {
    return;
  }
  e.preventDefault();
  actionableError = parseActionableError(e.error ?? e.message);
  hasError = true;
}

function retry() {
  hasError = false;
  actionableError = { message: '' };
}

$effect(() => {
  if (typeof window !== 'undefined') {
    window.addEventListener('error', handleError, true);
    return () => window.removeEventListener('error', handleError, true);
  }
});
</script>

{#if hasError}
  {#if fallback}
    {@render fallback({ error: actionableError, retry })}
  {:else}
    <div class="p-4 bg-red-900/20 border border-red-500/30 rounded-lg space-y-2">
      <p class="text-sm text-red-300">{actionableError.message}</p>
      {#if actionableError.detail && actionableError.detail !== actionableError.message}
        <p class="text-xs text-red-400/80 font-mono break-words">{actionableError.detail}</p>
      {/if}
      <div class="flex items-center gap-3">
        <button class="text-xs text-red-400 underline" onclick={retry}>Retry</button>
        {#if actionableError.action}
          <button class="text-xs text-cortex-300 underline" onclick={actionableError.action.handler}>
            {actionableError.action.label}
          </button>
        {/if}
      </div>
    </div>
  {/if}
{:else}
  {@render children()}
{/if}
