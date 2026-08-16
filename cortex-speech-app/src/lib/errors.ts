import { get } from 'svelte/store';
import { openSettings } from './stores/settingsStore';
import { t } from './i18n';
import { formatUnknownError } from './errorText';

export type ErrorAction = {
  label: string;
  handler: () => void;
};

export type ActionableError = {
  message: string;
  detail?: string;
  action?: ErrorAction;
};

const MODEL_PATTERNS = [
  /missing models?/i,
  /model not found/i,
  /asr unavailable/i,
  /asr model not loaded/i,
  /onnx runtime/i,
];

export function isModelError(message: string): boolean {
  return MODEL_PATTERNS.some((pattern) => pattern.test(message));
}

function openModelsSettings(): void {
  openSettings('models');
}

export function parseActionableError(error: unknown): ActionableError {
  // Never surface the literal string "undefined"/"null" to the user: a nullish error (e.g. a
  // resource event with no message) must degrade to a readable fallback, not String(undefined).
  const raw = formatUnknownError(error);

  if (isModelError(raw)) {
    return {
      message: get(t)('errors.modelMissing'),
      detail: raw,
      action: {
        label: get(t)('errors.openModelsSettings'),
        handler: openModelsSettings,
      },
    };
  }

  if (/import failed|failed to import/i.test(raw)) {
    return {
      message: get(t)('importFailed'),
      detail: raw,
    };
  }

  if (/transcription failed|transcribe/i.test(raw)) {
    return {
      message: get(t)('errors.transcriptionFailed'),
      detail: raw,
      action: isModelError(raw)
        ? { label: get(t)('errors.openModelsSettings'), handler: openModelsSettings }
        : undefined,
    };
  }

  return { message: raw, detail: raw };
}
