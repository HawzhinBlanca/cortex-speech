import { writable } from 'svelte/store';

export type Theme = 'dark' | 'light' | 'system';
export type ExportFormat = 'json' | 'jsonl' | 'csv' | 'parquet';
export type AsrModel = 'wsl-7b';
export const ADVISORY_CLOUD_MODEL = 'gemini-2.5-pro' as const;
export type AdvisoryCloudModel = typeof ADVISORY_CLOUD_MODEL;

export interface AppSettings {
  theme: Theme;
  autoNormalize: boolean;
  verbalizeNumbers: boolean;
  autoAlign: boolean;
  exportFormat: ExportFormat;
  asrModel: AsrModel;
  /** Production invariant mirrored from the backend; retained in the wire shape for compatibility. */
  useFinetuned: boolean;
  vadThreshold: number;
  minSegmentSec: number;
  maxSegmentSec: number;
  numThreads: number;
  enableGpu: boolean;
  language: string;
  enableDiarization: boolean;
  enableDenoising: boolean;
  maxSpeakers: number;
  assignSpeakerFromFilename: boolean;
  maxWerThreshold: number;
  maxCerThreshold: number;
  enforceQualityGates: boolean;
  autoplaySegments: boolean;
  hfTrainRatio: number;
  hfValRatio: number;
  hfTestRatio: number;
  hfSplitSeed: number;
  hfSpeakerDisjoint: boolean;
  hfLicense: string;
  llmMode: 'None' | 'Local' | 'Gemini';
  llmEndpoint: string;
  llmApiKey: string;
  llmApiKeyConfigured: boolean;
  cloudLlmOptIn: boolean;
  llmSystemPrompt: string;
  llmModel: string;
  externalAsrScriptPath: string;
  // Listening Jury settings
  juryCloudOptIn: boolean;
  juryModel: AdvisoryCloudModel;
  /** T2 judge transport: direct Gemini REST, or OpenRouter (same Gemini 2.5 Pro model, OR quota/key). */
  juryProvider: 'gemini' | 'openrouter';
  sourceReferenceModels: AdvisoryCloudModel[];
  jurySelfConsistencyN: number;
  juryAutonomyLevel: 'observe' | 'propose' | 'act_confirm' | 'act_auto';
  juryT1Threshold: number;
}

export const defaultSettings: AppSettings = {
  theme: 'dark',
  autoNormalize: true,
  verbalizeNumbers: true,
  autoAlign: false,
  exportFormat: 'json',
  // Accuracy-first factory contract: the fine-tuned OmniASR-7B + LoRA champion is the sole default.
  // If it is unavailable, the app fails closed and asks; it never silently selects a smaller model.
  asrModel: 'wsl-7b',
  // Compatibility-only wire field; production adapters and the Rust backend both force it off.
  useFinetuned: false,
  vadThreshold: 0.5,
  minSegmentSec: 3,
  maxSegmentSec: 15,
  numThreads: 4,
  enableGpu: true,
  language: 'ckb',
  enableDiarization: true,
  enableDenoising: false,
  maxSpeakers: 8,
  assignSpeakerFromFilename: true,
  maxWerThreshold: 0.35,
  maxCerThreshold: 0.2,
  enforceQualityGates: false,
  autoplaySegments: false,
  hfTrainRatio: 0.8,
  hfValRatio: 0.1,
  hfTestRatio: 0.1,
  hfSplitSeed: 42,
  hfSpeakerDisjoint: true,
  hfLicense: 'mit',
  llmMode: 'None', // factory default (2026-08-20): refinement is opt-in, never a champion dependency
  llmEndpoint: 'http://127.0.0.1:11434/v1/chat/completions',
  llmApiKey: '',
  llmApiKeyConfigured: false,
  cloudLlmOptIn: false,
  llmSystemPrompt:
    'You are an expert Kurdish linguist. Fix the phonetic transcription errors in the following text, preserving the exact meaning. Output ONLY the corrected text, no explanations.',
  llmModel: 'heretic-final:latest',
  externalAsrScriptPath: '',
  // Listening Jury defaults
  juryCloudOptIn: false,
  juryModel: ADVISORY_CLOUD_MODEL,
  juryProvider: 'gemini',
  sourceReferenceModels: [ADVISORY_CLOUD_MODEL],
  jurySelfConsistencyN: 3,
  juryAutonomyLevel: 'propose',
  juryT1Threshold: 0.75,
};

export type SettingsTab = 'general' | 'asr' | 'audio' | 'export' | 'models' | 'ai' | 'jury';

export const settings = writable<AppSettings>(defaultSettings);
export const showSettings = writable(false);
export const settingsTab = writable<SettingsTab>('general');

export function openSettings(tab: SettingsTab = 'general'): void {
  settingsTab.set(tab);
  showSettings.set(true);
}

// ---------------------------------------------------------------------------
// Theme application — keeps the <html> theme class in sync with the setting,
// so the design-system tokens (and Tailwind's `dark:` variants) switch live.
// 'system' follows the OS preference and updates when the OS changes.
// ---------------------------------------------------------------------------
export type ResolvedTheme = 'dark' | 'light';

function resolveTheme(theme: Theme): ResolvedTheme {
  if (theme === 'system') {
    return typeof window !== 'undefined' &&
      window.matchMedia('(prefers-color-scheme: light)').matches
      ? 'light'
      : 'dark';
  }
  return theme;
}

export function applyTheme(theme: Theme): void {
  if (typeof document === 'undefined') return;
  const resolved = resolveTheme(theme);
  const root = document.documentElement;
  root.classList.toggle('dark', resolved === 'dark');
  root.classList.toggle('light', resolved === 'light');
  root.style.colorScheme = resolved;
}

let activeTheme: Theme = defaultSettings.theme;
settings.subscribe((s) => {
  activeTheme = s.theme;
  applyTheme(s.theme);
});

if (typeof window !== 'undefined' && window.matchMedia) {
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (activeTheme === 'system') applyTheme('system');
  });
}
