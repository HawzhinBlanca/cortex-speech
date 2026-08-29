<script lang="ts">
  import ApiKeyField from './ApiKeyField.svelte';
  import { autonomyLabelKey, autonomyValues, t } from './i18n';
  import { ADVISORY_CLOUD_MODEL, type AppSettings } from './stores/settingsStore';

  let {
    settings = $bindable(),
    geminiKeyInput = $bindable(),
    openrouterKeyInput = $bindable(),
    configuredProviders,
    savingGeminiKey,
    savingOpenrouterKey,
    onSaveGeminiKey,
    onSaveOpenrouterKey,
    onConsentToggle,
    onSaveQuietly,
  }: {
    settings: AppSettings;
    geminiKeyInput: string;
    openrouterKeyInput: string;
    configuredProviders: string[];
    savingGeminiKey: boolean;
    savingOpenrouterKey: boolean;
    onSaveGeminiKey: () => Promise<void>;
    onSaveOpenrouterKey: () => Promise<void>;
    onConsentToggle: () => void;
    onSaveQuietly: () => Promise<void>;
  } = $props();

  function chooseAutonomy(value: AppSettings['juryAutonomyLevel']) {
    settings = { ...settings, juryAutonomyLevel: value };
    void onSaveQuietly();
  }
</script>

<div class="space-y-5">
  <h3 class="text-md font-semibold text-default">{$t('settings.juryTitle')}</h3>
  <p class="text-xs text-muted">{$t('settings.juryDescription')}</p>

  <label class="flex items-start gap-3 rounded-md border border-amber-500/30 bg-amber-950/20 p-3">
    <input
      data-testid="jury-cloud-opt-in"
      type="checkbox"
      class="mt-1 accent-cortex-500"
      bind:checked={settings.juryCloudOptIn}
      onchange={onConsentToggle}
    />
    <span class="text-xs text-amber-100">
      <strong>{$t('settings.juryT2ConsentLead')}</strong>
      {$t('settings.juryT2Consent')}
    </span>
  </label>

  <div class="space-y-2">
    <span class="text-sm text-muted block">{$t('settings.autonomyLevel')}</span>
    <div class="flex gap-2 flex-wrap" role="group" aria-label={$t('settings.autonomyLevel')}>
      {#each autonomyValues as value (value)}
        <button
          type="button"
          aria-pressed={settings.juryAutonomyLevel === value}
          class="px-3 py-1.5 rounded-lg border text-xs font-medium transition-all
            {settings.juryAutonomyLevel === value
            ? 'bg-purple-700 border-purple-500 text-white'
            : 'bg-cortex-900/40 border-cortex-700/50 text-cortex-300 hover:border-cortex-500'}"
          onclick={() => chooseAutonomy(value)}>{$t(autonomyLabelKey(value))}</button
        >
      {/each}
    </div>
    <p class="text-[10px] text-subtle">{$t('settings.autonomyHint')}</p>
  </div>

  <label class="flex items-center gap-3">
    <span class="text-sm text-muted w-36">{$t('settings.juryT1Threshold')}</span>
    <input
      type="range"
      min="0.50"
      max="0.99"
      step="0.01"
      bind:value={settings.juryT1Threshold}
      onchange={onSaveQuietly}
      class="flex-1 accent-cortex-500"
    />
    <span class="text-xs font-mono text-cortex-300 w-10 text-end">
      {Math.round(settings.juryT1Threshold * 100)}%
    </span>
  </label>
  <p class="text-[10px] text-subtle -mt-3">{$t('settings.juryT1ThresholdHint')}</p>

  {#if settings.juryCloudOptIn}
    <div class="flex flex-col gap-1">
      <span class="text-sm text-muted">{$t('settings.juryModelLabel')}</span>
      <div
        data-testid="jury-model-fixed"
        class="rounded-md border border-cortex-700/50 bg-cortex-900/40 px-3 py-2"
      >
        <div class="flex items-center justify-between gap-3">
          <span class="text-sm font-medium text-default">{$t('settings.advisoryModelName')}</span>
          <span class="text-[10px] text-subtle">{$t('settings.modelFixedByPolicy')}</span>
        </div>
        <span class="mt-0.5 block font-mono text-[10px] text-cortex-400">
          {ADVISORY_CLOUD_MODEL}
        </span>
      </div>
      <span class="text-[10px] text-subtle">{$t('settings.sourceReferenceFixedHint')}</span>
    </div>

    <label class="flex items-center gap-3">
      <span class="text-sm text-muted w-36">{$t('settings.selfConsistencyLabel')}</span>
      <input
        type="number"
        min="1"
        max="5"
        class="input w-20"
        bind:value={settings.jurySelfConsistencyN}
        onblur={onSaveQuietly}
      />
      <span class="text-[10px] text-subtle">{$t('settings.selfConsistencyHint')}</span>
    </label>
  {:else}
    <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3">
      <p class="text-xs text-subtle">{$t('settings.juryCloudDisabled')}</p>
    </div>
  {/if}

  {#if settings.juryCloudOptIn}
    <ApiKeyField
      labelKey="settings.geminiApiKey"
      hintKey="settings.jurySharedKeyHint"
      placeholder="AIzaSy…"
      configured={configuredProviders.includes('gemini')}
      bind:value={geminiKeyInput}
      saving={savingGeminiKey}
      disabled={savingGeminiKey || savingOpenrouterKey}
      onSave={onSaveGeminiKey}
    />

    <label class="flex flex-col gap-1">
      <span class="text-sm text-muted">{$t('settings.juryConnection')}</span>
      <select class="input w-full" bind:value={settings.juryProvider} onchange={onSaveQuietly}>
        <option value="gemini">{$t('settings.juryConnectionGemini')}</option>
        <option value="openrouter">{$t('settings.juryConnectionOpenRouter')}</option>
      </select>
      <span class="text-[10px] text-subtle">
        {$t('settings.juryPolicyLead')}
        <strong>{$t('settings.juryPolicyModel')}</strong>
        {$t('settings.juryPolicyDetail')}
      </span>
    </label>

    {#if settings.juryProvider === 'openrouter'}
      <ApiKeyField
        labelKey="settings.openRouterApiKey"
        hintKey="settings.openRouterKeyHint"
        placeholder="sk-or-…"
        configured={configuredProviders.includes('openrouter')}
        bind:value={openrouterKeyInput}
        saving={savingOpenrouterKey}
        disabled={savingGeminiKey || savingOpenrouterKey}
        onSave={onSaveOpenrouterKey}
      />
    {/if}
  {/if}
</div>
