<script lang="ts">
  import { onMount } from 'svelte';
  import { listModelVersions, type ModelVersion } from './commands';
  import { notifications } from './stores/notificationStore';
  import { isTauriRuntime } from './runtime';

  // Read-only provenance view of the model registry: what is registered, each
  // model's license + checkpoint checksum, and which version is the champion.
  let models = $state<ModelVersion[]>([]);
  let loading = $state(true);
  const tauriAvailable = isTauriRuntime();

  onMount(async () => {
    if (!tauriAvailable) {
      loading = false;
      return;
    }
    try {
      models = await listModelVersions();
    } catch (e: unknown) {
      notifications.error('Failed to load the model registry', { detail: String(e) });
    } finally {
      loading = false;
    }
  });

  function shortSha(sha: string): string {
    return sha ? sha.slice(0, 12) : '—';
  }
</script>

<section class="space-y-2" data-testid="model-registry">
  <h3 class="text-sm font-medium text-cortex-100">Registered models</h3>
  {#if loading}
    <p class="text-xs text-muted">Loading registry…</p>
  {:else if models.length === 0}
    <p class="text-xs text-muted" data-testid="model-registry-empty">
      No models are registered yet. An imported fine-tuned checkpoint appears here with its license
      and checkpoint checksum so its provenance stays auditable.
    </p>
  {:else}
    <ul class="space-y-1" data-testid="model-registry-list">
      {#each models as m (m.id)}
        <li
          class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-2 text-xs"
          data-testid="model-registry-row"
        >
          <div class="flex flex-wrap items-center gap-2">
            <span class="font-medium text-cortex-100">{m.id}</span>
            <span class="text-muted">·</span>
            <span class="text-muted">{m.family}</span>
            {#if m.status === 'champion'}
              <span class="rounded bg-emerald-700/50 px-1.5 py-0.5 text-emerald-100">champion</span>
            {:else}
              <span class="rounded bg-cortex-700/50 px-1.5 py-0.5 text-cortex-200">{m.status}</span>
            {/if}
          </div>
          <div class="mt-1 text-muted">
            license {m.license} · sha {shortSha(m.checkpoint_sha256)}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>
