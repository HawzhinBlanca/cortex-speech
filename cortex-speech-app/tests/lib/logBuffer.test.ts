import { describe, expect, it } from 'vitest';
import { appendBoundedLogLine, DEFAULT_MAX_LOG_LINES } from '../../src/lib/logBuffer';

describe('appendBoundedLogLine', () => {
  it('appends below the configured limit', () => {
    expect(appendBoundedLogLine(['a'], 'b', 3)).toEqual(['a', 'b']);
  });

  it('keeps the newest lines when the limit is exceeded', () => {
    expect(appendBoundedLogLine(['a', 'b', 'c'], 'd', 3)).toEqual(['b', 'c', 'd']);
  });

  it('drops all lines when the limit is zero or negative', () => {
    expect(appendBoundedLogLine(['a'], 'b', 0)).toEqual([]);
    expect(appendBoundedLogLine(['a'], 'b', -1)).toEqual([]);
  });

  it('defaults to a finite log history', () => {
    const lines = Array.from({ length: DEFAULT_MAX_LOG_LINES }, (_, i) => String(i));

    const next = appendBoundedLogLine(lines, 'newest');

    expect(next).toHaveLength(DEFAULT_MAX_LOG_LINES);
    expect(next[0]).toBe('1');
    expect(next.at(-1)).toBe('newest');
  });
});
