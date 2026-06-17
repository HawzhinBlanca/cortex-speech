import type { WordTimestamp } from './types';

export interface SegmentSourceMeta {
  sourceStartMs: number;
  sourceEndMs: number;
  chunkIndex: number;
  chunkCount: number;
}

function snakeToCamel(obj: Record<string, unknown>): SegmentSourceMeta | null {
  if (typeof obj.source_start_ms !== 'number' || typeof obj.source_end_ms !== 'number') {
    return null;
  }
  return {
    sourceStartMs: obj.source_start_ms as number,
    sourceEndMs: obj.source_end_ms as number,
    chunkIndex: (obj.chunk_index as number) ?? 0,
    chunkCount: (obj.chunk_count as number) ?? 1,
  };
}

export function parseSourceMeta(json: string | null | undefined): SegmentSourceMeta | null {
  if (!json) return null;
  try {
    const parsed = JSON.parse(json);
    if (Array.isArray(parsed)) return null;
    return snakeToCamel(parsed as Record<string, unknown>);
  } catch {
    return null;
  }
}

export function parseWordTimestamps(json: string | null | undefined): WordTimestamp[] {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json);
    if (Array.isArray(parsed)) return parsed as WordTimestamp[];
    if (parsed && Array.isArray(parsed.words)) return parsed.words as WordTimestamp[];
    return [];
  } catch {
    return [];
  }
}

export function mergeWordTimestamps(
  existingJson: string | null | undefined,
  words: WordTimestamp[],
): string {
  let base: Record<string, unknown> = {};
  if (existingJson) {
    try {
      const parsed = JSON.parse(existingJson);
      if (parsed && !Array.isArray(parsed) && typeof parsed === 'object') {
        base = { ...parsed };
      }
    } catch {
      /* keep empty base */
    }
  }
  return JSON.stringify({ ...base, words });
}

export function chunkPlaybackRange(meta: SegmentSourceMeta | null): {
  startTime: number;
  endTime: number;
} {
  if (!meta) return { startTime: 0, endTime: 0 };
  return {
    startTime: meta.sourceStartMs / 1000,
    endTime: meta.sourceEndMs / 1000,
  };
}

export function segmentSourceFilename(audioPath: string): string {
  return audioPath.split(/[/\\]/).pop() ?? audioPath;
}

export function truncateFilename(name: string, maxLen = 22): string {
  if (name.length <= maxLen) return name;
  const dot = name.lastIndexOf('.');
  const ext = dot > 0 ? name.slice(dot) : '';
  const base = dot > 0 ? name.slice(0, dot) : name;
  const keep = maxLen - ext.length - 1;
  if (keep < 4) return `${name.slice(0, maxLen - 1)}…`;
  return `${base.slice(0, keep)}…${ext}`;
}

export function segmentChunkLabel(json: string | null | undefined): string | null {
  const meta = parseSourceMeta(json);
  if (!meta || meta.chunkCount <= 1) return null;
  return `${meta.chunkIndex + 1}/${meta.chunkCount}`;
}
