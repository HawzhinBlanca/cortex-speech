import { describe, it, expect, vi } from 'vitest';
import { createAutosaveController } from '../../src/lib/autosave';

type Row = { id: string; text: string; speaker?: string };

function setup(initialRows: Row[]) {
  const rows = new Map<string, Row>(initialRows.map((r) => [r.id, { ...r }]));
  let target: string | null = initialRows[0]?.id ?? null;
  const saved: Row[] = [];
  const states: string[] = [];
  const errors: unknown[] = [];

  const ctrl = createAutosaveController<Row>({
    targetId: () => target,
    getRow: (id) => (rows.has(id) ? { ...(rows.get(id) as Row) } : null),
    save: async (row) => {
      saved.push({ ...row });
      rows.set(row.id, { ...row });
    },
    onState: (s) => states.push(s),
    onError: (e) => errors.push(e),
    debounceMs: 1000,
  });

  return {
    ctrl,
    saved,
    states,
    errors,
    rows,
    setTarget: (id: string | null) => {
      target = id;
    },
  };
}

describe('autosave controller', () => {
  it('saves the merged row after the debounce window', async () => {
    vi.useFakeTimers();
    const h = setup([{ id: 'A', text: 'orig', speaker: 's1' }]);

    h.ctrl.schedule({ text: 'user-edit' });
    // A concurrent change to a DIFFERENT field arrives before the timer fires.
    h.rows.set('A', { id: 'A', text: 'orig', speaker: 's2' });

    await vi.runAllTimersAsync();

    // The user's edited field wins; the concurrently-changed other field stays fresh.
    expect(h.saved).toEqual([{ id: 'A', text: 'user-edit', speaker: 's2' }]);
    vi.useRealTimers();
  });

  // F10 root fix: the save callback receives the raw edited FIELDS and the segment id, so the app can
  // persist ONLY the user's edits via compare-and-set IPC (updateSegmentMetadataV1) — the merged store
  // row may be stale during a long batch and must never be the persistence source of truth.
  it('passes the accumulated raw fields and the segment id to the save callback', async () => {
    vi.useFakeTimers();
    const calls: Array<{ fields: Record<string, unknown>; id: string }> = [];
    let target: string | null = 'A';
    const ctrl = createAutosaveController<Row>({
      targetId: () => target,
      getRow: () => ({ id: 'A', text: 'stale-store-text', speaker: 'stale' }),
      save: async (_row, fields, id) => {
        calls.push({ fields: { ...fields }, id });
      },
      debounceMs: 1000,
    });

    ctrl.schedule({ text: 'edit-1' });
    ctrl.schedule({ speaker: 'S2' }); // same segment: edits accumulate into ONE partial save
    await vi.runAllTimersAsync();

    expect(calls).toEqual([{ fields: { text: 'edit-1', speaker: 'S2' }, id: 'A' }]);
    // Only the edited fields ride to the backend — nothing from the (stale) store row.
    expect(Object.keys(calls[0].fields)).not.toContain('id');
    void target;
    vi.useRealTimers();
  });

  // The close-flush contract: flushAsync issues the queued save immediately and resolves only after
  // it settles, so a window-close handler can AWAIT it and never lose the last edit to the debounce.
  it('flushAsync resolves only after the queued save settles', async () => {
    let resolveSave: (() => void) | undefined;
    const saved: Row[] = [];
    const ctrl = createAutosaveController<Row>({
      targetId: () => 'A',
      getRow: () => ({ id: 'A', text: 'orig' }),
      save: (row) =>
        new Promise<void>((resolve) => {
          saved.push({ ...row });
          resolveSave = resolve;
        }),
      debounceMs: 1000,
    });
    ctrl.schedule({ text: 'last-edit' });
    // Flush BEFORE the debounce fires: it must issue the save now and return a still-pending promise.
    const flushed = ctrl.flushAsync();
    expect(saved).toEqual([{ id: 'A', text: 'last-edit' }]);
    let settled = false;
    void flushed.then(() => {
      settled = true;
    });
    await Promise.resolve();
    expect(settled).toBe(false);
    resolveSave?.();
    await flushed;
    expect(settled).toBe(true);
  });

  it('flushAsync resolves immediately when nothing is queued', async () => {
    const ctrl = createAutosaveController<Row>({
      targetId: () => 'A',
      getRow: () => ({ id: 'A', text: 'x' }),
      save: async () => {},
    });
    await expect(ctrl.flushAsync()).resolves.toBeUndefined();
  });

  it('flushAsync rejects and retains the edit until a later retry succeeds', async () => {
    let attempts = 0;
    const ctrl = createAutosaveController<Row>({
      targetId: () => 'A',
      getRow: () => ({ id: 'A', text: 'orig' }),
      save: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('stale metadata');
      },
    });
    ctrl.schedule({ text: 'must-survive' });

    await expect(ctrl.flushAsync()).rejects.toThrow('stale metadata');
    expect(ctrl.pendingId()).toBe('A');
    await expect(ctrl.flushAsync()).resolves.toBeUndefined();
    expect(attempts).toBe(2);
    expect(ctrl.pendingId()).toBeNull();
  });

  it('retains later-segment work when an earlier retry fails during a close flush', async () => {
    let target: string | null = 'A';
    const attempts: string[] = [];
    let failA = true;
    const ctrl = createAutosaveController<Row>({
      targetId: () => target,
      getRow: (id) => ({ id, text: 'orig' }),
      save: async (row) => {
        attempts.push(row.id);
        if (row.id === 'A' && failA) throw new Error('A unavailable');
      },
      debounceMs: 1000,
    });
    ctrl.schedule({ text: 'edit-a' });
    target = 'B';
    ctrl.schedule({ text: 'edit-b' });
    await Promise.resolve();

    await expect(ctrl.flushAsync()).rejects.toThrow('A unavailable');
    failA = false;
    await expect(ctrl.flushAsync()).resolves.toBeUndefined();
    expect(attempts.filter((id) => id === 'B')).toHaveLength(1);
  });

  // The core round-16 fix: switching to (and editing) a different segment within the debounce window
  // must FLUSH the prior segment's queued edit, not silently drop it.
  it('flushes the prior segment edit when switching segments mid-debounce', async () => {
    vi.useFakeTimers();
    const h = setup([
      { id: 'A', text: 'a' },
      { id: 'B', text: 'b' },
    ]);

    h.ctrl.schedule({ text: 'a-edited' }); // edit A
    h.setTarget('B'); // user clicks segment B
    h.ctrl.schedule({ text: 'b-edited' }); // and edits B, all within 1s

    await vi.runAllTimersAsync();

    const saved = h.saved.map((r) => `${r.id}:${r.text}`);
    expect(saved).toContain('A:a-edited'); // would be DROPPED by the old shared-slot debouncer
    expect(saved).toContain('B:b-edited');
    vi.useRealTimers();
  });

  it('flush() persists a queued edit immediately, without waiting for the debounce', async () => {
    vi.useFakeTimers();
    const h = setup([{ id: 'A', text: 'a' }]);

    h.ctrl.schedule({ text: 'edited' });
    h.ctrl.flush();
    await vi.runAllTimersAsync();

    expect(h.saved.map((r) => r.text)).toEqual(['edited']);
    vi.useRealTimers();
  });

  it('does not save when the target row no longer exists at fire time', async () => {
    vi.useFakeTimers();
    const h = setup([{ id: 'A', text: 'a' }]);

    h.ctrl.schedule({ text: 'edited' });
    h.rows.delete('A'); // row removed (e.g. deleted) before the timer fires

    await vi.runAllTimersAsync();

    expect(h.saved).toHaveLength(0);
    expect(h.states).toContain('idle');
    vi.useRealTimers();
  });

  it('is a no-op when there is no target segment', () => {
    const h = setup([]);
    h.setTarget(null);

    h.ctrl.schedule({ text: 'x' });

    expect(h.saved).toHaveLength(0);
  });

  it('cancel() drops a queued edit without saving', async () => {
    vi.useFakeTimers();
    const h = setup([{ id: 'A', text: 'a' }]);

    h.ctrl.schedule({ text: 'edited' });
    h.ctrl.cancel();
    await vi.runAllTimersAsync();

    expect(h.saved).toHaveLength(0);
    vi.useRealTimers();
  });

  // pendingId() is the guard Workstation.svelte call sites rely on to scope a cancel to one segment
  // before delete/re-transcribe — so a debounced flush can't resurrect a deleted row (via
  // update_segment's insert-on-conflict) or clobber a fresh machine transcript. A regression that
  // returns null while an edit is queued (or a stale id after cancel/flush/save) silently disables
  // every one of those guards while npm test stays green. Pin the whole contract.
  it('pendingId() tracks the queued segment through schedule, cancel, flush, re-key, and save', async () => {
    vi.useFakeTimers();
    const h = setup([
      { id: 's1', text: 'a' },
      { id: 's2', text: 'b' },
    ]);

    expect(h.ctrl.pendingId()).toBeNull(); // nothing queued yet

    h.ctrl.schedule({ text: 'edit-1' });
    expect(h.ctrl.pendingId()).toBe('s1'); // a queued edit reports its segment

    h.ctrl.cancel();
    expect(h.ctrl.pendingId()).toBeNull(); // cancel drops the queue — no stale id

    h.ctrl.schedule({ text: 'edit-2' });
    await h.ctrl.flushAsync();
    expect(h.ctrl.pendingId()).toBeNull(); // explicit flush clears the queue

    // The re-key case the delete guard depends on: switching targets mid-debounce must report the
    // NEW segment (the old edit was flushed, not still pending under the old id).
    h.ctrl.schedule({ text: 'edit-3' });
    h.setTarget('s2');
    h.ctrl.schedule({ text: 'edit-4' });
    expect(h.ctrl.pendingId()).toBe('s2');

    await vi.runAllTimersAsync();
    expect(h.ctrl.pendingId()).toBeNull(); // the debounced save landing clears the queue
    vi.useRealTimers();
  });

  it('retainedIds exposes every baseline required by pending and failed retry work', async () => {
    const rows = new Map([
      ['A', { id: 'A', text: 'old-a' }],
      ['B', { id: 'B', text: 'old-b' }],
    ]);
    let current = 'A';
    const save = vi
      .fn()
      .mockRejectedValueOnce(new Error('A unavailable'))
      .mockResolvedValue(undefined);
    const ctrl = createAutosaveController({
      targetId: () => current,
      getRow: (id) => rows.get(id) ?? null,
      save,
      debounceMs: 10_000,
    });

    ctrl.schedule({ text: 'new-a' });
    expect(ctrl.retainedIds()).toEqual(['A']);
    await expect(ctrl.flushAsync()).rejects.toThrow('A unavailable');
    current = 'B';
    ctrl.schedule({ text: 'new-b' });
    expect(new Set(ctrl.retainedIds())).toEqual(new Set(['A', 'B']));
    await ctrl.flushAsync();
    expect(ctrl.retainedIds()).toEqual([]);
  });

  // Latent until a second autosaved field existed, but the class is dataset loss: an edit typed while
  // the previous save was in flight merged into the SAME entry, then the in-flight save's `.then`
  // cleared `pending` — so the next segment switch took the "nothing pending" branch and its
  // clearTimer() dropped that edit unsaved. The queue is only clear once no timer rides the entry.
  it('keeps a same-segment edit queued when it is typed during an in-flight save', async () => {
    vi.useFakeTimers();
    const saved: Array<Record<string, unknown>> = [];
    let resolveSave!: () => void;
    let target: string | null = 'A';
    const ctrl = createAutosaveController<Row>({
      targetId: () => target,
      getRow: (id) => ({ id, text: 'orig' }),
      save: (_row, fields) =>
        new Promise<void>((resolve) => {
          saved.push({ ...fields });
          resolveSave = resolve;
        }),
      debounceMs: 1000,
    });

    ctrl.schedule({ text: 'edit-1' });
    await vi.advanceTimersByTimeAsync(1000); // the debounce fires: edit-1 is in flight
    ctrl.schedule({ text: 'edit-2' }); // same segment, typed while that save is still open
    resolveSave(); // edit-1 lands
    await vi.advanceTimersByTimeAsync(0); // let the save's .then run

    target = 'B'; // the user moves to another clip and edits it, re-keying the debounce
    ctrl.schedule({ text: 'b-edit' });
    await vi.runAllTimersAsync();

    const texts = saved.map((f) => f.text);
    expect(texts).toContain('edit-2'); // fail-before: dropped by the re-key's clearTimer()
    expect(texts).toContain('b-edit');
    vi.useRealTimers();
  });

  it('reports an error and returns to idle when the save fails', async () => {
    vi.useFakeTimers();
    const rows = new Map<string, Row>([['A', { id: 'A', text: 'a' }]]);
    const errors: unknown[] = [];
    const states: string[] = [];
    const ctrl = createAutosaveController<Row>({
      targetId: () => 'A',
      getRow: (id) => (rows.has(id) ? { ...(rows.get(id) as Row) } : null),
      save: async () => {
        throw new Error('db locked');
      },
      onState: (s) => states.push(s),
      onError: (e) => errors.push(e),
      debounceMs: 1000,
    });

    ctrl.schedule({ text: 'edited' });
    await vi.runAllTimersAsync();

    expect(errors).toHaveLength(1);
    expect(states[states.length - 1]).toBe('idle');
    vi.useRealTimers();
  });
});
