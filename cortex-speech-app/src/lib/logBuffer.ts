export const DEFAULT_MAX_LOG_LINES = 1000;

export function appendBoundedLogLine(
  lines: readonly string[],
  line: string,
  maxLines = DEFAULT_MAX_LOG_LINES,
): string[] {
  if (maxLines <= 0) return [];

  const next = [...lines, line];
  if (next.length <= maxLines) return next;
  return next.slice(next.length - maxLines);
}
