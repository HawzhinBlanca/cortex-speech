import type { DiffChange, DiffResult } from './types';
import type { CommandErrorV1 } from '../generated/ipc';

const MAX_WORDS = 10_000;
const MAX_LCS_CELLS = 12_500_000;

function buildLcsTable(a: string[], b: string[]): number[][] | null {
  const m = a.length;
  const n = b.length;
  if (m * n > MAX_LCS_CELLS) {
    return null;
  }

  const dp = Array.from({ length: m + 1 }, () => new Array<number>(n + 1).fill(0));
  for (let i = 1; i <= m; i += 1) {
    for (let j = 1; j <= n; j += 1) {
      dp[i][j] =
        a[i - 1] === b[j - 1] ? dp[i - 1][j - 1] + 1 : Math.max(dp[i - 1][j], dp[i][j - 1]);
    }
  }
  return dp;
}

function extractLcs(a: string[], b: string[], dp: number[][]): string[] {
  const result: string[] = [];
  let i = a.length;
  let j = b.length;

  while (i > 0 && j > 0) {
    if (a[i - 1] === b[j - 1]) {
      result.unshift(a[i - 1]);
      i -= 1;
      j -= 1;
    } else if (dp[i - 1][j] > dp[i][j - 1]) {
      i -= 1;
    } else {
      j -= 1;
    }
  }

  return result;
}

function localDiffRefusal(code: 'DIFF_TOO_LARGE' | 'DIFF_TOO_COMPLEX'): CommandErrorV1 {
  return {
    schema: 1,
    code,
    message:
      code === 'DIFF_TOO_LARGE'
        ? 'The transcript comparison is too large to process safely.'
        : 'The transcript comparison would require too much memory.',
    retryable: false,
    suggestedAction: null,
    operationId: null,
  };
}

function statsFor(changes: DiffChange[]): DiffResult['stats'] {
  let added = 0;
  let removed = 0;
  let changed = 0;
  let unchanged = 0;

  for (const change of changes) {
    switch (change.op) {
      case 'Equal':
        unchanged += 1;
        break;
      case 'Insert':
        added += 1;
        break;
      case 'Delete':
        removed += 1;
        break;
      case 'Replace':
        changed += 1;
        break;
    }
  }

  const total = added + removed + changed + unchanged;
  return {
    added_words: added,
    removed_words: removed,
    changed_words: changed,
    unchanged_words: unchanged,
    similarity: total === 0 ? 100 : (unchanged / total) * 100,
  };
}

export function computeLocalDiff(raw: string, annotated: string): DiffResult {
  const rawWords = raw.split(/\s+/).filter(Boolean);
  const annotatedWords = annotated.split(/\s+/).filter(Boolean);

  if (rawWords.length > MAX_WORDS || annotatedWords.length > MAX_WORDS) {
    throw localDiffRefusal('DIFF_TOO_LARGE');
  }

  const dp = buildLcsTable(rawWords, annotatedWords);
  if (!dp) {
    throw localDiffRefusal('DIFF_TOO_COMPLEX');
  }

  const lcs = extractLcs(rawWords, annotatedWords, dp);
  const changes: DiffChange[] = [];
  let rawIndex = 0;
  let annotatedIndex = 0;
  let lcsIndex = 0;

  while (rawIndex < rawWords.length || annotatedIndex < annotatedWords.length) {
    // A side "is at the LCS" when its current word equals the next common word. That word MUST be
    // emitted as Equal and never consumed into a Replace/Delete/Insert, or the remainder misaligns.
    // (Mirrors the Rust `diff::compute_diff`.)
    const rawIsLcs =
      rawIndex < rawWords.length && lcsIndex < lcs.length && rawWords[rawIndex] === lcs[lcsIndex];
    const annIsLcs =
      annotatedIndex < annotatedWords.length &&
      lcsIndex < lcs.length &&
      annotatedWords[annotatedIndex] === lcs[lcsIndex];

    // Both sides sit on the next common word \u2192 Equal.
    if (rawIsLcs && annIsLcs) {
      changes.push({ op: 'Equal', value: lcs[lcsIndex] });
      rawIndex += 1;
      annotatedIndex += 1;
      lcsIndex += 1;
      continue;
    }

    // Replace ONLY when BOTH words diverge from the LCS (a genuine substitution). Replacing while one
    // side is still on its common word would consume that common word and cascade wrong ops \u2014 the bug
    // where an insert/delete next to an unchanged word rendered a spurious "x \u2192 y" and undercounted
    // similarity (e.g. "a c" \u2192 "a b c" scored 33% with a bogus c\u2192b replace instead of 67%).
    if (
      rawIndex < rawWords.length &&
      annotatedIndex < annotatedWords.length &&
      !rawIsLcs &&
      !annIsLcs
    ) {
      changes.push({
        op: 'Replace',
        value: `${rawWords[rawIndex]} \u2192 ${annotatedWords[annotatedIndex]}`,
      });
      rawIndex += 1;
      annotatedIndex += 1;
      continue;
    }

    // A raw word that is not the next common word \u2192 Delete; the annotated side waits at its common word.
    if (rawIndex < rawWords.length && !rawIsLcs) {
      changes.push({ op: 'Delete', value: rawWords[rawIndex] });
      rawIndex += 1;
      continue;
    }

    // An annotated word that is not the next common word \u2192 Insert (raw waits); also drains the
    // annotated tail once raw is exhausted.
    if (annotatedIndex < annotatedWords.length) {
      changes.push({ op: 'Insert', value: annotatedWords[annotatedIndex] });
      annotatedIndex += 1;
      continue;
    }

    // Only raw remains, on a common word with no annotated partner left \u2192 Delete it (loop backstop).
    if (rawIndex < rawWords.length) {
      changes.push({ op: 'Delete', value: rawWords[rawIndex] });
      rawIndex += 1;
    }
  }

  return {
    raw,
    annotated,
    changes,
    stats: statsFor(changes),
  };
}
