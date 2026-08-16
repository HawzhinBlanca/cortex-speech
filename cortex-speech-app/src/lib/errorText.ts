const DEFAULT_MAX_LENGTH = 2_000;

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
