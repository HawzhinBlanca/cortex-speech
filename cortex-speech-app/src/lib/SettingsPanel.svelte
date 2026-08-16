<script lang="ts">
  import { settings, showSettings, settingsTab, type AppSettings } from './stores/settingsStore';
  import { focusTrap } from './actions/focusTrap';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { segments } from './stores/segmentStore';
  import { isVerifiedGood } from './segmentQuality';
  import { isProcessing, statusMessage, batchProgress } from './stores/uiStore';
  import { startOperation, endOperation } from './invoke';
  import ModelDownload from './ModelDownload.svelte';
  import ModelRegistry from './ModelRegistry.svelte';
  import DiagnosticsPanel from './DiagnosticsPanel.svelte';
  import { t } from './i18n';
  import { get } from 'svelte/store';
  import { onDestroy, onMount } from 'svelte';
  import { isTauriRuntime } from './runtime';

  let localSettings: AppSettings = $state({ ...$settings });
  let activeTab = $state<
    'general' | 'asr' | 'audio' | 'export' | 'models' | 'ai' | 'jury' | 'diagnostics'
  >($settingsTab);
  let saving = $state(false);
  let exportingAudio = $state(false);
  // Seeded ONCE from the store here (like localSettings above). It must NOT be re-mirrored from
  // $settings on every store change — see the removed $effect: saveQuietly()/toggles write the store
  // mid-edit, and re-seeding would revert whatever the user is currently typing (silent data loss).
  let sourceReferenceModelsInput = $state(($settings.sourceReferenceModels ?? []).join(', '));
  // Lowercase provider names whose key is present in secrets.env (e.g. "elevenlabs").
  // Read-only signal so cloud opt-ins can warn when their key is missing; never holds key values.
  let configuredProviders = $state<string[]>([]);
  const tauriAvailable = isTauriRuntime();
  // Set true only by the Cancel button so onDestroy discards unsaved edits instead of persisting
  // them. (Theme/sliders have no per-field auto-save and rely on the close-to-save onDestroy path,
  // so we must NOT gate it globally — only suppress it for an explicit Cancel.)
  let cancelled = $state(false);

  $effect(() => {
    activeTab = $settingsTab;
  });

  onMount(async () => {
    // Surface which cloud keys are present so an opt-in can warn before a use-time failure.
    // Best-effort + read-only: a failure must never break the settings panel.
    if (!tauriAvailable) return;
    try {
      configuredProviders = await api.getConfiguredProviders();
    } catch (e) {
      console.error('Failed to load configured cloud providers:', e);
    }
  });

  // Couch Review (LAN phone review server): session state + start/stop. Each reviewer's URL (with its
  // own one-session token) is only ever displayed here, for the owner to hand to that person.
  let couchStatus = $state<import('./commands').CouchStatus | null>(null);
  let couchBusy = $state(false);
  // Comma-separated reviewer names. Left blank, the server starts a single-reviewer session under its
  // default name — the previous behaviour exactly.
  let couchNames = $state('');
  async function toggleCouch() {
    if (couchBusy || !tauriAvailable) return;
    couchBusy = true;
    try {
      couchStatus = couchStatus?.running
        ? await api.stopCouchReview()
        : await api.startCouchReview(
            couchNames
              .split(',')
              .map((n) => n.trim())
              .filter(Boolean),
          );
    } catch (e) {
      notifications.error($t('settings.couchFailed'), { detail: String(e) });
    } finally {
      couchBusy = false;
    }
  }
  // Spot-check scores: how each remote reviewer did on clips whose answer was already known. This is
  // the only signal in the app about whether a REVIEWER was honest, as opposed to whether the machine
  // was — so it is shown wherever their links are handed out.
  let spotChecks = $state<import('./commands').SpotCheckScore[]>([]);
  // Agreement sample: exported on demand, because writing a file every time Settings opens would be
  // a surprising side effect of merely looking.
  // Per-reviewer throughput from the append-only audit trail. Partitioned per person — unlike the
  // global stats.rs figure, which is only meaningful when exactly one human is reviewing.
  let throughput = $state<import('./commands').ReviewerThroughput[]>([]);
  async function revokeReviewer(name: string) {
    if (couchBusy || !tauriAvailable) return;
    couchBusy = true;
    try {
      couchStatus = await api.revokeCouchReviewer(name);
    } catch (e) {
      notifications.error($t('settings.couchRevoke'), { detail: String(e) });
    } finally {
      couchBusy = false;
    }
  }
  let agreement = $state<import('./commands').AgreementExport | null>(null);
  let agreementBusy = $state(false);
  async function exportAgreement() {
    if (agreementBusy || !tauriAvailable) return;
    agreementBusy = true;
    try {
      agreement = await api.exportAgreementSample();
      if (!agreement) notifications.info($t('settings.couchAgreementNone'));
    } catch (e) {
      notifications.error($t('settings.couchAgreement'), { detail: String(e) });
    } finally {
      agreementBusy = false;
    }
  }
  onMount(async () => {
    if (!tauriAvailable) return;
    try {
      couchStatus = await api.couchReviewStatus();
    } catch (e) {
      console.error('couch status load failed:', e);
    }
    try {
      // `?? []` is load-bearing, not defensive noise: a backend (or a test double) that answers with
      // null made `spotChecks.length` throw during render and took the WHOLE settings dialog down —
      // a reviewer-quality panel must never be able to break the settings the app depends on.
      spotChecks = (await api.spotCheckReport()) ?? [];
    } catch (e) {
      console.error('spot-check report load failed:', e);
      spotChecks = [];
    }
    try {
      throughput = (await api.reviewerThroughput()) ?? [];
    } catch (e) {
      console.error('reviewer throughput load failed:', e);
      throughput = [];
    }
  });

  // OpenRouter key entry. The pasted value goes straight to the backend (secrets.env) and is cleared
  // from this input on success — it is never kept in the store, settings.json, or logs, and the UI
  // only ever reflects set/unset (configuredProviders holds provider NAMES, never values).
  let openrouterKeyInput = $state('');
  let savingOpenrouterKey = $state(false);
  async function saveOpenrouterKey() {
    if (!tauriAvailable || savingOpenrouterKey) return;
    savingOpenrouterKey = true;
    try {
      configuredProviders = await api.setApiKey('openrouter', openrouterKeyInput.trim());
      openrouterKeyInput = '';
      notifications.success(
        configuredProviders.includes('openrouter')
          ? $t('settings.apiKeySavedToast', { provider: 'OpenRouter' })
          : $t('settings.apiKeyClearedToast', { provider: 'OpenRouter' }),
      );
    } catch (e) {
      notifications.error($t('settings.apiKeySaveFailedToast', { provider: 'OpenRouter' }), {
        detail: String(e),
      });
    } finally {
      savingOpenrouterKey = false;
    }
  }

  // GEMINI KEY — same route as OpenRouter, and it did not exist before 2026-08-10.
  //
  // The Gemini field used to bind to localSettings.llmApiKey and persist through saveQuietly, i.e. into
  // settings.json. AppSettings::load then SCRUBS that field (clears it and rewrites the file) so a
  // plaintext key never survives on disk — so the key was written and then deleted, while
  // llmApiKeyConfigured stayed true and the UI reported a key that no longer existed. Measured: an
  // import failed 3/3 with "Gemini API key is required" while the field looked configured.
  //
  // set_api_key already accepted 'gemini'; nothing in the UI ever called it. This does.
  let geminiKeyInput = $state('');
  let savingGeminiKey = $state(false);
  async function saveGeminiKey() {
    if (!tauriAvailable || savingGeminiKey) return;
    savingGeminiKey = true;
    try {
      configuredProviders = await api.setApiKey('gemini', geminiKeyInput.trim());
      geminiKeyInput = '';
      notifications.success(
        configuredProviders.includes('gemini')
          ? $t('settings.apiKeySavedToast', { provider: 'Gemini' })
          : $t('settings.apiKeyClearedToast', { provider: 'Gemini' }),
      );
    } catch (e) {
      notifications.error($t('settings.apiKeySaveFailedToast', { provider: 'Gemini' }), {
        detail: String(e),
      });
    } finally {
      savingGeminiKey = false;
    }
  }

  // A key typed into either field is DISCARDED by every exit path unless that field's OWN "Save key"
  // button was pressed — and the panel's bottom-right Save is the bigger, bluer, more obvious button.
  // Worse, the bottom Save routes through update_settings, whose load() SCRUBS llm_api_key, so the
  // trap is silent: no error, no toast, and the key is simply gone.
  //
  // Measured 2026-08-10: the owner pasted a Gemini key, pressed Save, and it never reached
  // secrets.env — GEMINI_API_KEY stayed empty while the panel looked like it had accepted the key.
  //
  // Fixed at the convergence point rather than by relabelling one button: EVERY path that leaves the
  // panel flushes pending key inputs through the encrypted store first, so whichever button the user
  // reaches for does the right thing. Awaited in save() (which then closes the panel); fire-and-forget
  // in onDestroy, where the component is already going away and there is nothing left to await for.
  async function flushPendingKeys() {
    if (openrouterKeyInput.trim()) await saveOpenrouterKey();
    if (geminiKeyInput.trim()) await saveGeminiKey();
  }

  onDestroy(() => {
    // Cancel must discard: don't persist unsaved edits when the user explicitly cancelled.
    if (cancelled) return;
    void flushPendingKeys();
    applySourceReferenceModelsInput();
    // Close-to-save for theme/sliders (they have no per-field auto-save). Route through saveQuietly so
    // this path gets the SAME NaN coercion + rollback-on-failure + error toast as every other persist.
    // The old bare fire-and-forget updateSettings here skipped coerceSettingsForRuntime: clearing a
    // type=number field (min/max segment sec, maxSpeakers, jurySelfConsistencyN) to NaN then closing via
    // ✕/Escape sent NaN->null and failed the WHOLE backend write, silently discarding every edit the user
    // did make (theme included) and leaving NaN in the store.
    if (JSON.stringify(localSettings) !== JSON.stringify(get(settings))) {
      void saveQuietly();
    }
  });

  function coerceSettingsForRuntime() {
    // A <input type="number"> binds NaN when the user clears the field to retype it. Shipping NaN to
    // updateSettings either fails backend deserialization (u32 fields — the setting silently doesn't
    // save) or, for a float threshold, defeats a gate. Revert any non-finite numeric field to its
    // LAST-PERSISTED value (no invented bounds that could reject a legitimate entry; the backend still
    // enforces the [0,1] threshold ranges).
    const prev = get(settings);
    const finite = (v: number, fallback: number): number => (Number.isFinite(v) ? v : fallback);
    localSettings = {
      ...localSettings,
      // A stale optional-model toggle must not travel beside the selected champion. Rust already
      // gives WSL7B precedence; normalize the persisted UI state too so it never claims otherwise.
      useFinetuned: localSettings.asrModel === 'wsl-7b' ? false : localSettings.useFinetuned,
      vadThreshold: finite(localSettings.vadThreshold, prev.vadThreshold),
      minSegmentSec: finite(localSettings.minSegmentSec, prev.minSegmentSec),
      maxSegmentSec: finite(localSettings.maxSegmentSec, prev.maxSegmentSec),
      maxSpeakers: finite(localSettings.maxSpeakers, prev.maxSpeakers),
      maxWerThreshold: finite(localSettings.maxWerThreshold, prev.maxWerThreshold),
      maxCerThreshold: finite(localSettings.maxCerThreshold, prev.maxCerThreshold),
      jurySelfConsistencyN: finite(localSettings.jurySelfConsistencyN, prev.jurySelfConsistencyN),
    };
  }

  async function saveQuietly() {
    coerceSettingsForRuntime();
    if (!tauriAvailable) {
      settings.set(localSettings);
      return;
    }
    const prev = get(settings); // authoritative last-persisted state, kept for rollback on failure
    settings.set(localSettings);
    try {
      await api.updateSettings(localSettings);
    } catch (e) {
      console.error('Auto-save settings failed:', e);
      // Roll BACK so the UI can't show a consent toggle (or any setting) as applied when the backend
      // REJECTED the write. A silent consent mismatch — the user believes cloud STT/LLM is on (or off)
      // while the backend disagrees — is safety-critical for an offline-first app. Revert local + store
      // to the last-persisted state and surface the failure instead of only logging it.
      localSettings = { ...prev };
      sourceReferenceModelsInput = (prev.sourceReferenceModels ?? []).join(', ');
      settings.set(prev);
      notifications.error($t('settingsSaveFailed'), { detail: String(e) });
    }
  }

  function parseSourceReferenceModels(value: string): string[] {
    const seen = new Set<string>();
    const models = value
      .split(',')
      .map((model) => model.trim())
      .filter((model) => model.length > 0)
      .filter((model) => {
        if (seen.has(model)) return false;
        seen.add(model);
        return true;
      });
    return models.length > 0 ? models : ['gemini-2.5-pro'];
  }

  function saveSourceReferenceModels() {
    applySourceReferenceModelsInput();
    saveQuietly();
  }

  function applySourceReferenceModelsInput() {
    localSettings.sourceReferenceModels = parseSourceReferenceModels(sourceReferenceModelsInput);
    sourceReferenceModelsInput = localSettings.sourceReferenceModels.join(', ');
  }

  const tabs = [
    { id: 'general', labelKey: 'general' },
    { id: 'asr', labelKey: 'asr' },
    { id: 'audio', labelKey: 'audio' },
    { id: 'export', labelKey: 'export' },
    { id: 'models', labelKey: 'models' },
    { id: 'ai', labelKey: 'settings.aiTab' },
    { id: 'jury', labelKey: 'settings.juryTab' },
    { id: 'diagnostics', labelKey: 'diagnostics' },
  ] as const;

  async function save() {
    saving = true;
    try {
      applySourceReferenceModelsInput();
      coerceSettingsForRuntime();
      settings.set(localSettings);
      if (!tauriAvailable) {
        notifications.info($t('settingsPreviewOnly'));
        showSettings.set(false);
        return;
      }
      // BEFORE update_settings: its load() scrubs llm_api_key, so a key still sitting in an input when
      // the user presses Save would otherwise be dropped without a word. See flushPendingKeys.
      await flushPendingKeys();
      await api.updateSettings(localSettings);
      notifications.success($t('settingsSaved'));
      showSettings.set(false);
    } catch (e) {
      notifications.error($t('settingsSaveFailed'), { detail: String(e) });
    } finally {
      saving = false;
    }
  }

  async function handleExportAudioFromSettings() {
    if (!tauriAvailable) {
      notifications.info($t('desktopRuntimeRequired'));
      return;
    }
    // Export the audio of human-confirmed GOOD clips only — never a human-rejected ("mark bad") clip,
    // which carries verified=true solely to leave the review queue.
    const verifiedIds = get(segments)
      .filter((s) => isVerifiedGood(s))
      .map((s) => s.id);
    if (verifiedIds.length === 0) {
      notifications.warning($t('exportAudio.noVerified'));
      return;
    }
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({ directory: true, multiple: false });
      if (!dir || typeof dir !== 'string') return;

      exportingAudio = true;
      startOperation('export-audio');
      isProcessing.set(true);
      statusMessage.set($t('exportAudio.progress'));
      const result = await api.exportAudio(verifiedIds, {
        output_dir: dir,
        format: api.AudioExportFormat.Wav,
        sample_rate: 16000,
        include_metadata: true,
      });

      if (result.failed > 0) {
        notifications.warning(
          $t('exportAudio.partial', {
            succeeded: String(result.succeeded),
            failed: String(result.failed),
          }),
          { detail: result.output_dir },
        );
      } else {
        notifications.success($t('exportAudio.success', { count: String(result.succeeded) }), {
          detail: result.output_dir,
        });
      }
    } catch (e) {
      notifications.error($t('exportAudio.failed'), { detail: String(e) });
    } finally {
      exportingAudio = false;
      isProcessing.set(false);
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set($t('ready'));
      endOperation('export-audio');
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') showSettings.set(false);
  }
