const DEFAULT_MAX_LENGTH = 2_000;
const PUBLIC_REFERENCE_MAX_LENGTH = 196;
const PUBLIC_CODE = /^[A-Z][A-Z0-9_]{0,63}$/;
const PUBLIC_OPERATION_ID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export type PublicSuggestedAction = 'retry' | 'openHealth' | 'openModels' | 'reloadClip';

export interface PublicErrorReference {
  code?: string;
  operationId?: string;
  retryable?: boolean;
  suggestedAction?: PublicSuggestedAction;
}

function bounded(text: string, fallback: string, maxLength: number): string {
  const normalized = text.trim() || fallback;
  if (normalized.length <= maxLength) return normalized;
  return `${normalized.slice(0, Math.max(0, maxLength - 1))}…`;
}

/**
 * Convert an arbitrary thrown value into bounded display text without ever throwing itself.
 * Error paths must remain dependable even for proxies, hostile coercion hooks, circular data,
 * or unexpectedly large backend payloads.
 */
export function formatUnknownError(
  value: unknown,
  fallback = 'Unknown error',
  maxLength = DEFAULT_MAX_LENGTH,
): string {
  const safeLimit = Number.isFinite(maxLength)
    ? Math.max(1, Math.floor(maxLength))
    : DEFAULT_MAX_LENGTH;

  if (typeof value === 'string') return bounded(value, fallback, safeLimit);
  if (value === null || value === undefined) return bounded(fallback, 'Unknown error', safeLimit);

  try {
    if (value instanceof Error) {
      const message = typeof value.message === 'string' ? value.message : '';
      const name = typeof value.name === 'string' ? value.name : '';
      return bounded(message || name, fallback, safeLimit);
    }
  } catch {
    // A Proxy can throw during instanceof or property access. Continue to safer fallbacks.
  }

  if (typeof value === 'object') {
    try {
      const json = JSON.stringify(value);
      if (typeof json === 'string') return bounded(json, fallback, safeLimit);
    } catch {
      // Circular values and throwing toJSON hooks are handled by the coercion fallback below.
    }
  }

  try {
    return bounded(String(value), fallback, safeLimit);
  } catch {
    return bounded(fallback, 'Unknown error', safeLimit);
  }
}

function readProperty(value: object, key: string): unknown {
  try {
    return (value as Record<string, unknown>)[key];
  } catch {
    return undefined;
  }
}

function parseStructuredError(value: unknown): object | null {
  if (value !== null && typeof value === 'object') return value;
  if (typeof value !== 'string') return null;

  const candidate = value.trim();
  if (candidate.length < 2 || candidate.length > 4_096 || candidate[0] !== '{') return null;
  try {
    const parsed: unknown = JSON.parse(candidate);
    return parsed !== null && typeof parsed === 'object' ? parsed : null;
  } catch {
    return null;
  }
}

function safeCode(value: unknown): string | undefined {
  return typeof value === 'string' && PUBLIC_CODE.test(value) ? value : undefined;
}

function safeOperationId(value: unknown): string | undefined {
  return typeof value === 'string' && PUBLIC_OPERATION_ID.test(value) ? value : undefined;
}

function safeSuggestedAction(value: unknown): PublicSuggestedAction | undefined {
  return value === 'retry' ||
    value === 'openHealth' ||
    value === 'openModels' ||
    value === 'reloadClip'
    ? value
    : undefined;
}

/**
 * Extract only bounded, protocol-defined identifiers from an arbitrary failure.
 *
 * Backend prose is intentionally excluded: messages may contain SQL, stack traces, absolute paths,
 * secrets, or text in the wrong locale. Normal user surfaces should pair this reference with a
 * localized explanation. Explicit technical consoles may continue to use `formatUnknownError`.
 */
export function publicErrorReference(value: unknown): PublicErrorReference {
  const structured = parseStructuredError(value);
  if (structured && readProperty(structured, 'schema') === 1) {
    const code = safeCode(readProperty(structured, 'code'));
    const operationId = safeOperationId(readProperty(structured, 'operationId'));
    const retryableValue = readProperty(structured, 'retryable');
    const suggestedAction = safeSuggestedAction(readProperty(structured, 'suggestedAction'));
    return {
      ...(code ? { code } : {}),
      ...(operationId ? { operationId } : {}),
      ...(typeof retryableValue === 'boolean' ? { retryable: retryableValue } : {}),
      ...(suggestedAction ? { suggestedAction } : {}),
    };
  }

  // Some legacy commands still reject with an `E_*` token. Tauri/browser bridges may preserve it
  // as a string or wrap it in an Error-like `message`; inspect through the hostile-safe accessor and
  // preserve only the closed identifier, never surrounding prose.
  const legacyMessage = structured ? readProperty(structured, 'message') : undefined;
  const legacyText =
    typeof value === 'string'
      ? value
      : typeof legacyMessage === 'string'
        ? legacyMessage
        : undefined;
  if (legacyText) {
    const match = /(?:^|\s)(E_[A-Z0-9_]{1,61})(?=\s|:|\.|,|;|$)/u.exec(legacyText.trim());
    const code = safeCode(match?.[1]);
    return code ? { code } : {};
  }

  return {};
}

/** A bidi-safe caller can render this ASCII-only reference after its localized failure message. */
export function formatPublicErrorReference(
  value: unknown,
  maxLength = PUBLIC_REFERENCE_MAX_LENGTH,
): string | undefined {
  const reference = publicErrorReference(value);
  const parts = [reference.code, reference.operationId].filter(
    (part): part is string => typeof part === 'string',
  );
  if (!parts.length) return undefined;

  const safeLimit = Number.isFinite(maxLength)
    ? Math.max(1, Math.min(PUBLIC_REFERENCE_MAX_LENGTH, Math.floor(maxLength)))
    : PUBLIC_REFERENCE_MAX_LENGTH;
  return bounded(parts.join(' · '), '', safeLimit);
}
