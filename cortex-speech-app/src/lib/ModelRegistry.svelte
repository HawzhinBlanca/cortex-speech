<script lang="ts">
  import { onMount } from 'svelte';
  import { listModelVersions, importModelCheckpoint, type ModelVersion } from './commands';
  import { notifications } from './stores/notificationStore';
  import { isTauriRuntime } from './runtime';

  // Provenance view of the model registry: what is registered, each model's license + checkpoint
  // checksum, which version is the champion, plus an import form to register a new checkpoint.
  let models = $state<ModelVersion[]>([]);
  let loading = $state(true);
  const tauriAvailable = isTauriRuntime();

  // Import-checkpoint form state.
  let showImport = $state(false);
  let importing = $state(false);
  let form = $state({
    id: '',
    family: '',
    source: '',
    license: '',
    modelCardName: '',
    checkpointPath: '',
  });

  async function load() {
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
  }

  onMount(load);

  function shortSha(sha: string): string {
    return sha ? sha.slice(0, 12) : '—';
  }

  async function pickCheckpoint() {
    const { open } = await import('@tauri-apps/plugin-dialog');
    const picked = await open({ multiple: false, title: 'Select a model checkpoint' });
    if (typeof picked === 'string') form.checkpointPath = picked;
  }

  const canSubmit = $derived(
    form.id.trim() !== '' &&
      form.family.trim() !== '' &&
      form.source.trim() !== '' &&
      form.license.trim() !== '' &&
      form.checkpointPath.trim() !== '' &&
      !importing,
  );

  async function submitImport() {
    if (!canSubmit) return;
    importing = true;
    try {
      const newId = await importModelCheckpoint({
        id: form.id.trim(),
        family: form.family.trim(),
        checkpointPath: form.checkpointPath.trim(),
        source: form.source.trim(),
        license: form.license.trim(),
        modelCardName: form.modelCardName.trim() || null,
      });
      notifications.success(`Imported checkpoint "${newId}" as a candidate.`);
      form = { id: '', family: '', source: '', license: '', modelCardName: '', checkpointPath: '' };
      showImport = false;
      await load();
    } catch (e: unknown) {
      notifications.error('Failed to import checkpoint', { detail: String(e) });
    } finally {
      importing = false;
    }
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

  {#if tauriAvailable}
    <div class="pt-2 border-t border-cortex-800/40">
      <button
        class="btn-ghost text-xs"
        onclick={() => (showImport = !showImport)}
        data-testid="model-import-toggle"
      >
        {showImport ? '− Cancel import' : '+ Import checkpoint'}
      </button>
      {#if showImport}
        <div class="mt-2 space-y-2" data-testid="model-import-form">
          <div class="grid grid-cols-2 gap-2">
            <input
              class="input text-xs"
              placeholder="id (e.g. mms-ckb-v2)"
              aria-label="Model id"
              bind:value={form.id}
            />
            <input
              class="input text-xs"
              placeholder="family (e.g. mms-ckb)"
              aria-label="Model family"
              bind:value={form.family}
            />
            <input
              class="input text-xs"
              placeholder="source (e.g. fine-tune)"
              aria-label="Model source"
              bind:value={form.source}
            />
            <input
              class="input text-xs"
              placeholder="license (e.g. CC-BY-NC-4.0)"
              aria-label="Model license"
              bind:value={form.license}
            />
          </div>
          <input
            class="input text-xs w-full"
            placeholder="model card name (optional)"
            aria-label="Model card name (optional)"
            bind:value={form.modelCardName}
          />
          <div class="flex items-center gap-2">
            <button
              class="btn btn-secondary !text-xs"
              onclick={pickCheckpoint}
              data-testid="model-import-pick"
            >
              Choose file…
            </button>
            <span class="text-[10px] text-muted truncate flex-1" title={form.checkpointPath}>
              {form.checkpointPath || 'No checkpoint selected'}
            </span>
          </div>
          <button
            class="btn btn-primary !text-xs"
            onclick={submitImport}
            disabled={!canSubmit}
            data-testid="model-import-submit"
          >
            {importing ? 'Importing…' : 'Import as candidate'}
          </button>
        </div>
      {/if}
    </div>
  {/if}
</section>
