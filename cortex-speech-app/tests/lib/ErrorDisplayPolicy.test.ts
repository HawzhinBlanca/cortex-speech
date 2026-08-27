import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const sourceRoot = join(process.cwd(), 'src');

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'generated') return [];
      return sourceFiles(path);
    }
    return /\.(?:svelte|ts)$/.test(entry.name) ? [path] : [];
  });
}

describe('frontend error-display policy', () => {
  it('routes caught values through the total bounded formatter', () => {
    const forbidden = [
      /\bString\s*\(\s*(?:e|err|error|cause)\s*\)/g,
      /\$\{\s*(?:e|err|error|cause)\s*\}/g,
    ];
    const violations: string[] = [];

    for (const file of sourceFiles(sourceRoot)) {
      // This is the one defensive adapter allowed to attempt String(value): the call is bounded and
      // wrapped in try/catch specifically so hostile coercion cannot break an error path.
      if (file.endsWith(`${join('lib', 'errorText.ts')}`)) continue;
      const source = readFileSync(file, 'utf8');
      for (const pattern of forbidden) {
        for (const match of source.matchAll(pattern)) {
          const line = source.slice(0, match.index).split('\n').length;
          violations.push(`${relative(sourceRoot, file)}:${line}: ${match[0]}`);
        }
      }
    }

    expect(violations, violations.join('\n')).toEqual([]);
  });

  it('keeps the raw formatter out of ordinary user-display sinks', () => {
    const rawAllowed = new Set([
      'Workstation.svelte', // cancellation classification only
      'lib/commands.ts', // legacy/champion error classification only
      'lib/errorText.ts', // the defensive formatter and public sanitizer
      'lib/errors.ts', // localized error classification only
      'lib/globalErrorTrap.ts', // exported diagnostic description; toast receives the original cause
      'lib/WslConsolePanel.svelte', // explicit technical console log
    ]);
    const violations: string[] = [];

    for (const file of sourceFiles(sourceRoot)) {
      const relativePath = relative(sourceRoot, file).replaceAll('\\', '/');
      const source = readFileSync(file, 'utf8');
      if (source.includes('formatUnknownError') && !rawAllowed.has(relativePath)) {
        violations.push(`${relativePath}: imports or calls formatUnknownError`);
      }
      for (const pattern of [
        /\b(?:errorMessage|loadError|loadMoreError|reviewLoadError|waveformError)\s*=\s*formatUnknownError\b/g,
        /\{\s*(?:err|error)\s*:\s*formatUnknownError\b/g,
      ]) {
        for (const match of source.matchAll(pattern)) {
          const line = source.slice(0, match.index).split('\n').length;
          violations.push(`${relativePath}:${line}: ${match[0].slice(0, 100)}`);
        }
      }
      for (const match of source.matchAll(/\bdetail\s*:/g)) {
        const prefix = source.slice(Math.max(0, (match.index ?? 0) - 500), match.index);
        const callStart = prefix.lastIndexOf('notifications.error(');
        const callEnd = prefix.lastIndexOf(');');
        if (callStart > callEnd) {
          const line = source.slice(0, match.index).split('\n').length;
          violations.push(`${relativePath}:${line}: error notification uses raw detail`);
        }
      }
    }

    expect(violations, violations.join('\n')).toEqual([]);
  });

  it('keeps the workstation labels localized and replaces text-symbol pseudo-icons', () => {
    const source = readFileSync(join(sourceRoot, 'Workstation.svelte'), 'utf8');
    for (const forbidden of [
      'Show segments (⇧S)',
      'Show stats (⇧D)',
      'Local 7B ASR (WSL)',
      'Review Inbox (Ctrl+Shift+R)',
      'Speaker Management',
      'Merge Dataset JSON',
      'ASR Confidence Score',
      '>✓<',
      '>✕<',
      "reviewCorrect.start')} →",
    ]) {
      expect(source, forbidden).not.toContain(forbidden);
    }
  });
});
