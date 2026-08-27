// Debounced auto-save controller for transcript/speaker curation edits.
//
// Why this exists as a standalone, testable unit: the previous inline implementation in App.svelte
// shared ONE pending-save slot and ONE debounce timer for the whole app. Switching to (and editing)
// a different segment within the 1s debounce silently dropped the prior segment's queued DB write —
// it only survived in the in-memory store and was lost on the next `segments.load()` (undo/redo,
// batch completion, import, or app restart). For a tool whose entire value is human-verified labels,
// that is silent dataset corruption. The fix: before re-keying the debounce to a new segment, FLUSH
// the previous segment's queued edit instead of discarding it.

export type SaveState = 'idle' | 'saving' | 'saved';

export interface AutosaveDeps<T extends object> {
  /** id of the segment currently being edited, or null. Read FRESH at schedule/flush time. */
  targetId: () => string | null;
  /** Freshest row for an id (from the store), or null if it no longer exists. */
  getRow: (id: string) => T | null;
  /**
   * Persist the queued edit. MUST be idempotent: if the debounce timer's save is still in-flight when
   * `flush`/`flushAsync` runs (e.g. the window closes the instant after the timer fired), `pending` is
   * not cleared until that save's `.then`, so the SAME entry can be persisted a SECOND time. That is
   * harmless for an idempotent field write, but a side-effectful save — recording a human decision,
   * crediting LOOP-0 confidence, appending a ledger row — would DOUBLE-COUNT. Keep side effects out of
   * this callback; do them on the explicit verify path.
   *
   * Receives the merged row (fresh store row + edits, for callers that persist whole rows), plus the
   * raw edited `fields` and the segment `id` — the app wires these to the partial-update IPC
   * (`updateSegmentMetadataV1`), so ONLY the user-edited fields are persisted and a stale store row can
   * never clobber concurrently-written columns (F10 root fix).
   */
  save: (row: T, fields: Record<string, unknown>, id: string) => Promise<unknown>;
  /** UI state transitions ('saving' on schedule, 'saved' on success, 'idle' on no-op/error). */
  onState?: (state: SaveState) => void;
  /** Save failure callback. */
  onError?: (error: unknown) => void;
  /** Debounce window in ms (default 1000). */
  debounceMs?: number;
}

export interface AutosaveController {
  /** Queue the given field edits for the currently-targeted segment, debounced. */
  schedule: (edits: Record<string, unknown>) => void;
  /** Persist any queued edit immediately (call when leaving a segment). */
  flush: () => void;
  /**
   * Flush and return a promise that resolves when the in-flight save settles (or immediately when
   * nothing is queued). Call this on window close so the last edit of a session is never lost to the
   * debounce — `flush()` is fire-and-forget and would let the window close before the write lands.
   */
  flushAsync: () => Promise<void>;
  /** Drop any queued edit without saving (teardown). */
  cancel: () => void;
  /**
   * The id of the segment a debounced save is currently queued for, or null. Callers use this to
   * scope a cancel to a specific segment (e.g. drop the pending edit only when THIS segment is about
   * to be deleted or re-transcribed), so an unrelated segment's queued edit is never lost.
   */
  pendingId: () => string | null;
  /** IDs whose hydrated compare-and-set baselines must remain resident until save/retry settles. */
  retainedIds: () => string[];
}

/** Flush only when a destructive action intersects queued/retry work; failure leaves that work held. */
export async function flushAutosaveForIds(
  controller: AutosaveController,
  ids: Iterable<string>,
): Promise<boolean> {
  const targets = new Set(ids);
  if (!controller.retainedIds().some((id) => targets.has(id))) return true;
  try {
    await controller.flushAsync();
    return true;
  } catch {
    return false;
  }
}

