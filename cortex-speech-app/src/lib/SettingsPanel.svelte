<script lang="ts">
  import { settings, showSettings, settingsTab, type AppSettings } from './stores/settingsStore';
  import * as api from './commands';
  import { notifications } from './stores/notificationStore';
  import { segments } from './stores/segmentStore';
  import { isProcessing, statusMessage, batchProgress } from './stores/uiStore';
  import { startOperation, endOperation } from './invoke';
  import { PARQUET_EXPORT_SUPPORTED } from './appFeatures';
  import ModelDownload from './ModelDownload.svelte';
  import { t } from './i18n';
  import { get } from 'svelte/store';
  import { onDestroy } from 'svelte';
  import { isTauriRuntime } from './runtime';

  let localSettings: AppSettings = $state({ ...$settings });
  let activeTab = $state<'general' | 'asr' | 'audio' | 'export' | 'models' | 'ai' | 'jury'>($settingsTab);
  let saving = $state(false);
  let exportingAudio = $state(false);
  let sourceReferenceModelsInput = $state('');
  const tauriAvailable = isTauriRuntime();

  $effect(() => {
    const nextSettings = $settings;
    localSettings = { ...nextSettings };
    sourceReferenceModelsInput = nextSettings.sourceReferenceModels.join(', ');
  });
  $effect(() => { activeTab = $settingsTab; });

  onDestroy(() => {
    applySourceReferenceModelsInput();
    const currentStore = get(settings);
    if (JSON.stringify(localSettings) !== JSON.stringify(currentStore)) {
      settings.set(localSettings);
      if (tauriAvailable) {
        api.updateSettings(localSettings).catch(console.error);
      }
    }
  });

  function coerceSettingsForRuntime() {
    if (localSettings.exportFormat === 'parquet' && !PARQUET_EXPORT_SUPPORTED) {
      localSettings = { ...localSettings, exportFormat: 'json' };
    }
  }

  async function saveQuietly() {
    coerceSettingsForRuntime();
    settings.set(localSettings);
    if (!tauriAvailable) return;
    try {
      await api.updateSettings(localSettings);
    } catch (e) {
      console.error("Auto-save settings failed:", e);
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
    { id: 'ai', labelKey: 'AI Post-Processing' },
    { id: 'jury', labelKey: '📬 Listening Jury' },
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
    const verifiedIds = get(segments).filter((s) => s.verified).map((s) => s.id);
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
        notifications.success(
          $t('exportAudio.success', { count: String(result.succeeded) }),
          { detail: result.output_dir },
        );
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
  onkeydown={handleKeydown}
>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div role="presentation" class="card p-0 max-w-2xl w-full mx-4 max-h-[85vh] flex flex-col shadow-2xl" onclick={(e) => e.stopPropagation()}>
    <div class="flex items-center justify-between px-6 py-4 border-b border-cortex-800/50">
      <h2 class="text-lg font-semibold text-gray-100">{$t('settings')}</h2>
      <button data-testid="settings-close-btn" class="text-gray-400 hover:text-gray-200 transition-colors" onclick={() => showSettings.set(false)} aria-label={$t('close')}>✕</button>
    </div>

    <div class="flex gap-0 flex-1 min-h-0">
      <nav class="w-40 shrink-0 p-2 border-r border-cortex-800/50 space-y-1">
        {#each tabs as tab}
          <button
            class="w-full text-left px-3 py-2 rounded-lg text-sm transition-colors {activeTab === tab.id ? 'bg-cortex-700 text-cortex-100' : 'text-cortex-300 hover:text-cortex-100 hover:bg-cortex-800/50'}"
            disabled={!tauriAvailable && tab.id === 'models'}
            title={!tauriAvailable && tab.id === 'models' ? $t('desktopRuntimeRequired') : ''}
            onclick={() => activeTab = tab.id}
          >{tab.id === 'ai' ? 'AI Post-Processing' : $t(tab.labelKey)}</button>
        {/each}
      </nav>

      <div class="flex-1 p-6 overflow-y-auto space-y-5">
        {#if !tauriAvailable}
          <div class="rounded-md border border-amber-500/30 bg-amber-950/20 p-3 text-xs text-amber-100">
            {$t('settingsPreviewOnly')}
          </div>
        {/if}

        {#if activeTab === 'general'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('theme')}</span>
            <select class="input flex-1" bind:value={localSettings.theme}>
              <option value="dark">{$t('dark')}</option>
              <option value="light">{$t('light')}</option>
              <option value="system">{$t('system')}</option>
            </select>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('language')}</span>
            <select class="input flex-1" bind:value={localSettings.language}>
              <option value="ckb">{$t('kurdish')}</option>
              <option value="kmr">Kurmanji</option>
            </select>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={localSettings.autoNormalize} class="accent-cortex-500">
            <span class="text-sm text-gray-300">{$t('autoNormalize')}</span>
          </label>
          {#if localSettings.autoNormalize}
            <label class="flex items-center gap-3 cursor-pointer pl-6">
              <input type="checkbox" bind:checked={localSettings.verbalizeNumbers} class="accent-cortex-500">
              <span class="text-sm text-gray-300">{$t('verbalizeNumbers')}</span>
            </label>
          {/if}
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={localSettings.autoAlign} class="accent-cortex-500">
            <span class="text-sm text-gray-300">{$t('autoAlign')}</span>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={localSettings.autoplaySegments} class="accent-cortex-500">
            <span class="text-sm text-gray-300">Autoplay Segments on Selection</span>
          </label>

        {:else if activeTab === 'asr'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('asrModel')}</span>
            <select class="input flex-1" bind:value={localSettings.asrModel}>
              <option value="ctc-300m">Meta OmniASR CTC 300M</option>
              <option value="ctc-1b">Meta OmniASR CTC 1B</option>
              <option value="wsl-7b">Meta OmniASR 7B (WSL GPU)</option>
            </select>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('threads')}</span>
            <input type="range" min="1" max="16" bind:value={localSettings.numThreads} class="flex-1 accent-cortex-500">
            <span class="text-xs font-mono text-cortex-300 w-6 text-right">{localSettings.numThreads}</span>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={localSettings.enableGpu} class="accent-cortex-500">
            <span class="text-sm text-gray-300">{$t('gpuAcceleration')}</span>
          </label>

        {:else if activeTab === 'audio'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('vadThreshold')}</span>
            <input type="range" min="0" max="1" step="0.05" bind:value={localSettings.vadThreshold} class="flex-1 accent-cortex-500">
            <span class="text-xs font-mono text-cortex-300 w-8 text-right">{localSettings.vadThreshold}</span>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('minSegment')}</span>
            <input type="number" bind:value={localSettings.minSegmentSec} class="input w-20" min="1" max="60">
            <span class="text-xs text-cortex-400">{$t('seconds')}</span>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('maxSegment')}</span>
            <input type="number" bind:value={localSettings.maxSegmentSec} class="input w-20" min="1" max="300">
            <span class="text-xs text-cortex-400">{$t('seconds')}</span>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={localSettings.enableDenoising} class="accent-cortex-500">
            <span class="text-sm text-gray-300">AI Audio Cleanup (Denoise before ASR)</span>
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={localSettings.enableDiarization} class="accent-cortex-500">
            <span class="text-sm text-gray-300">{$t('enableDiarization')}</span>
          </label>
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('maxSpeakers')}</span>
            <input type="number" bind:value={localSettings.maxSpeakers} class="input w-20" min="1" max="32">
          </label>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={localSettings.assignSpeakerFromFilename} class="accent-cortex-500">
            <span class="text-sm text-gray-300">{$t('assignSpeakerFromFilename')}</span>
          </label>
          <div class="pt-3 border-t border-cortex-800/50 space-y-3">
            <p class="text-xs font-semibold text-cortex-300 uppercase tracking-wider">{$t('qualityGates')}</p>
            <label class="flex items-center gap-3 cursor-pointer">
              <input type="checkbox" bind:checked={localSettings.enforceQualityGates} class="accent-cortex-500">
              <span class="text-sm text-gray-300">{$t('enforceQualityGates')}</span>
            </label>
            <label class="flex items-center gap-3">
              <span class="text-sm text-gray-300 w-32">{$t('maxWer')}</span>
              <input type="range" min="0.05" max="0.80" step="0.05" bind:value={localSettings.maxWerThreshold} class="flex-1 accent-cortex-500">
              <span class="text-xs font-mono text-cortex-300 w-10 text-right">{Math.round(localSettings.maxWerThreshold * 100)}%</span>
            </label>
            <label class="flex items-center gap-3">
              <span class="text-sm text-gray-300 w-32">{$t('maxCer')}</span>
              <input type="range" min="0.05" max="0.80" step="0.05" bind:value={localSettings.maxCerThreshold} class="flex-1 accent-cortex-500">
              <span class="text-xs font-mono text-cortex-300 w-10 text-right">{Math.round(localSettings.maxCerThreshold * 100)}%</span>
            </label>
          </div>

        {:else if activeTab === 'export'}
          <label class="flex items-center gap-3">
            <span class="text-sm text-gray-300 w-32">{$t('exportFormat')}</span>
            <select class="input flex-1" bind:value={localSettings.exportFormat}>
              <option value="json">JSON (COCO-style manifest)</option>
              <option value="jsonl">JSONL (one segment per line)</option>
              <option value="csv">CSV</option>
              {#if PARQUET_EXPORT_SUPPORTED}
                <option value="parquet">Parquet</option>
              {/if}
            </select>
          </label>
          <div class="pt-2 border-t border-cortex-800/50 space-y-2">
            <p class="text-xs text-cortex-400">{$t('exportAudio.description')}</p>
            <button
              class="btn-secondary !text-xs"
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
          {:else}
            <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3 text-xs text-gray-400">
              {$t('desktopRuntimeRequired')}
            </div>
          {/if}

        {:else if activeTab === 'ai'}
          <div class="space-y-4">
            <h3 class="text-md font-semibold text-gray-200">Dual-Pass Transcription (LLM Refiner)</h3>
            <p class="text-xs text-gray-400">Use a local LLM by default. Cloud providers are opt-in and send text to the provider.</p>
            <label class="flex flex-col gap-1">
              <span class="text-sm text-gray-300">External ASR Provider Script</span>
              <input type="text" class="input w-full" bind:value={localSettings.externalAsrScriptPath} onblur={saveQuietly} onchange={saveQuietly} placeholder="/mnt/c/path/to/provider_refine.py" />
              <span class="text-[10px] text-gray-500">Required only for the WSL 7B provider. Use a WSL-visible path.</span>
            </label>
            <label class="flex items-center gap-3">
              <span class="text-sm text-gray-300 w-32">LLM Engine</span>
              <select class="input flex-1" bind:value={localSettings.llmMode}>
                <option value="None">Disabled (Fastest)</option>
                <option value="Local">Local API (e.g., LM Studio / Ollama)</option>
                <option value="Gemini">Google Gemini 3.1 Pro (Cloud)</option>
              </select>
            </label>

            {#if localSettings.llmMode === 'Local'}
              <label class="flex flex-col gap-1">
                <span class="text-sm text-gray-300">Local API Endpoint</span>
                <input type="text" class="input w-full" bind:value={localSettings.llmEndpoint} onblur={saveQuietly} onchange={saveQuietly} placeholder="http://127.0.0.1:11434/v1/chat/completions" />
                <span class="text-[10px] text-gray-500">Must be an OpenAI-compatible /v1/chat/completions endpoint.</span>
              </label>
              <label class="flex flex-col gap-1 mt-2">
                <span class="text-sm text-gray-300">Model Name</span>
                <input type="text" class="input w-full" bind:value={localSettings.llmModel} onblur={saveQuietly} onchange={saveQuietly} placeholder="heretic-final:latest" />
                <span class="text-[10px] text-gray-500">
                  Quick select: 
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300 mr-2" onclick={() => { localSettings.llmModel = 'heretic-final:latest'; saveQuietly(); }}>heretic-final:latest</button>
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300" onclick={() => { localSettings.llmModel = 'qwen2.5-coder:7b'; saveQuietly(); }}>qwen2.5-coder:7b</button>
                </span>
              </label>
            {:else if localSettings.llmMode === 'Gemini'}
              <label class="flex items-start gap-3 rounded-md border border-amber-500/30 bg-amber-950/20 p-3">
                <input type="checkbox" class="mt-1" bind:checked={localSettings.cloudLlmOptIn} onchange={saveQuietly} />
                <span class="text-xs text-amber-100">
                  I understand Gemini sends transcript text to Google. Keep this disabled for fully offline dataset work.
                </span>
              </label>
              <label class="flex flex-col gap-1">
                <span class="text-sm text-gray-300">Gemini API Key</span>
                <input type="password" class="input w-full" bind:value={localSettings.llmApiKey} onblur={saveQuietly} onchange={saveQuietly} placeholder="AIzaSy..." />
                <span class="text-[10px] text-gray-500">
                  The key is used for this session and is not written to settings.json.
                  {#if localSettings.llmApiKeyConfigured} A cloud key was previously configured. {/if}
                </span>
              </label>
              <label class="flex flex-col gap-1 mt-2">
                <span class="text-sm text-gray-300">Gemini Model</span>
                <input type="text" class="input w-full" bind:value={localSettings.llmModel} onblur={saveQuietly} onchange={saveQuietly} placeholder="gemini-2.5-pro" />
                <span class="text-[10px] text-gray-500">
                  Quick select: 
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300 mr-2" onclick={() => { localSettings.llmModel = 'gemini-2.5-pro'; saveQuietly(); }}>Gemini 2.5 Pro (Recommended)</button>
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300" onclick={() => { localSettings.llmModel = 'gemini-2.5-flash'; saveQuietly(); }}>Gemini 2.5 Flash</button>
                </span>
              </label>
            {/if}

            <label class="flex flex-col gap-1 mt-4">
              <span class="text-sm text-gray-300">System Prompt</span>
              <textarea class="input w-full h-32 text-xs font-mono" bind:value={localSettings.llmSystemPrompt} onblur={saveQuietly} onchange={saveQuietly}></textarea>
              <span class="text-[10px] text-gray-500">The instructions sent to the LLM to process the transcription.</span>
            </label>
          </div>

        {:else if activeTab === 'jury'}
          <div class="space-y-5">
            <h3 class="text-md font-semibold text-gray-200">📬 Listening Jury</h3>
            <p class="text-xs text-gray-400">
              The Jury cascades segments from IRT consensus (T0) → text analysis (T1) → Gemini audio (T2) → human inbox.
              Cloud tiers send audio to Google and require an opt-in.
            </p>

            <!-- Cloud opt-in gate -->
            <label class="flex items-start gap-3 rounded-md border border-amber-500/30 bg-amber-950/20 p-3">
              <input type="checkbox" class="mt-1 accent-cortex-500" bind:checked={localSettings.juryCloudOptIn} onchange={saveQuietly} />
              <span class="text-xs text-amber-100">
                <strong>I understand</strong> that enabling T2 sends audio clips to Google Gemini.
                Keep disabled for fully offline / air-gapped dataset work.
              </span>
            </label>

            <!-- Autonomy dial -->
            <div class="space-y-2">
              <span class="text-sm text-gray-300 block">Autonomy level</span>
              <div class="flex gap-2 flex-wrap">
                {#each [['observe','👁 Observe'],['propose','💡 Propose'],['act_confirm','✅ Act+Confirm'],['act_auto','🤖 Act Auto']] as [val, label]}
                  <button
                    type="button"
                    class="px-3 py-1.5 rounded-lg border text-xs font-medium transition-all
                      {localSettings.juryAutonomyLevel === val
                        ? 'bg-purple-700 border-purple-500 text-white'
                        : 'bg-cortex-900/40 border-cortex-700/50 text-cortex-300 hover:border-cortex-500'}"
                    onclick={() => { localSettings = { ...localSettings, juryAutonomyLevel: val as typeof localSettings.juryAutonomyLevel }; saveQuietly(); }}
                  >{label}</button>
                {/each}
              </div>
              <p class="text-[10px] text-gray-500">
                Observe: jury runs but humans decide everything. Act Auto: jury commits without review (requires high T1 threshold).
              </p>
            </div>

            <!-- T1 commit threshold -->
            <label class="flex items-center gap-3">
              <span class="text-sm text-gray-300 w-36">T1 commit threshold</span>
              <input type="range" min="0.50" max="0.99" step="0.01"
                bind:value={localSettings.juryT1Threshold}
                onchange={saveQuietly}
                class="flex-1 accent-cortex-500"
              >
              <span class="text-xs font-mono text-cortex-300 w-10 text-right">{Math.round(localSettings.juryT1Threshold * 100)}%</span>
            </label>
            <p class="text-[10px] text-gray-500 -mt-3">Segments below this combined lexicon+perplexity score escalate to T2. Raise to reduce cloud calls.</p>

            <!-- T2 model + self-consistency -->
            {#if localSettings.juryCloudOptIn}
              <label class="flex flex-col gap-1">
                <span class="text-sm text-gray-300">Gemini model (T2 audio judge)</span>
                <input type="text" class="input w-full" bind:value={localSettings.juryModel}
                  onblur={saveQuietly} placeholder="gemini-2.5-pro" />
                <span class="text-[10px] text-gray-500">
                  Quick select:
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300 mr-2"
                    onclick={() => { localSettings.juryModel = 'gemini-2.5-pro'; saveQuietly(); }}
                  >2.5 Pro (Recommended)</button>
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300"
                    onclick={() => { localSettings.juryModel = 'gemini-2.5-flash'; saveQuietly(); }}
                  >2.5 Flash (Faster)</button>
                </span>
              </label>

              <label class="flex flex-col gap-1">
                <span class="text-sm text-gray-300">Source reference models</span>
                <input type="text" class="input w-full" bind:value={sourceReferenceModelsInput}
                  onblur={saveSourceReferenceModels} onchange={saveSourceReferenceModels}
                  placeholder="gemini-2.5-pro, gemini-2.5-flash" />
                <span class="text-[10px] text-gray-500">
                  Quick select:
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300 mr-2"
                    onclick={() => { sourceReferenceModelsInput = 'gemini-2.5-pro, gemini-2.5-flash'; saveSourceReferenceModels(); }}
                  >2.5 Pro + Flash</button>
                  <button type="button" class="underline text-cortex-400 hover:text-cortex-300"
                    onclick={() => { sourceReferenceModelsInput = 'gemini-2.5-pro'; saveSourceReferenceModels(); }}
                  >2.5 Pro only</button>
                </span>
              </label>

              <label class="flex items-center gap-3">
                <span class="text-sm text-gray-300 w-36">Self-consistency N</span>
                <input type="number" min="1" max="5" class="input w-20"
                  bind:value={localSettings.jurySelfConsistencyN}
                  onblur={saveQuietly}
                >
                <span class="text-[10px] text-gray-500">Votes per segment. 3 = majority vote. Higher = more accurate, more API calls.</span>
              </label>
            {:else}
              <div class="rounded-md border border-cortex-700/40 bg-cortex-900/30 p-3">
                <p class="text-xs text-gray-500">T2 Gemini audio judge is disabled. Enable the cloud opt-in above to configure it.</p>
              </div>
            {/if}

            <!-- API key (shared with LLM) -->
            {#if localSettings.juryCloudOptIn}
              <label class="flex flex-col gap-1">
                <span class="text-sm text-gray-300">Gemini API Key</span>
                <input type="password" class="input w-full" bind:value={localSettings.llmApiKey}
                  onblur={saveQuietly} placeholder="AIzaSy…" />
                <span class="text-[10px] text-gray-500">Shared with the AI Post-Processing tab. Not written to disk.</span>
              </label>
            {/if}
          </div>
        {/if}
      </div>
    </div>

    <div class="flex justify-end gap-3 px-6 py-4 border-t border-cortex-800/50">
      <button class="btn-secondary" onclick={() => showSettings.set(false)}>{$t('cancel')}</button>
      <button class="btn-primary" onclick={save} disabled={saving}>
        {saving ? $t('saving') : $t('save')}
      </button>
    </div>
  </div>
</div>
