import { get } from 'svelte/store';
import { openSettings } from './stores/settingsStore';
import { t } from './i18n';
import {
  formatPublicErrorReference,
  formatUnknownError,
  publicErrorReference,
  type PublicSuggestedAction,
} from './errorText';

export type ErrorAction = {
  label: string;
  handler: () => void;
};

export type ActionableError = {
  message: string;
  detail?: string;
  action?: ErrorAction;
  code?: string;
  operationId?: string;
  retryable?: boolean;
  suggestedAction?: PublicSuggestedAction;
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

export function parseActionableError(error: unknown, fallbackMessage?: string): ActionableError {
  // Never surface the literal string "undefined"/"null" to the user: a nullish error (e.g. a
  // resource event with no message) must degrade to a readable fallback, not String(undefined).
  const raw = formatUnknownError(error);
  const reference = publicErrorReference(error);
  const detail = formatPublicErrorReference(error);
  const metadata = {
    ...(detail ? { detail } : {}),
    ...(reference.code ? { code: reference.code } : {}),
    ...(reference.operationId ? { operationId: reference.operationId } : {}),
    ...(typeof reference.retryable === 'boolean' ? { retryable: reference.retryable } : {}),
    ...(reference.suggestedAction ? { suggestedAction: reference.suggestedAction } : {}),
  };

  if (isModelError(raw) || reference.suggestedAction === 'openModels') {
    return {
      message: get(t)('errors.modelMissing'),
      ...metadata,
      action: {
        label: get(t)('errors.openModelsSettings'),
        handler: openModelsSettings,
      },
    };
  }

  if (/import failed|failed to import/i.test(raw)) {
    return {
      message: get(t)('importFailed'),
      ...metadata,
    };
  }

  if (/transcription failed|transcribe/i.test(raw)) {
    return {
      message: get(t)('errors.transcriptionFailed'),
      ...metadata,
    };
  }

  return { message: fallbackMessage || get(t)('errors.unknown'), ...metadata };
}