export function createAutosaveController<T extends object>(
  deps: AutosaveDeps<T>,
): AutosaveController {
  type PendingSave = { id: string; fields: Record<string, unknown>; sequence: number };
  const debounceMs = deps.debounceMs ?? 1000;
  let nextSequence = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: PendingSave | null = null;
  const retryQueue = new Map<string, PendingSave>();

  function queueRetry(entry: PendingSave): void {
    const existing = retryQueue.get(entry.id);
    if (!existing || existing === entry) {
      retryQueue.set(entry.id, entry);
      return;
    }
    // Retain disjoint fields from both attempts and let the chronologically newer entry win for an
    // overlapping field. This race occurs when a failed in-flight save returns after the user has
    // revisited the same segment and queued a new entry.
    if (entry.sequence > existing.sequence) {
      entry.fields = { ...existing.fields, ...entry.fields };
      retryQueue.set(entry.id, entry);
    } else {
      existing.fields = { ...entry.fields, ...existing.fields };
    }
  }

  // Re-read the FRESH row so a concurrent change to OTHER fields (a verify/normalize/background
  // reload during the debounce) is preserved, then re-apply the user's edited fields so their edit
  // always wins. The save callback ALSO receives the raw fields + id so the app can persist only the
  // edited fields (partial-update IPC — the store row itself may be stale during a long batch, so the
  // merged row is advisory, never the persistence source of truth). Returns the in-flight save
  // promise, or null (no save issued) when the row no longer exists — callers that only need "was a
  // save issued?" can still test truthiness (null is falsy).
  function persist(entry: PendingSave): Promise<void> | null {
    const fresh = deps.getRow(entry.id);
    if (!fresh) return null;
    const merged = { ...fresh, ...entry.fields } as T;
    return deps.save(merged, entry.fields, entry.id).then(
      () => {
        // Only clear the queue when no NEW debounce is riding this same entry: a same-segment edit
        // scheduled while this save was in flight merged into `entry` and armed a fresh timer, and
        // nulling `pending` here would let the next re-key's `clearTimer()` drop that edit unsaved.
        if (pending === entry && timer === null) pending = null;
        if (retryQueue.get(entry.id) === entry) retryQueue.delete(entry.id);
        deps.onState?.('saved');
      },
      (error) => {
        queueRetry(entry);
        deps.onError?.(error);
        deps.onState?.('idle');
        throw error;
      },
    );
  }

  function clearTimer() {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function flush() {
    // `onError` already surfaced the failure; fire-and-forget callers cannot await it, so consume the
    // rejection here. `flushAsync` deliberately preserves it for close guards and explicit Save.
    void flushAsync().catch(() => undefined);
  }

  async function flushAsync(): Promise<void> {
    clearTimer();
    const entries = [...retryQueue.values()];
    if (pending && !entries.includes(pending)) entries.push(pending);
    pending = null;
    for (const entry of entries) queueRetry(entry);
    for (const entry of entries) {
      const saving = persist(entry);
      if (!saving) {
        if (retryQueue.get(entry.id) === entry) retryQueue.delete(entry.id);
        continue;
      }
      await saving;
    }
  }

  function schedule(edits: Record<string, unknown>) {
    deps.onState?.('saving');
    const id = deps.targetId();
    if (!id) {
      deps.onState?.('idle');
      return;
    }
    // Switching to a different segment mid-debounce must NOT drop the prior segment's queued edit:
    // persist it now before re-keying the timer to the new target.
    if (pending && pending.id !== id) flush();
    clearTimer();
    if (!pending || pending.id !== id) {
      pending = retryQueue.get(id) ?? { id, fields: {}, sequence: 0 };
    }
    pending.sequence = ++nextSequence;
    Object.assign(pending.fields, edits);
    const entry = pending;
    timer = setTimeout(() => {
      timer = null;
      const saving = persist(entry);
      if (!saving) deps.onState?.('idle');
      else void saving.catch(() => undefined);
    }, debounceMs);
  }

  function cancel() {
    clearTimer();
    const target = pending?.id ?? deps.targetId();
    if (target) retryQueue.delete(target);
    pending = null;
  }

  function pendingId(): string | null {
    if (pending) return pending.id;
    const target = deps.targetId();
    return target && retryQueue.has(target) ? target : null;
  }

  function retainedIds(): string[] {
    const ids = new Set(retryQueue.keys());
    if (pending) ids.add(pending.id);
    return [...ids];
  }

  return { schedule, flush, flushAsync, cancel, pendingId, retainedIds };
}
