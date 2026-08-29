<script lang="ts">
  import ApiKeyField from './ApiKeyField.svelte';
  import { t } from './i18n';
  import { ADVISORY_CLOUD_MODEL, type AppSettings } from './stores/settingsStore';

  let {
    settings = $bindable(),
    geminiKeyInput = $bindable(),
    configuredProviders,
    savingGeminiKey,
    savingOpenrouterKey,
    onSaveGeminiKey,
    onConsentToggle,
    onSaveQuietly,
  }: {
    settings: AppSettings;
    geminiKeyInput: string;
    configuredProviders: string[];
    savingGeminiKey: boolean;
    savingOpenrouterKey: boolean;
    onSaveGeminiKey: () => Promise<void>;
    onConsentToggle: () => void;
    onSaveQuietly: () => Promise<void>;
  } = $props();

  function chooseLocalModel(model: string) {
    settings = { ...settings, llmModel: model };
    void onSaveQuietly();
  }
</script>

<div class="space-y-4">
  <h3 class="text-md font-semibold text-default">{$t('settings.aiTitle')}</h3>
  <p class="text-xs text-muted">{$t('settings.aiDescription')}</p>
  <label class="flex flex-col gap-1">
    <span class="text-sm text-muted">{$t('settings.externalAsrScript')}</span>
    <input
      type="text"
      class="input w-full"
      bind:value={settings.externalAsrScriptPath}
      onblur={onSaveQuietly}
      onchange={onSaveQuietly}
      placeholder="/mnt/c/path/to/provider_refine.py"
      dir="ltr"
    />
    <span class="text-[10px] text-subtle">{$t('settings.externalAsrScriptHint')}</span>
  </label>
  <label class="flex items-center gap-3">
    <span class="text-sm text-muted w-32">{$t('settings.llmEngine')}</span>
    <select data-testid="llm-engine-select" class="input flex-1" bind:value={settings.llmMode}>
      <option value="None">{$t('settings.llmDisabledOption')}</option>
      <option value="Local">{$t('settings.llmLocalOption')}</option>
      <option value="Gemini">{$t('settings.llmCloudOption')}</option>
    </select>
  </label>

  {#if settings.llmMode === 'Local'}
    <label class="flex flex-col gap-1">
      <span class="text-sm text-muted">{$t('settings.localApiEndpoint')}</span>
      <input
        type="text"
        class="input w-full"
        bind:value={settings.llmEndpoint}
        onblur={onSaveQuietly}
        onchange={onSaveQuietly}
        placeholder="http://127.0.0.1:11434/v1/chat/completions"
        dir="ltr"
      />
      <span class="text-[10px] text-subtle">{$t('settings.localEndpointHint')}</span>
    </label>
    <label class="flex flex-col gap-1 mt-2">
      <span class="text-sm text-muted">{$t('settings.modelName')}</span>
      <input
        type="text"
        class="input w-full"
        bind:value={settings.llmModel}
        onblur={onSaveQuietly}
        onchange={onSaveQuietly}
        placeholder="heretic-final:latest"
        dir="ltr"
      />
      <span class="text-[10px] text-subtle">
        {$t('settings.quickSelect')}
        <button
          type="button"
          class="underline text-cortex-400 hover:text-cortex-300 me-2"
          onclick={() => chooseLocalModel('heretic-final:latest')}
          dir="ltr">heretic-final:latest</button
        >
        <button
          type="button"
          class="underline text-cortex-400 hover:text-cortex-300"
          onclick={() => chooseLocalModel('qwen2.5-coder:7b')}
          dir="ltr">qwen2.5-coder:7b</button
        >
      </span>
    </label>
  {:else if settings.llmMode === 'Gemini'}
    <label class="flex items-start gap-3 rounded-md border border-amber-500/30 bg-amber-950/20 p-3">
      <input
        type="checkbox"
        class="mt-1"
        bind:checked={settings.cloudLlmOptIn}
        onchange={onConsentToggle}
      />
      <span class="text-xs text-amber-100">{$t('settings.cloudLlmConsent')}</span>
    </label>
    <ApiKeyField
      labelKey="settings.geminiApiKey"
      hintKey="settings.apiKeyStorageHint"
      placeholder="AIzaSy..."
      configured={configuredProviders.includes('gemini')}
      bind:value={geminiKeyInput}
      saving={savingGeminiKey}
      disabled={savingGeminiKey || savingOpenrouterKey}
      onSave={onSaveGeminiKey}
    />
    <div class="flex flex-col gap-1 mt-2">
      <span class="text-sm text-muted">{$t('settings.geminiModel')}</span>
      <div
        data-testid="cloud-llm-model-fixed"
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
    </div>
  {/if}

  <label class="flex flex-col gap-1 mt-4">
    <span class="text-sm text-muted">{$t('settings.systemPrompt')}</span>
    <textarea
      class="input w-full h-32 text-xs font-mono"
      bind:value={settings.llmSystemPrompt}
      onblur={onSaveQuietly}
      onchange={onSaveQuietly}
    ></textarea>
    <span class="text-[10px] text-subtle">{$t('settings.systemPromptHint')}</span>
  </label>
</div>
