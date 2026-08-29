import { get } from 'svelte/store';
import * as api from './commands';
import { t } from './i18n';
import { notifications } from './stores/notificationStore';

export class ApiKeySaveFailure extends Error {
  constructor(provider: string) {
    super(`The ${provider} API key was not saved`);
    this.name = 'ApiKeySaveFailure';
  }
}

export function createSettingsKeyController(tauriAvailable: boolean) {
  let configuredProviders = $state<string[]>([]);
  let openrouterKeyInput = $state('');
  let geminiKeyInput = $state('');
  let savingOpenrouterKey = $state(false);
  let savingGeminiKey = $state(false);
  let openrouterKeySave: Promise<void> | null = null;
  let geminiKeySave: Promise<void> | null = null;

  async function loadConfiguredProviders(): Promise<void> {
    if (!tauriAvailable) return;
    try {
      configuredProviders = await api.getConfiguredProviders();
    } catch (error) {
      console.error('Failed to load configured cloud providers:', error);
    }
  }

  async function saveKey(provider: 'gemini' | 'openrouter'): Promise<void> {
    if (!tauriAvailable) return;
    const activeSave = provider === 'gemini' ? geminiKeySave : openrouterKeySave;
    if (activeSave) return activeSave;

    const providerName = provider === 'gemini' ? 'Gemini' : 'OpenRouter';
    const requested = (provider === 'gemini' ? geminiKeyInput : openrouterKeyInput).trim();
    if (provider === 'gemini') savingGeminiKey = true;
    else savingOpenrouterKey = true;

    const operation = (async () => {
      try {
        configuredProviders = await api.setApiKey(provider, requested);
        if (provider === 'gemini' && geminiKeyInput.trim() === requested) geminiKeyInput = '';
        if (provider === 'openrouter' && openrouterKeyInput.trim() === requested) {
          openrouterKeyInput = '';
        }
        const translate = get(t);
        notifications.success(
          configuredProviders.includes(provider)
            ? translate('settings.apiKeySavedToast', { provider: providerName })
            : translate('settings.apiKeyClearedToast', { provider: providerName }),
        );
      } catch (error) {
        notifications.error(get(t)('settings.apiKeySaveFailedToast', { provider: providerName }), {
          cause: error,
        });
        throw new ApiKeySaveFailure(providerName);
      }
    })();

    if (provider === 'gemini') geminiKeySave = operation;
    else openrouterKeySave = operation;
    try {
      await operation;
    } finally {
      if (provider === 'gemini') {
        if (geminiKeySave === operation) geminiKeySave = null;
        savingGeminiKey = false;
      } else {
        if (openrouterKeySave === operation) openrouterKeySave = null;
        savingOpenrouterKey = false;
      }
    }
  }

  const saveGeminiKey = () => saveKey('gemini');
  const saveOpenrouterKey = () => saveKey('openrouter');

  async function flushPendingKeys(): Promise<void> {
    if (openrouterKeyInput.trim() || openrouterKeySave) await saveOpenrouterKey();
    if (geminiKeyInput.trim() || geminiKeySave) await saveGeminiKey();
  }

  return {
    get configuredProviders() {
      return configuredProviders;
    },
    get openrouterKeyInput() {
      return openrouterKeyInput;
    },
    set openrouterKeyInput(value: string) {
      openrouterKeyInput = value;
    },
    get geminiKeyInput() {
      return geminiKeyInput;
    },
    set geminiKeyInput(value: string) {
      geminiKeyInput = value;
    },
    get savingOpenrouterKey() {
      return savingOpenrouterKey;
    },
    get savingGeminiKey() {
      return savingGeminiKey;
    },
    get hasPendingKey() {
      return Boolean(
        openrouterKeyInput.trim() || geminiKeyInput.trim() || openrouterKeySave || geminiKeySave,
      );
    },
    loadConfiguredProviders,
    saveGeminiKey,
    saveOpenrouterKey,
    flushPendingKeys,
  };
}
