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
   * (`updateSegmentFields`), so ONLY the user-edited fields are persisted and a stale store row can
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
}

export function createAutosaveController<T extends object>(
  deps: AutosaveDeps<T>,
): AutosaveController {
  const debounceMs = deps.debounceMs ?? 1000;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: { id: string; fields: Record<string, unknown> } | null = null;

  // Re-read the FRESH row so a concurrent change to OTHER fields (a verify/normalize/background
  // reload during the debounce) is preserved, then re-apply the user's edited fields so their edit
  // always wins. The save callback ALSO receives the raw fields + id so the app can persist only the
  // edited fields (partial-update IPC — the store row itself may be stale during a long batch, so the
  // merged row is advisory, never the persistence source of truth). Returns the in-flight save
  // promise, or null (no save issued) when the row no longer exists — callers that only need "was a
  // save issued?" can still test truthiness (null is falsy).
  function persist(entry: { id: string; fields: Record<string, unknown> }): Promise<void> | null {
    const fresh = deps.getRow(entry.id);
    if (!fresh) return null;
    const merged = { ...fresh, ...entry.fields } as T;
    return deps.save(merged, entry.fields, entry.id).then(
      () => {
        if (pending === entry) pending = null;
        deps.onState?.('saved');
      },
      (error) => {
        deps.onError?.(error);
        deps.onState?.('idle');
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
    void flushAsync();
  }

  function flushAsync(): Promise<void> {
    clearTimer();
    const entry = pending;
    pending = null;
    if (!entry) return Promise.resolve();
    return persist(entry) ?? Promise.resolve();
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
    if (!pending || pending.id !== id) pending = { id, fields: {} };
    Object.assign(pending.fields, edits);
    const entry = pending;
    timer = setTimeout(() => {
      timer = null;
      if (!persist(entry)) deps.onState?.('idle');
    }, debounceMs);
  }

  function cancel() {
    clearTimer();
    pending = null;
  }

  function pendingId(): string | null {
    return pending ? pending.id : null;
  }

  return { schedule, flush, flushAsync, cancel, pendingId };
}
