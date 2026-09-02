<script lang="ts">
  import type { Component } from 'svelte';

  type LoadableComponent = Component<Record<string, unknown>>;

  let {
    load,
    componentProps = {},
    loadingLabel,
    failedLabel,
    retryLabel,
    closeLabel,
    onClose,
    overlay = false,
  }: {
    load: () => Promise<unknown>;
    componentProps?: Record<string, unknown>;
    loadingLabel: string;
    failedLabel: string;
    retryLabel: string;
    closeLabel: string;
    onClose?: () => void;
    overlay?: boolean;
  } = $props();

  let Loaded = $state<LoadableComponent | null>(null);
  let loadFailed = $state(false);
  let attempt = $state(0);
  let activeAttempt = 0;

  $effect(() => {
    const currentAttempt = ++activeAttempt;
    const retryToken = attempt;
    const currentLoader = load;
    Loaded = null;
    loadFailed = false;

    void currentLoader()
      .then((module: unknown) => {
        if (currentAttempt !== activeAttempt || retryToken !== attempt) return;
        if (
          typeof module !== 'object' ||
          module === null ||
          !('default' in module) ||
          typeof module.default !== 'function'
        ) {
          throw new Error('LAZY_COMPONENT_MODULE_INVALID');
        }
        Loaded = module.default as LoadableComponent;
      })
      .catch((cause: unknown) => {
        if (currentAttempt !== activeAttempt || retryToken !== attempt) return;
        console.error('Lazy workspace load failed', cause);
        loadFailed = true;
      });

    return () => {
      activeAttempt += 1;
    };
  });

  function retry() {
    attempt += 1;
  }
</script>

{#if Loaded}
  <Loaded {...componentProps} />
{:else}
  <div
    class:fixed={overlay}
    class:inset-0={overlay}
    class:z-50={overlay}
    class:glass={overlay}
    class:flex={overlay}
    class:items-center={overlay}
    class:justify-center={overlay}
    aria-busy={!loadFailed}
  >
    <div class="card m-4 max-w-lg space-y-3 p-4 text-center">
      {#if loadFailed}
        <p role="alert" class="text-sm text-red-300">
          {failedLabel}
        </p>
        <div class="flex items-center justify-center gap-3">
          <button type="button" class="btn btn-primary" onclick={retry}>{retryLabel}</button>
          {#if onClose}
            <button type="button" class="btn btn-secondary" onclick={onClose}>{closeLabel}</button>
          {/if}
        </div>
      {:else}
        <p role="status" aria-live="polite" class="text-sm text-cortex-300">{loadingLabel}</p>
      {/if}
    </div>
  </div>
{/if}
