<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { focusTrap } from './actions/focusTrap';
  import * as api from './commands';
  import { t } from './i18n';
  import { isTauriRuntime } from './runtime';
  import SettingsAiTab from './SettingsAiTab.svelte';
  import SettingsAsrTab from './SettingsAsrTab.svelte';
  import SettingsAudioTab from './SettingsAudioTab.svelte';
  import SettingsDiagnosticsTab from './SettingsDiagnosticsTab.svelte';
  import SettingsExportTab from './SettingsExportTab.svelte';
  import SettingsGeneralFields from './SettingsGeneralFields.svelte';
  import SettingsJuryTab from './SettingsJuryTab.svelte';
  import SettingsModelsTab from './SettingsModelsTab.svelte';
  import { exportVerifiedAudioFromSettings } from './settingsAudioExport';
  import { createSettingsKeyController } from './settingsKeyController.svelte';
  import { createSettingsPersistenceController } from './settingsPersistenceController';
  import { notifications } from './stores/notificationStore';
  import { settings, showSettings, settingsTab, type AppSettings } from './stores/settingsStore';

  type SettingsTab =
    'general' | 'asr' | 'audio' | 'export' | 'models' | 'ai' | 'jury' | 'diagnostics';

  let localSettings: AppSettings = $state({ ...$settings });
  let activeTab = $state<SettingsTab>($settingsTab);
  let saving = $state(false);
  let exportingAudio = $state(false);
  let cancelled = $state(false);
  const tauriAvailable = isTauriRuntime();
  const keys = createSettingsKeyController(tauriAvailable);
  const persistence = createSettingsPersistenceController({
    tauriAvailable,
    getLocal: () => localSettings,
    setLocal: (value) => (localSettings = value),
    flushPendingKeys: keys.flushPendingKeys,
    onSavingChange: (value) => (saving = value),
  });

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

  $effect(() => {
    activeTab = $settingsTab;
  });

  // Couch Review remains isolated here: this decomposition does not change reviewer admission,
  // revocation, compensation, or attribution behavior.
  let couchStatus = $state<import('./commands').CouchStatus | null>(null);
  let couchBusy = $state(false);
  let couchNames = $state('');
  let spotChecks = $state<import('./commands').SpotCheckScore[]>([]);
  let throughput = $state<import('./commands').ReviewerThroughput[]>([]);
  let agreement = $state<import('./commands').AgreementExport | null>(null);
  let agreementBusy = $state(false);

  async function toggleCouch() {
    if (couchBusy || !tauriAvailable) return;
    couchBusy = true;
    try {
      couchStatus = couchStatus?.running
        ? await api.stopCouchReview()
        : await api.startCouchReview(
            couchNames
              .split(',')
              .map((name) => name.trim())
              .filter(Boolean),
          );
    } catch (error) {
      notifications.error($t('settings.couchFailed'), { cause: error });
    } finally {
      couchBusy = false;
    }
  }

  async function revokeReviewer(name: string) {
    if (couchBusy || !tauriAvailable) return;
    couchBusy = true;
    try {
      couchStatus = await api.revokeCouchReviewer(name);
    } catch (error) {
      notifications.error($t('settings.couchRevoke'), { cause: error });
    } finally {
      couchBusy = false;
    }
  }

  async function exportAgreement() {
    if (agreementBusy || !tauriAvailable) return;
    agreementBusy = true;
    try {
      agreement = await api.exportAgreementSample();
      if (!agreement) notifications.info($t('settings.couchAgreementNone'));
    } catch (error) {
      notifications.error($t('settings.couchAgreement'), { cause: error });
    } finally {
      agreementBusy = false;
    }
  }

  async function loadCouchEvidence() {
    if (!tauriAvailable) return;
    try {
      couchStatus = await api.couchReviewStatus();
    } catch (error) {
      console.error('couch status load failed:', error);
    }
    try {
      spotChecks = (await api.spotCheckReport()) ?? [];
    } catch (error) {
      console.error('spot-check report load failed:', error);
      spotChecks = [];
    }
    try {
      throughput = (await api.reviewerThroughput()) ?? [];
    } catch (error) {
      console.error('reviewer throughput load failed:', error);
      throughput = [];
    }
  }

  onMount(() => {
    void keys.loadConfiguredProviders();
    void loadCouchEvidence();
  });

  onDestroy(() => {
    if (cancelled) return;
    void keys.flushPendingKeys().catch(() => {});
    persistence.saveOnDestroy();
  });

  const llmConsentToggled = () => persistence.consentToggled('cloudLlmOptIn');
  const juryConsentToggled = () => persistence.consentToggled('juryCloudOptIn');
  const saveQuietly = () => persistence.saveQuietly();
  const save = () => persistence.save();

  const handleExportAudioFromSettings = () =>
    exportVerifiedAudioFromSettings(tauriAvailable, (busy) => (exportingAudio = busy));

  function requestClose() {
    if (keys.hasPendingKey) {
      void save();
      return;
    }
    showSettings.set(false);
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') requestClose();
  }
</script>