</script>

<div
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
  data-testid="settings-panel"
  role="dialog"
  aria-modal="true"
  tabindex="-1"
  use:focusTrap
  onkeydown={handleKeydown}
>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    role="presentation"
    class="card p-0 max-w-2xl w-full mx-4 max-h-[85vh] flex flex-col shadow-2xl"
    onclick={(e) => e.stopPropagation()}
  >
    <div class="flex items-center justify-between px-6 py-4 border-b border-cortex-800/50">
      <h2 class="text-lg font-semibold text-default">{$t('settings')}</h2>
      <button
        data-testid="settings-close-btn"
        class="text-muted hover:text-default transition-colors"
        onclick={() => showSettings.set(false)}
        aria-label={$t('close')}>✕</button
      >
    </div>

    <div class="flex gap-0 flex-1 min-h-0">
      <nav class="w-40 shrink-0 p-2 border-r border-cortex-800/50 space-y-1">
        {#each tabs as tab}
          <button
            class="w-full text-start px-3 py-2 rounded-lg text-sm transition-colors {activeTab ===
            tab.id
              ? 'bg-cortex-700 text-cortex-100'
              : 'text-cortex-300 hover:text-cortex-100 hover:bg-cortex-800/50'}"
            disabled={!tauriAvailable && tab.id === 'models'}
            title={!tauriAvailable && tab.id === 'models' ? $t('desktopRuntimeRequired') : ''}
            onclick={() => (activeTab = tab.id)}>{$t(tab.labelKey)}</button
          >
        {/each}
      </nav>

      <div class="flex-1 p-6 overflow-y-auto space-y-5">
        {#if !tauriAvailable}
          <div
            class="rounded-md border border-amber-500/30 bg-amber-950/20 p-3 text-xs text-amber-100"
          >
            {$t('settingsPreviewOnly')}
          </div>
        {/if}

        {#if activeTab === 'general'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('theme')}</span>
            <select class="input flex-1" bind:value={localSettings.theme}>
              <option value="dark">{$t('dark')}</option>
              <option value="light">{$t('light')}</option>
              <option value="system">{$t('system')}</option>
            </select>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('language')}</span>
            <select class="input flex-1" bind:value={localSettings.language}>
              <option value="ckb">{$t('kurdish')}</option>
            </select>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              bind:checked={localSettings.autoNormalize}
              class="accent-cortex-500"
            />
            <span class="text-sm text-muted">{$t('autoNormalize')}</span>
          </label>
          {#if localSettings.autoNormalize}
            <label class="flex items-center gap-3 cursor-pointer ps-6">
              <input
                type="checkbox"
                bind:checked={localSettings.verbalizeNumbers}
                class="accent-cortex-500"
              />
              <span class="text-sm text-muted">{$t('verbalizeNumbers')}</span>
            </label>
          {/if}
          <label class="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              bind:checked={localSettings.autoAlign}
              class="accent-cortex-500"
            />
            <span class="text-sm text-muted">{$t('autoAlign')}</span>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              bind:checked={localSettings.autoplaySegments}
              class="accent-cortex-500"
            />
            <span class="text-sm text-muted">{$t('settings.autoplaySegments')}</span>
          </label>

          <!-- Couch Review: LAN-only, token-gated phone review server (off by default, per-session).
               The URL carries a random session token; audio never leaves the local network. -->
          <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 space-y-2">
            <div class="flex items-center justify-between">
              <span class="text-sm text-default">📱 {$t('settings.couchTitle')}</span>
              <button
                type="button"
                class="btn-secondary text-xs px-3"
                disabled={couchBusy || !tauriAvailable}
                onclick={() => void toggleCouch()}
              >
                {couchBusy
                  ? '…'
                  : couchStatus?.running
                    ? $t('settings.couchStop')
                    : $t('settings.couchStart')}
              </button>
            </div>
            {#if couchStatus?.running && couchStatus.reviewers.length}
              {#if couchStatus.certificateFingerprint}
                <div class="rounded border border-cortex-700/40 bg-cortex-950/40 p-2 space-y-1">
                  <span class="text-[10px] text-subtle block"
                    >{$t('settings.couchTlsFingerprint')}</span
                  >
                  <code class="block break-all text-[10px] text-default select-all"
                    >{couchStatus.certificateFingerprint}</code
                  >
                  <span class="text-[10px] text-subtle block"
                    >{$t('settings.couchTlsFingerprintHint')}</span
                  >
                </div>
              {/if}
              <!-- One block per reviewer: each link carries that person's own token, and every decision
                   they make is stored under their name. Handing out the wrong link mislabels the data,
                   so the name is shown above the URL it belongs to. -->
              {#each couchStatus.reviewers as reviewer (reviewer.name)}
                <div
                  class="space-y-1 border-t border-cortex-700/30 pt-2 first:border-t-0 first:pt-0"
                >
                  <div class="flex items-center justify-between">
                    <span class="text-xs text-default font-semibold">{reviewer.name}</span>
                    {#if couchStatus.reviewers.length > 1}
                      <!-- Revoking one link leaves every other reviewer working. Their completed work,
                           scores and audit trail are untouched - a record, not a permission. -->
                      <button
                        type="button"
                        class="btn-secondary !text-[10px] px-2 py-0.5"
                        disabled={couchBusy}
                        onclick={() => void revokeReviewer(reviewer.name)}
                        >{$t('settings.couchRevoke')}</button
                      >
                    {/if}
                  </div>
                  <span class="text-[10px] text-subtle block">{$t('settings.couchWifiUrl')}</span>
                  <input
                    class="input w-full !text-xs font-mono"
                    readonly
                    value={reviewer.url}
                    onfocus={(e) => (e.target as HTMLInputElement).select()}
                  />
                  {#if reviewer.tailscaleUrl}
                    <span class="text-[10px] text-subtle block"
                      >{$t('settings.couchTailscaleUrl')}</span
                    >
                    <input
                      class="input w-full !text-xs font-mono"
                      readonly
                      value={reviewer.tailscaleUrl}
                      onfocus={(e) => (e.target as HTMLInputElement).select()}
                    />
                  {/if}
                </div>
              {/each}
              <p class="text-[10px] text-subtle">{$t('settings.couchRunningHint')}</p>
            {:else}
              <label class="block space-y-1">
                <span class="text-[10px] text-subtle">{$t('settings.couchReviewers')}</span>
                <input
                  class="input w-full !text-xs"
                  bind:value={couchNames}
                  placeholder={$t('settings.couchReviewersPlaceholder')}
                />
              </label>
              <p class="text-[10px] text-subtle">{$t('settings.couchHint')}</p>
              <p class="text-[10px] text-subtle">{$t('settings.couchReviewersHint')}</p>
            {/if}
            {#if throughput.length}
              <div class="border-t border-cortex-700/30 pt-2 space-y-1">
                <span class="text-[10px] text-subtle">{$t('settings.couchThroughput')}</span>
                {#each throughput as r (r.reviewer)}
                  <div class="flex items-center justify-between text-xs">
                    <span class="text-default">{r.reviewer}</span>
                    <span class="text-muted">
                      {r.clips}{r.medianSeconds !== null ? ` · ${r.medianSeconds.toFixed(1)}s` : ''}
                    </span>
                  </div>
                {/each}
              </div>
            {/if}
            {#if spotChecks.length}
              <!-- Spot checks: a share of every reviewer's queue is drawn from clips that already have
                   a human answer, served with the known-WRONG draft. "Noticed" is the number to read
                   first — a reviewer who listens corrects it, one who taps accept hands it back. Sorted
                   worst-first by the backend, so a reviewer who may not be listening appears at top. -->
              <div class="border-t border-cortex-700/30 pt-2 space-y-1">
                <span class="text-[10px] text-subtle">{$t('settings.couchSpotChecks')}</span>
                {#each spotChecks as s (s.reviewer)}
                  <div class="flex items-center justify-between text-xs">
                    <span class="text-default">{s.reviewer}</span>
                    <span
                      class={s.noticed < s.checks / 2
                        ? 'text-rose-300 font-semibold'
                        : 'text-muted'}
                    >
                      {s.noticed}/{s.checks} · CER {(s.meanCer * 100).toFixed(1)}%
                    </span>
                  </div>
                {/each}
                <p class="text-[10px] text-subtle">{$t('settings.couchSpotChecksHint')}</p>
                <!-- Inter-annotator agreement. Spot checks are not leased, so two reviewers already
                     answer the same clips independently — the overlap a kappa study needs exists
                     already. This exports the TSV; the number comes from the unit-tested harness
                     (scripts/agreement_kappa.py), never from a second implementation here. -->
                <button
                  type="button"
                  class="btn-secondary text-xs px-3 w-full"
                  disabled={agreementBusy || !tauriAvailable}
                  onclick={() => void exportAgreement()}
                >
                  {$t('settings.couchAgreement')}
                </button>
                {#if agreement}
                  <p class="text-[10px] text-subtle">
                    {agreement.raterA} · {agreement.raterB} — {agreement.items}
                    {#if agreement.otherReviewers.length}· +{agreement.otherReviewers.join(
                        ', ',
                      )}{/if}
                  </p>
                  <input
                    class="input w-full !text-[10px] font-mono"
                    readonly
                    value={agreement.path}
                    onfocus={(e) => (e.target as HTMLInputElement).select()}
                  />
                  <p class="text-[10px] text-subtle">{$t('settings.couchAgreementHint')}</p>
                {/if}
              </div>
            {/if}
          </div>
        {:else if activeTab === 'asr'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('asrModel')}</span>
            <select class="input flex-1" bind:value={localSettings.asrModel}>
              <option value="ctc-300m">Meta OmniASR CTC 300M</option>
              <option value="ctc-1b">Meta OmniASR CTC 1B</option>
              <option value="wsl-7b">Meta OmniASR 7B Champion + LoRA (WSL GPU)</option>
            </select>
          </label>
          {#if localSettings.asrModel !== 'wsl-7b'}
            <label class="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                bind:checked={localSettings.useFinetuned}
                class="accent-cortex-500"
                data-testid="use-finetuned-toggle"
              />
              <span class="text-sm text-muted">{$t('settings.useFinetuned')}</span>
            </label>
            <p class="-mt-1 ms-8 text-xs text-subtle">{$t('settings.useFinetunedHint')}</p>
          {/if}
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('threads')}</span>
            <input
              type="range"
              min="1"
              max="16"
              bind:value={localSettings.numThreads}
              class="flex-1 accent-cortex-500"
            />
            <span class="text-xs font-mono text-cortex-300 w-6 text-end"
              >{localSettings.numThreads}</span
            >
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              bind:checked={localSettings.enableGpu}
              class="accent-cortex-500"
            />
            <span class="text-sm text-muted">{$t('gpuAcceleration')}</span>
          </label>
          <p class="text-xs text-subtle ms-7 -mt-1">{$t('gpuAccelerationNote')}</p>
        {:else if activeTab === 'audio'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('vadThreshold')}</span>
            <input
              type="range"
              min="0"
              max="1"
              step="0.05"
              bind:value={localSettings.vadThreshold}
              class="flex-1 accent-cortex-500"
            />
            <span class="text-xs font-mono text-cortex-300 w-8 text-end"
              >{localSettings.vadThreshold}</span
            >
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('minSegment')}</span>
            <input
              type="number"
              bind:value={localSettings.minSegmentSec}
              class="input w-20"
              min="1"
              max="60"
            />
            <span class="text-xs text-cortex-400">{$t('seconds')}</span>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('maxSegment')}</span>
            <input
              type="number"
              bind:value={localSettings.maxSegmentSec}
              class="input w-20"
              min="1"
              max="300"
            />
            <span class="text-xs text-cortex-400">{$t('seconds')}</span>
          </label>
          <label
            class="flex items-start gap-3 rounded-md border border-amber-500/30 bg-amber-950/20 p-3 cursor-pointer"
          >
            <input
              type="checkbox"
              class="mt-1 accent-cortex-500"
              bind:checked={localSettings.cloudSttOptIn}
              onchange={saveQuietly}
            />
            <!-- Consent copy MUST be in the user's language (true-10 audit): voice is biometric,
                 and the opt-in text is where clarity matters most. -->
            <span class="text-xs text-amber-100">
              {$t('settings.cloudSttConsent')}
            </span>
          </label>
          {#if tauriAvailable && localSettings.cloudSttOptIn}
            {#if configuredProviders.includes('elevenlabs')}
              <p class="text-[10px] text-emerald-300 -mt-1" data-testid="elevenlabs-key-status">
                ✓ ElevenLabs key detected in secrets.env.
              </p>
            {:else}
              <p class="text-[10px] text-amber-300 -mt-1" data-testid="elevenlabs-key-status">
                ⚠ No ElevenLabs key found in secrets.env — cloud transcription will fail until you
                add one.
              </p>
            {/if}
          {/if}
          <label class="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              bind:checked={localSettings.enableDenoising}
              class="accent-cortex-500"
            />
            <span class="text-sm text-muted">AI Audio Cleanup (Denoise before ASR)</span>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              bind:checked={localSettings.enableDiarization}
              class="accent-cortex-500"
            />
            <span class="text-sm text-muted">{$t('enableDiarization')}</span>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('maxSpeakers')}</span>
            <input
              type="number"
              bind:value={localSettings.maxSpeakers}
              class="input w-20"
              min="1"
              max="32"
            />
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input
              type="checkbox"
              bind:checked={localSettings.assignSpeakerFromFilename}
              class="accent-cortex-500"
            />
            <span class="text-sm text-muted">{$t('assignSpeakerFromFilename')}</span>
          </label>
          <div class="pt-3 border-t border-cortex-800/50 space-y-3">
            <p class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">
              {$t('qualityGates')}
            </p>
            <label class="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                bind:checked={localSettings.enforceQualityGates}
                class="accent-cortex-500"
              />
              <span class="text-sm text-muted">{$t('enforceQualityGates')}</span>
            </label>
            <label class="flex items-center gap-3">
              <span class="text-sm text-muted w-32">{$t('maxWer')}</span>
              <input
                type="range"
                min="0.05"
                max="0.80"
                step="0.05"
                bind:value={localSettings.maxWerThreshold}
                class="flex-1 accent-cortex-500"
              />
              <span class="text-xs font-mono text-cortex-300 w-10 text-end"
                >{Math.round(localSettings.maxWerThreshold * 100)}%</span
              >
            </label>
            <label class="flex items-center gap-3">
              <span class="text-sm text-muted w-32">{$t('maxCer')}</span>
              <input
                type="range"
                min="0.05"
                max="0.80"
                step="0.05"
                bind:value={localSettings.maxCerThreshold}
                class="flex-1 accent-cortex-500"
              />
              <span class="text-xs font-mono text-cortex-300 w-10 text-end"
                >{Math.round(localSettings.maxCerThreshold * 100)}%</span
              >
            </label>
          </div>
        {:else if activeTab === 'export'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-muted w-32">{$t('exportFormat')}</span>
            <select class="input flex-1" bind:value={localSettings.exportFormat}>
              <option value="json">JSON (COCO-style manifest)</option>
              <option value="jsonl">JSONL (one segment per line)</option>
              <option value="csv">CSV</option>
              <option value="parquet">Parquet</option>
            </select>
          </label>
          <div class="pt-2 border-t border-cortex-800/50 space-y-2">
            <p class="text-xs text-cortex-400">{$t('exportAudio.description')}</p>
            <button
              class="btn btn-secondary !text-xs"
              onclick={handleExportAudioFromSettings}
              disabled={!tauriAvailable || exportingAudio || $isProcessing}
              title={tauriAvailable ? $t('exportAudio.label') : $t('desktopRuntimeRequired')}
            >
              {exportingAudio ? $t('exportAudio.progress') : $t('exportAudio.label')}
            </button>
          </div>
        {:else if activeTab === 'models'}
          {#if tauriAvailable}
            <ModelDownload />
            <ModelRegistry />
          {:else}
            <div
              class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 text-xs text-muted"
            >
              {$t('desktopRuntimeRequired')}
            </div>
          {/if}
        {:else if activeTab === 'ai'}
          <div class="space-y-4">
            <h3 class="text-md font-semibold text-default">
              {$t('settings.aiTitle')}
            </h3>
            <p class="text-xs text-muted">
              {$t('settings.aiDescription')}
            </p>
            <label class="flex flex-col gap-1">
              <span class="text-sm text-muted">{$t('settings.externalAsrScript')}</span>
              <input
                type="text"
                class="input w-full"
                bind:value={localSettings.externalAsrScriptPath}
                onblur={saveQuietly}
                onchange={saveQuietly}
                placeholder="/mnt/c/path/to/provider_refine.py"
              />
              <span class="text-[10px] text-subtle">{$t('settings.externalAsrScriptHint')}</span>
            </label>
            <label class="flex items-center gap-3">
              <span class="text-sm text-muted w-32">{$t('settings.llmEngine')}</span>
              <select class="input flex-1" bind:value={localSettings.llmMode}>
                <option value="None">{$t('settings.llmDisabledOption')}</option>
                <option value="Local">{$t('settings.llmLocalOption')}</option>
                <option value="Gemini">{$t('settings.llmCloudOption')}</option>
              </select>
            </label>

            {#if localSettings.llmMode === 'Local'}
              <label class="flex flex-col gap-1">
                <span class="text-sm text-muted">{$t('settings.localApiEndpoint')}</span>
                <input
                  type="text"
                  class="input w-full"
                  bind:value={localSettings.llmEndpoint}
                  onblur={saveQuietly}
                  onchange={saveQuietly}
                  placeholder="http://127.0.0.1:11434/v1/chat/completions"
                />
                <span class="text-[10px] text-subtle">{$t('settings.localEndpointHint')}</span>
              </label>
              <label class="flex flex-col gap-1 mt-2">
                <span class="text-sm text-muted">{$t('settings.modelName')}</span>
                <input
                  type="text"
                  class="input w-full"
                  bind:value={localSettings.llmModel}
                  onblur={saveQuietly}
                  onchange={saveQuietly}
                  placeholder="heretic-final:latest"
                />
                <span class="text-[10px] text-subtle">
                  {$t('settings.quickSelect')}
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300 me-2"
                    onclick={() => {
                      localSettings.llmModel = 'heretic-final:latest';
                      saveQuietly();
                    }}>heretic-final:latest</button
                  >
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300"
                    onclick={() => {
                      localSettings.llmModel = 'qwen2.5-coder:7b';
                      saveQuietly();
                    }}>qwen2.5-coder:7b</button
                  >
                </span>
              </label>
            {:else if localSettings.llmMode === 'Gemini'}
              <label
                class="flex items-start gap-3 rounded-md border border-amber-500/30 bg-amber-950/20 p-3"
              >
                <input
                  type="checkbox"
                  class="mt-1"
                  bind:checked={localSettings.cloudLlmOptIn}
                  onchange={saveQuietly}
                />
                <span class="text-xs text-amber-100">
                  {$t('settings.cloudLlmConsent')}
                </span>
              </label>
              <label class="flex flex-col gap-1">
                <span class="text-sm text-muted">
                  {$t('settings.geminiApiKey')}
                  {#if configuredProviders.includes('gemini')}
                    <span class="ms-2 text-[10px] text-emerald-400"
                      >● {$t('settings.apiKeySaved')}</span
                    >
                  {:else}
                    <span class="ms-2 text-[10px] text-amber-400"
                      >○ {$t('settings.apiKeyMissing')}</span
                    >
                  {/if}
                </span>
                <div class="flex gap-2">
                  <input
                    type="password"
                    class="input flex-1"
                    bind:value={geminiKeyInput}
                    placeholder="AIzaSy..."
                    autocomplete="off"
                    onkeydown={(e) => {
                      if (e.key === 'Enter') void saveGeminiKey();
                    }}
                  />
                  <button
                    type="button"
                    class="btn-secondary text-xs px-3"
                    disabled={savingGeminiKey}
                    onclick={() => void saveGeminiKey()}
                  >
                    {savingGeminiKey ? $t('settings.savingKey') : $t('settings.saveKey')}
                  </button>
                </div>
                <span class="text-[10px] text-subtle">{$t('settings.apiKeyStorageHint')}</span>
              </label>
              <label class="flex flex-col gap-1 mt-2">
                <span class="text-sm text-muted">{$t('settings.geminiModel')}</span>
                <input
                  type="text"
                  class="input w-full"
                  bind:value={localSettings.llmModel}
                  onblur={saveQuietly}
                  onchange={saveQuietly}
                  placeholder="gemini-2.5-pro"
                />
                <span class="text-[10px] text-subtle">
                  {$t('settings.quickSelect')}
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300 me-2"
                    onclick={() => {
                      localSettings.llmModel = 'gemini-2.5-pro';
                      saveQuietly();
                    }}>{$t('settings.geminiProRecommended')}</button
                  >
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300"
                    onclick={() => {
                      localSettings.llmModel = 'gemini-2.5-flash';
                      saveQuietly();
                    }}>{$t('settings.geminiFlash')}</button
                  >
                </span>
              </label>
            {/if}

            <label class="flex flex-col gap-1 mt-4">
              <span class="text-sm text-muted">{$t('settings.systemPrompt')}</span>
              <textarea
                class="input w-full h-32 text-xs font-mono"
                bind:value={localSettings.llmSystemPrompt}
                onblur={saveQuietly}
                onchange={saveQuietly}
              ></textarea>
              <span class="text-[10px] text-subtle">{$t('settings.systemPromptHint')}</span>
            </label>
          </div>
        {:else if activeTab === 'jury'}
          <div class="space-y-5">
            <h3 class="text-md font-semibold text-default">{$t('settings.juryTitle')}</h3>
            <p class="text-xs text-muted">{$t('settings.juryDescription')}</p>

            <!-- Cloud opt-in gate -->
            <label
              class="flex items-start gap-3 rounded-md border border-amber-500/30 bg-amber-950/20 p-3"
            >
              <input
                type="checkbox"
                class="mt-1 accent-cortex-500"
                bind:checked={localSettings.juryCloudOptIn}
                onchange={saveQuietly}
              />
              <span class="text-xs text-amber-100">
                <strong>{$t('settings.juryT2ConsentLead')}</strong>
                {$t('settings.juryT2Consent')}
              </span>
            </label>

            <!-- Autonomy dial -->
            <div class="space-y-2">
              <span class="text-sm text-muted block">{$t('settings.autonomyLevel')}</span>
              <div
                class="flex gap-2 flex-wrap"
                role="group"
                aria-label={$t('settings.autonomyLevel')}
              >
                {#each [['observe', '👁', 'inbox.autonomy.observe'], ['propose', '💡', 'inbox.autonomy.propose'], ['act_confirm', '✅', 'inbox.autonomy.actConfirm'], ['act_auto', '🤖', 'inbox.autonomy.actAuto']] as [val, emoji, labelKey]}
                  <button
                    type="button"
                    aria-pressed={localSettings.juryAutonomyLevel === val}
                    class="px-3 py-1.5 rounded-lg border text-xs font-medium transition-all
                      {localSettings.juryAutonomyLevel === val
                      ? 'bg-purple-700 border-purple-500 text-white'
                      : 'bg-cortex-900/40 border-cortex-700/50 text-cortex-300 hover:border-cortex-500'}"
                    onclick={() => {
                      localSettings = {
                        ...localSettings,
                        juryAutonomyLevel: val as typeof localSettings.juryAutonomyLevel,
                      };
                      saveQuietly();
                    }}>{emoji} {$t(labelKey)}</button
                  >
                {/each}
              </div>
              <p class="text-[10px] text-subtle">{$t('settings.autonomyHint')}</p>
            </div>

            <!-- T1 commit threshold -->
            <label class="flex items-center gap-3">
              <span class="text-sm text-muted w-36">{$t('settings.juryT1Threshold')}</span>
              <input
                type="range"
                min="0.50"
                max="0.99"
                step="0.01"
                bind:value={localSettings.juryT1Threshold}
                onchange={saveQuietly}
                class="flex-1 accent-cortex-500"
              />
              <span class="text-xs font-mono text-cortex-300 w-10 text-end"
                >{Math.round(localSettings.juryT1Threshold * 100)}%</span
              >
            </label>
            <p class="text-[10px] text-subtle -mt-3">{$t('settings.juryT1ThresholdHint')}</p>

            <!-- T2 model + self-consistency -->
            {#if localSettings.juryCloudOptIn}
              <label class="flex flex-col gap-1">
                <span class="text-sm text-muted">{$t('settings.juryModelLabel')}</span>
                <input
                  type="text"
                  class="input w-full"
                  bind:value={localSettings.juryModel}
                  onblur={saveQuietly}
                  placeholder="gemini-2.5-pro"
                />
                <span class="text-[10px] text-subtle">
                  {$t('settings.quickSelect')}
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300 me-2"
                    onclick={() => {
                      localSettings.juryModel = 'gemini-2.5-pro';
                      saveQuietly();
                    }}>{$t('settings.modelProRecommended')}</button
                  >
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300"
                    onclick={() => {
                      localSettings.juryModel = 'gemini-2.5-flash';
                      saveQuietly();
                    }}>{$t('settings.modelFlashFaster')}</button
                  >
                </span>
              </label>

              <label class="flex flex-col gap-1">
                <span class="text-sm text-muted">{$t('settings.sourceReferenceModels')}</span>
                <input
                  type="text"
                  class="input w-full"
                  bind:value={sourceReferenceModelsInput}
                  onblur={saveSourceReferenceModels}
                  onchange={saveSourceReferenceModels}
                  placeholder="gemini-2.5-pro, gemini-2.5-flash"
                />
                <span class="text-[10px] text-subtle">
                  {$t('settings.quickSelect')}
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300 me-2"
                    onclick={() => {
                      sourceReferenceModelsInput = 'gemini-2.5-pro, gemini-2.5-flash';
                      saveSourceReferenceModels();
                    }}>{$t('settings.sourceModelsBoth')}</button
                  >
                  <button
                    type="button"
                    class="underline text-cortex-400 hover:text-cortex-300"
                    onclick={() => {
                      sourceReferenceModelsInput = 'gemini-2.5-pro';
                      saveSourceReferenceModels();
                    }}>{$t('settings.sourceModelsProOnly')}</button
                  >
                </span>
              </label>

              <label class="flex items-center gap-3">
                <span class="text-sm text-muted w-36">{$t('settings.selfConsistencyLabel')}</span>
                <input
                  type="number"
                  min="1"
                  max="5"
                  class="input w-20"
                  bind:value={localSettings.jurySelfConsistencyN}
                  onblur={saveQuietly}
                />
                <span class="text-[10px] text-subtle">{$t('settings.selfConsistencyHint')}</span>
              </label>
            {:else}
              <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3">
                <p class="text-xs text-subtle">{$t('settings.juryCloudDisabled')}</p>
              </div>
            {/if}

            <!-- API key (shared with LLM) — same secrets.env route as the AI Post-Processing tab. -->
            {#if localSettings.juryCloudOptIn}
              <label class="flex flex-col gap-1">
                <span class="text-sm text-muted">
                  {$t('settings.geminiApiKey')}
                  {#if configuredProviders.includes('gemini')}
                    <span class="ms-2 text-[10px] text-emerald-400"
                      >● {$t('settings.apiKeySaved')}</span
                    >
                  {:else}
                    <span class="ms-2 text-[10px] text-amber-400"
                      >○ {$t('settings.apiKeyMissing')}</span
                    >
                  {/if}
                </span>
                <div class="flex gap-2">
                  <input
                    type="password"
                    class="input flex-1"
                    bind:value={geminiKeyInput}
                    placeholder="AIzaSy…"
                    autocomplete="off"
                    onkeydown={(e) => {
                      if (e.key === 'Enter') void saveGeminiKey();
                    }}
                  />
                  <button
                    type="button"
                    class="btn-secondary text-xs px-3"
                    disabled={savingGeminiKey}
                    onclick={() => void saveGeminiKey()}
                  >
                    {savingGeminiKey ? $t('settings.savingKey') : $t('settings.saveKey')}
                  </button>
                </div>
                <span class="text-[10px] text-subtle">{$t('settings.jurySharedKeyHint')}</span>
              </label>

              <!-- T2 transport: direct Gemini REST, or the SAME Gemini 2.5 Pro via OpenRouter -->
              <label class="flex flex-col gap-1">
                <span class="text-sm text-muted">{$t('settings.juryConnection')}</span>
                <select
                  class="input w-full"
                  bind:value={localSettings.juryProvider}
                  onchange={saveQuietly}
                >
                  <option value="gemini">{$t('settings.juryConnectionGemini')}</option>
                  <option value="openrouter">{$t('settings.juryConnectionOpenRouter')}</option>
                </select>
                <span class="text-[10px] text-subtle">
                  {$t('settings.juryPolicyLead')}
                  <strong>{$t('settings.juryPolicyModel')}</strong>
                  {$t('settings.juryPolicyDetail')}
                </span>
              </label>

              {#if localSettings.juryProvider === 'openrouter'}
                <div class="flex flex-col gap-1">
                  <span class="text-sm text-muted">
                    {$t('settings.openRouterApiKey')}
                    {#if configuredProviders.includes('openrouter')}
                      <span class="ms-2 text-[10px] text-emerald-400"
                        >● {$t('settings.apiKeySaved')}</span
                      >
                    {:else}
                      <span class="ms-2 text-[10px] text-amber-400"
                        >○ {$t('settings.apiKeyMissing')}</span
                      >
                    {/if}
                  </span>
                  <div class="flex gap-2">
                    <input
                      type="password"
                      class="input flex-1"
                      bind:value={openrouterKeyInput}
                      placeholder="sk-or-…"
                      autocomplete="off"
                      onkeydown={(e) => {
                        if (e.key === 'Enter') void saveOpenrouterKey();
                      }}
                    />
                    <button
                      type="button"
                      class="btn-secondary text-xs px-3"
                      disabled={savingOpenrouterKey}
                      onclick={() => void saveOpenrouterKey()}
                    >
                      {savingOpenrouterKey ? $t('settings.savingKey') : $t('settings.saveKey')}
                    </button>
                  </div>
                  <span class="text-[10px] text-subtle">{$t('settings.openRouterKeyHint')}</span>
                </div>
              {/if}
            {/if}
          </div>
        {:else if activeTab === 'diagnostics'}
          {#if tauriAvailable}
            <DiagnosticsPanel />
          {:else}
            <div
              class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 text-xs text-muted"
            >
              {$t('desktopRuntimeRequired')}
            </div>
          {/if}
        {/if}
      </div>
    </div>

    <div class="flex justify-end gap-3 px-6 py-4 border-t border-cortex-800/50">
      <button
        class="btn btn-secondary"
        onclick={() => {
          cancelled = true;
          showSettings.set(false);
        }}>{$t('cancel')}</button
      >
      <button class="btn btn-primary" onclick={save} disabled={saving}>
        {saving ? $t('saving') : $t('save')}
      </button>
    </div>
  </div>
</div>
