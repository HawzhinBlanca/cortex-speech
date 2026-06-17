export type DiffOp = 'Equal' | 'Insert' | 'Delete' | 'Replace';

export interface DiffChange {
  op: DiffOp;
  value: string;
}

export interface DiffStats {
  added_words: number;
  removed_words: number;
  changed_words: number;
  unchanged_words: number;
  similarity: number;
}

export interface DiffResult {
  raw: string;
  annotated: string;
  changes: DiffChange[];
  stats: DiffStats;
}
