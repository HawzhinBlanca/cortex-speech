<script lang="ts">
  import { onMount } from 'svelte';
  import { chooseFile } from './fileDialogs';
  import { listModelVersions, importModelCheckpoint, type ModelVersion } from './commands';
  import { notifications } from './stores/notificationStore';
  import { isTauriRuntime } from './runtime';
  import { t } from './i18n';

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
      notifications.error($t('modelRegistry.loadFailed'), { cause: e });
    } finally {
      loading = false;
    }
  }

  onMount(load);

  function shortSha(sha: string): string {
    return sha ? sha.slice(0, 12) : '—';
  }

  async function pickCheckpoint() {
    const picked = await chooseFile({ title: $t('modelRegistry.selectCheckpoint') });
    if (picked) form.checkpointPath = picked;
  }

  const canSubmit = $derived(
    form.id.trim() !== '' &&
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
        checkpointPath: form.checkpointPath.trim(),
        source: form.source.trim(),
        license: form.license.trim(),
        modelCardName: form.modelCardName.trim() || null,
      });
      notifications.success($t('modelRegistry.importedCandidate', { id: newId }));
      form = { id: '', source: '', license: '', modelCardName: '', checkpointPath: '' };
      showImport = false;
      await load();
    } catch (e: unknown) {
      notifications.error($t('modelRegistry.importFailed'), { cause: e });
    } finally {
      importing = false;
    }
  }
</script>

<section class="space-y-2" data-testid="model-registry">
  <h3 class="text-sm font-medium text-cortex-100">{$t('modelRegistry.title')}</h3>
  {#if loading}
    <p class="text-xs text-muted">{$t('modelRegistry.loading')}</p>
  {:else if models.length === 0}
    <p class="text-xs text-muted" data-testid="model-registry-empty">
      {$t('modelRegistry.empty')}
    </p>
  {:else}
    <ul class="space-y-1" data-testid="model-registry-list">
      {#each models as m (m.id)}
        <li
          class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-2 text-xs"
          data-testid="model-registry-row"
        >
          <div class="flex flex-wrap items-center gap-2">
            <bdi dir="ltr" class="font-medium text-cortex-100">{m.id}</bdi>
            <span class="text-muted">·</span>
            <bdi dir="ltr" class="text-muted">{m.family}</bdi>
            {#if m.status === 'champion'}
              <span class="rounded bg-emerald-700/50 px-1.5 py-0.5 text-emerald-100">
                {$t('modelRegistry.champion')}
              </span>
            {:else}
              <bdi dir="ltr" class="rounded bg-cortex-700/50 px-1.5 py-0.5 text-cortex-200">
                {m.status}
              </bdi>
            {/if}
          </div>
          <div class="mt-1 text-muted" dir="auto">
            {$t('modelRegistry.licenseLabel')}
            <bdi dir="ltr">{m.license}</bdi>
            · {$t('modelRegistry.shaLabel')}
            <bdi dir="ltr">{shortSha(m.checkpointSha256)}</bdi>
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
        {showImport ? $t('modelRegistry.cancelImport') : $t('modelRegistry.importCheckpoint')}
      </button>
      {#if showImport}
        <div class="mt-2 space-y-2" data-testid="model-import-form">
          <div class="grid grid-cols-2 gap-2">
            <input
              class="input text-xs"
              placeholder={$t('modelRegistry.idPlaceholder')}
              aria-label={$t('modelRegistry.idLabel')}
              bind:value={form.id}
            />
            <input
              class="input text-xs"
              placeholder={$t('modelRegistry.sourcePlaceholder')}
              aria-label={$t('modelRegistry.sourceLabel')}
              bind:value={form.source}
            />
            <input
              class="input text-xs"
              placeholder={$t('modelRegistry.licensePlaceholder')}
              aria-label={$t('modelRegistry.licenseFieldLabel')}
              bind:value={form.license}
            />
          </div>
          <input
            class="input text-xs w-full"
            placeholder={$t('modelRegistry.modelCardPlaceholder')}
            aria-label={$t('modelRegistry.modelCardLabel')}
            bind:value={form.modelCardName}
          />
          <div class="flex items-center gap-2">
            <button
              class="btn btn-secondary !text-xs"
              onclick={pickCheckpoint}
              data-testid="model-import-pick"
            >
              {$t('modelRegistry.chooseFile')}
            </button>
            <span class="text-[10px] text-muted truncate flex-1">
              {form.checkpointPath
                ? $t('modelRegistry.checkpointSelected')
                : $t('modelRegistry.noCheckpointSelected')}
            </span>
          </div>
          <button
            class="btn btn-primary !text-xs"
            onclick={submitImport}
            disabled={!canSubmit}
            data-testid="model-import-submit"
          >
            {importing ? $t('modelRegistry.importing') : $t('modelRegistry.importAsCandidate')}
          </button>
        </div>
      {/if}
    </div>
  {/if}
</section>
