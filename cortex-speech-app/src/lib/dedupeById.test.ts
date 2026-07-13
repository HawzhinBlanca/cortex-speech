import { describe, it, expect } from 'vitest';
import { dedupeById } from './dedupeById';

describe('dedupeById', () => {
  it('drops later duplicates, keeping the first occurrence', () => {
    const a = { id: 'x', v: 1 };
    const b = { id: 'y', v: 2 };
    const aDup = { id: 'x', v: 99 };
    expect(dedupeById([a, b, aDup])).toEqual([a, b]);
  });

  it('preserves order', () => {
    const items = [{ id: 'c' }, { id: 'a' }, { id: 'b' }, { id: 'a' }];
    expect(dedupeById(items).map((i) => i.id)).toEqual(['c', 'a', 'b']);
  });

  it('is a no-op on an already-unique list (same length, same order)', () => {
    const items = [{ id: '1' }, { id: '2' }, { id: '3' }];
    expect(dedupeById(items)).toEqual(items);
  });

  it('handles an empty list', () => {
    expect(dedupeById([])).toEqual([]);
  });

  it('collapses an all-same-id list to one row (the crash case)', () => {
    const dup = [{ id: 'same' }, { id: 'same' }, { id: 'same' }];
    expect(dedupeById(dup)).toEqual([{ id: 'same' }]);
  });
});
