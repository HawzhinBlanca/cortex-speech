import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import {
  describeRejection,
  isUncaughtScriptError,
  notifyUnhandledRejection,
} from '../../src/lib/globalErrorTrap';
import { notifications } from '../../src/lib/stores/notificationStore';

describe('globalErrorTrap (P2.2 / audit F3)', () => {
  beforeEach(() => notifications.clear());

  it('describeRejection extracts a readable message from any reason shape', () => {
    expect(describeRejection(new Error('boom'))).toBe('boom');
    expect(describeRejection('a string reason')).toBe('a string reason');
    expect(describeRejection({ code: 42 })).toBe('{"code":42}');
    expect(describeRejection(null)).toBe('Unknown error');
    expect(describeRejection(undefined)).toBe('Unknown error');
    // An Error with no message falls back to its name, never an empty toast.
    expect(describeRejection(new TypeError())).toBe('TypeError');
  });

  it('never throws for hostile coercion, circular data, or oversized values', () => {
    const hostile = {
      toJSON: () => {
        throw new Error('no JSON');
      },
      toString: () => {
        throw new Error('no string');
      },
      [Symbol.toPrimitive]: () => {
        throw new Error('no primitive');
      },
    };
    const circular: Record<string, unknown> = {};
    circular.self = circular;

    expect(() => describeRejection(hostile)).not.toThrow();
    expect(describeRejection(hostile)).toBe('Unknown error');
    expect(() => describeRejection(circular)).not.toThrow();
    expect(describeRejection('x'.repeat(3_000))).toHaveLength(2_000);
  });

  it('surfaces a fire-and-forget rejection as an error toast so it never vanishes', () => {
    notifyUnhandledRejection(new Error('invoke failed: database is locked'));
    const list = get(notifications);
    expect(list).toHaveLength(1);
    expect(list[0].type).toBe('error');
    // The real backend message is carried in the detail so the failure is diagnosable, not silent.
    expect(list[0].detail).toBe('invoke failed: database is locked');
  });

  /**
   * Uncaught SYNCHRONOUS errors moved here from ErrorBoundary on 2026-08-17, because a per-instance
   * window listener made every boundary in the tree fire at once. The two exclusions below are the
   * incidents that shaped it and must keep holding.
   */
  it('classifies window errors: real script errors yes, resource + ResizeObserver noise no', () => {
    const scriptError = new ErrorEvent('error', { message: 'x is not a function' });
    expect(isUncaughtScriptError(scriptError)).toBe(true);

    // A CSP-blocked <link>/<img>/<audio> fires 'error' at the ELEMENT. Once blanked the whole UI.
    const link = document.createElement('link');
    document.body.appendChild(link);
    const resourceError = new ErrorEvent('error', { message: '' });
    link.dispatchEvent(resourceError);
    expect(isUncaughtScriptError(resourceError)).toBe(false);
    link.remove();

    // Browser layout bookkeeping, not a failure.
    expect(
      isUncaughtScriptError(
        new ErrorEvent('error', {
          message: 'ResizeObserver loop completed with undelivered notifications.',
        }),
      ),
    ).toBe(false);
  });
});