<div
  class="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
  data-testid="settings-panel"
  role="dialog"
  aria-modal="true"
  aria-labelledby="settings-title"
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
      <h2 id="settings-title" class="text-lg font-semibold text-default">{$t('settings')}</h2>
      <button
        data-testid="settings-close-btn"
        class="text-muted hover:text-default transition-colors text-xs"
        onclick={requestClose}
      >
        {$t('close')}
      </button>
    </div>

    <div class="flex gap-0 flex-1 min-h-0">
      <!-- overflow-y-auto: the dialog is capped at 85vh, so at 200 % browser zoom (or on a short
           window) the eight tabs are taller than the column. Without its own scroller the last tabs
           — Jury and Diagnostics — were simply CLIPPED, with no way to reach them at all. -->
      <nav class="w-40 shrink-0 overflow-y-auto p-2 border-r border-cortex-800/50 space-y-1">
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
          <SettingsGeneralFields bind:settings={localSettings} />

          <!-- Couch Review: LAN-only, token-gated phone review server (off by default, per-session).
               The URL carries a random session token; audio never leaves the local network. -->
          <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 space-y-2">
            <div class="flex items-center justify-between">
              <span class="text-sm text-default">{$t('settings.couchTitle')}</span>
              <button
                type="button"
                class="btn-secondary text-xs px-3"
                disabled={couchBusy || !tauriAvailable}
                onclick={() => void toggleCouch()}
              >
                {couchBusy
                  ? $t('settings.couchWorking')
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
                  <code class="block break-all text-[10px] text-default select-all" dir="ltr"
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
                    <bdi class="text-xs text-default font-semibold" dir="auto">{reviewer.name}</bdi>
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
                    dir="ltr"
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
                      dir="ltr"
                      onfocus={(e) => (e.target as HTMLInputElement).select()}
                    />
                  {/if}
                  {#if reviewer.funnelUrl}
                    <span class="text-[10px] text-subtle block"
                      >{$t('settings.couchFunnelUrl')}</span
                    >
                    <input
                      class="input w-full !text-xs font-mono"
                      readonly
                      value={reviewer.funnelUrl}
                      dir="ltr"
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
                    <bdi class="text-default" dir="auto">{r.reviewer}</bdi>
                    <bdi class="text-muted" dir="ltr">
                      {r.clips}{r.medianSeconds !== null ? ` · ${r.medianSeconds.toFixed(1)}s` : ''}
                    </bdi>
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
                    <bdi class="text-default" dir="auto">{s.reviewer}</bdi>
                    <bdi
                      dir="ltr"
                      class={s.noticed < s.checks / 2
                        ? 'text-rose-300 font-semibold'
                        : 'text-muted'}
                    >
                      {s.noticed}/{s.checks} · CER {(s.meanCer * 100).toFixed(1)}%
                    </bdi>
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
                  <p class="text-[10px] text-subtle" dir="auto">
                    <bdi dir="auto">{agreement.raterA}</bdi> ·
                    <bdi dir="auto">{agreement.raterB}</bdi>
                    — <bdi dir="ltr">{agreement.items}</bdi>
                    {#if agreement.otherReviewers.length}· +<bdi dir="auto"
                        >{agreement.otherReviewers.join(', ')}</bdi
                      >{/if}
                  </p>
                  <input
                    class="input w-full !text-[10px] font-mono"
                    readonly
                    value={agreement.path}
                    dir="ltr"
                    onfocus={(e) => (e.target as HTMLInputElement).select()}
                  />
                  <p class="text-[10px] text-subtle">{$t('settings.couchAgreementHint')}</p>
                {/if}
              </div>
            {/if}
          </div>
        {:else if activeTab === 'asr'}
          <SettingsAsrTab bind:settings={localSettings} />
        {:else if activeTab === 'audio'}
          <SettingsAudioTab bind:settings={localSettings} />
        {:else if activeTab === 'export'}
          <SettingsExportTab
            bind:settings={localSettings}
            {tauriAvailable}
            {exportingAudio}
            onExportAudio={handleExportAudioFromSettings}
          />
        {:else if activeTab === 'models'}
          <SettingsModelsTab {tauriAvailable} />
        {:else if activeTab === 'ai'}
          <SettingsAiTab
            bind:settings={localSettings}
            bind:geminiKeyInput={keys.geminiKeyInput}
            configuredProviders={keys.configuredProviders}
            savingGeminiKey={keys.savingGeminiKey}
            savingOpenrouterKey={keys.savingOpenrouterKey}
            onSaveGeminiKey={keys.saveGeminiKey}
            onConsentToggle={llmConsentToggled}
            onSaveQuietly={saveQuietly}
          />
        {:else if activeTab === 'jury'}
          <SettingsJuryTab
            bind:settings={localSettings}
            bind:geminiKeyInput={keys.geminiKeyInput}
            bind:openrouterKeyInput={keys.openrouterKeyInput}
            configuredProviders={keys.configuredProviders}
            savingGeminiKey={keys.savingGeminiKey}
            savingOpenrouterKey={keys.savingOpenrouterKey}
            onSaveGeminiKey={keys.saveGeminiKey}
            onSaveOpenrouterKey={keys.saveOpenrouterKey}
            onConsentToggle={juryConsentToggled}
            onSaveQuietly={saveQuietly}
          />
        {:else if activeTab === 'diagnostics'}
          <SettingsDiagnosticsTab {tauriAvailable} />
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
