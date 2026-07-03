import { writable } from 'svelte/store';

export type Theme = 'dark' | 'light' | 'system';
export type ExportFormat = 'json' | 'jsonl' | 'csv' | 'parquet';
export type AsrModel = 'ctc-300m' | 'ctc-1b' | 'wsl-7b';

export interface AppSettings {
  theme: Theme;
  autoNormalize: boolean;
  verbalizeNumbers: boolean;
  autoAlign: boolean;
  exportFormat: ExportFormat;
  asrModel: AsrModel;
  /** P2.3: use the embedded fine-tuned MMS-CTC engine (best local Sorani) — overrides asrModel. */
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
  cloudSttOptIn: boolean;
  llmSystemPrompt: string;
  llmModel: string;
  externalAsrScriptPath: string;
  // Listening Jury settings
  juryCloudOptIn: boolean;
  juryModel: string;
  sourceReferenceModels: string[];
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
  // The fine-tuned OmniASR-7B Champion (WSL GPU path) is the owner's main model and the app default,
  // matching the Rust AppSettings default — a reset never silently reverts to the weaker base CTC.
  asrModel: 'wsl-7b',
  // P2.3: off by default here (mirrors the Rust default); the ASR-tab toggle enables the embedded
  // fine-tuned engine, which overrides asrModel when on. Backend default flip awaits the M1 decision.
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
  llmMode: 'Local',
  llmEndpoint: 'http://127.0.0.1:11434/v1/chat/completions',
  llmApiKey: '',
  llmApiKeyConfigured: false,
  cloudLlmOptIn: false,
  cloudSttOptIn: false,
  llmSystemPrompt:
    'You are an expert Kurdish linguist. Fix the phonetic transcription errors in the following text, preserving the exact meaning. Output ONLY the corrected text, no explanations.',
  llmModel: 'heretic-final:latest',
  externalAsrScriptPath: '',
  // Listening Jury defaults
  juryCloudOptIn: false,
  juryModel: 'gemini-2.5-pro',
  sourceReferenceModels: ['gemini-2.5-pro', 'gemini-2.5-flash'],
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
