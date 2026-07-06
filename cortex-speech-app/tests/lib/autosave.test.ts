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
