import type { WordTimestamp } from './types';

export interface SegmentSourceMeta {
  sourceStartMs: number;
  sourceEndMs: number;
  chunkIndex: number;
  chunkCount: number;
}

function snakeToCamel(obj: Record<string, unknown>): SegmentSourceMeta | null {
  const start = obj.source_start_ms;
  const end = obj.source_end_ms;
  const index = obj.chunk_index ?? 0;
  const count = obj.chunk_count ?? 1;
  if (
    typeof start !== 'number' ||
    !Number.isFinite(start) ||
    start < 0 ||
    typeof end !== 'number' ||
    !Number.isFinite(end) ||
    end <= start ||
    typeof index !== 'number' ||
    !Number.isInteger(index) ||
    index < 0 ||
    typeof count !== 'number' ||
    !Number.isInteger(count) ||
    count < 1 ||
    index >= count
  ) {
    return null;
  }
  return {
    sourceStartMs: start,
    sourceEndMs: end,
    chunkIndex: index,
    chunkCount: count,
  };
}

function validWords(value: unknown): WordTimestamp[] | null {
  if (!Array.isArray(value)) return null;
  let previousStart = 0;
  for (let index = 0; index < value.length; index += 1) {
    const item = value[index] as Partial<WordTimestamp> | null;
    if (
      !item ||
      typeof item !== 'object' ||
      typeof item.word !== 'string' ||
      item.word.trim().length === 0 ||
      typeof item.start !== 'number' ||
      !Number.isFinite(item.start) ||
      item.start < 0 ||
      typeof item.end !== 'number' ||
      !Number.isFinite(item.end) ||
      item.end < item.start ||
      typeof item.confidence !== 'number' ||
      !Number.isFinite(item.confidence) ||
      item.confidence < 0 ||
      item.confidence > 1 ||
      (index > 0 && item.start < previousStart)
    ) {
      return null;
    }
    previousStart = item.start;
  }
  return value as WordTimestamp[];
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
    if (Array.isArray(parsed)) return validWords(parsed) ?? [];
    if (parsed && typeof parsed === 'object') return validWords(parsed.words) ?? [];
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
