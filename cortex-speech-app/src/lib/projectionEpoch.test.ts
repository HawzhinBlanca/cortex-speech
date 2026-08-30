import { describe, expect, it } from 'vitest';
import { ProjectionEpoch } from './projectionEpoch';

describe('projection epoch receipts', () => {
  it('lets a successful newer epoch supersede a hung predecessor and rejects its late finish', () => {
    const projection = new ProjectionEpoch();
    expect(projection.receipt()).toBe(0);

    const first = projection.begin();
    expect(projection.receipt()).toBeNull();

    const second = projection.begin();
    expect(projection.isLatest(first)).toBe(false);
    expect(projection.isLatest(second)).toBe(true);
    expect(projection.receipt()).toBeNull();

    expect(projection.settle(second, true)).toBe(second);
    expect(projection.receipt()).toBe(second);

    expect(projection.settle(first, true)).toBeNull();
    expect(projection.receipt()).toBe(second);
    expect(projection.isLatest(first)).toBe(false);
  });

  it('never issues a receipt for the latest failed epoch', () => {
    const projection = new ProjectionEpoch();
    const failed = projection.begin();

    expect(projection.settle(failed, false)).toBeNull();
    expect(projection.receipt()).toBeNull();

    const recovered = projection.begin();
    expect(projection.settle(recovered, true)).toBe(recovered);
    expect(projection.receipt()).toBe(recovered);
  });
});
