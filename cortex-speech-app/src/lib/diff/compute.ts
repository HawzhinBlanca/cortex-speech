import type { DiffChange, DiffResult } from './types';

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
      dp[i][j] = a[i - 1] === b[j - 1] ? dp[i - 1][j - 1] + 1 : Math.max(dp[i - 1][j], dp[i][j - 1]);
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

function emptyGuardResult(raw: string, annotated: string): DiffResult {
  return {
    raw,
    annotated,
    changes: [],
    stats: {
      added_words: 0,
      removed_words: 0,
      changed_words: 0,
      unchanged_words: 0,
      similarity: 100,
    },
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
    return emptyGuardResult(raw, annotated);
  }

  const dp = buildLcsTable(rawWords, annotatedWords);
  if (!dp) {
    return emptyGuardResult(raw, annotated);
  }

  const lcs = extractLcs(rawWords, annotatedWords, dp);
  const changes: DiffChange[] = [];
  let rawIndex = 0;
  let annotatedIndex = 0;
  let lcsIndex = 0;

  while (rawIndex < rawWords.length || annotatedIndex < annotatedWords.length) {
    if (
      rawIndex < rawWords.length &&
      annotatedIndex < annotatedWords.length &&
      lcsIndex < lcs.length &&
      rawWords[rawIndex] === lcs[lcsIndex] &&
      annotatedWords[annotatedIndex] === lcs[lcsIndex]
    ) {
      changes.push({ op: 'Equal', value: lcs[lcsIndex] });
      rawIndex += 1;
      annotatedIndex += 1;
      lcsIndex += 1;
      continue;
    }

    if (rawIndex < rawWords.length && annotatedIndex < annotatedWords.length) {
      changes.push({ op: 'Replace', value: `${rawWords[rawIndex]} \u2192 ${annotatedWords[annotatedIndex]}` });
      rawIndex += 1;
      annotatedIndex += 1;
      continue;
    }

    if (rawIndex < rawWords.length) {
      changes.push({ op: 'Delete', value: rawWords[rawIndex] });
      rawIndex += 1;
      continue;
    }

    if (annotatedIndex < annotatedWords.length) {
      changes.push({ op: 'Insert', value: annotatedWords[annotatedIndex] });
      annotatedIndex += 1;
    }
  }

  return {
    raw,
    annotated,
    changes,
    stats: statsFor(changes),
  };
}
