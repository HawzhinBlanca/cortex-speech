import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import {
  describeRejection,
  installGlobalErrorTrap,
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
    expect(list[0].message).toBeTruthy();
    expect(list[0].detail).toBeUndefined();
    expect(JSON.stringify(list[0])).not.toContain('database is locked');
  });

  it('retains a typed code/action/operation reference without its backend prose', () => {
    notifyUnhandledRejection({
      schema: 1,
      code: 'WRITE_FAILED',
      message: 'SQL error at C:\\private\\library.db',
      retryable: true,
      suggestedAction: 'retry',
      operationId: '018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10',
    });
    const notification = get(notifications)[0];
    expect(notification.detail).toBe('WRITE_FAILED · 018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10');
    expect(notification.suggestedAction).toBe('retry');
    expect(JSON.stringify(notification)).not.toMatch(/SQL|private|library\.db/);
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

  it('installs once and routes only actionable global failures through the privacy-safe toast path', () => {
    installGlobalErrorTrap();

    const rejection = new Event('unhandledrejection');
    Object.defineProperty(rejection, 'reason', {
      value: {
        schema: 1,
        code: 'BACKGROUND_WRITE_FAILED',
        message: 'SQL failed at C:\\private\\library.db',
        retryable: true,
        suggestedAction: 'retry',
        operationId: '018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10',
      },
    });
    window.dispatchEvent(rejection);

    let list = get(notifications);
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({
      type: 'error',
      detail: 'BACKGROUND_WRITE_FAILED · 018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10',
      suggestedAction: 'retry',
    });
    expect(JSON.stringify(list[0])).not.toMatch(/SQL|private|library\.db/);

    // Reinstall attempts are a no-op: one rejected operation must never produce duplicate alarms.
    installGlobalErrorTrap();
    const repeated = new Event('unhandledrejection');
    Object.defineProperty(repeated, 'reason', { value: new Error('second background failure') });
    window.dispatchEvent(repeated);
    expect(get(notifications)).toHaveLength(2);

    // Resource failures reach the capture listener but are not uncaught application exceptions.
    const audio = document.createElement('audio');
    document.body.appendChild(audio);
    audio.dispatchEvent(new ErrorEvent('error', { message: 'private missing audio path' }));
    audio.remove();
    expect(get(notifications)).toHaveLength(2);

    // Browser layout bookkeeping is likewise ignored even when dispatched at the window.
    window.dispatchEvent(
      new ErrorEvent('error', {
        message: 'Resize observer loop completed with undelivered notifications.',
      }),
    );
    expect(get(notifications)).toHaveLength(2);

    // A genuine uncaught script error with no Error object still uses the event message.
    window.dispatchEvent(new ErrorEvent('error', { message: 'render callback failed' }));
    list = get(notifications);
    expect(list).toHaveLength(3);
    expect(list[2]).toMatchObject({ type: 'error' });
    expect(list[2].message).toBeTruthy();
    expect(list[2].detail).toBeUndefined();
    expect(JSON.stringify(list[2])).not.toContain('render callback failed');

    // Some browser script errors omit both `error` and `message`; the trap must remain total.
    expect(() => window.dispatchEvent(new ErrorEvent('error'))).not.toThrow();
    list = get(notifications);
    expect(list).toHaveLength(4);
    expect(list[3]).toMatchObject({ type: 'error' });
    expect(list[3].message).toBeTruthy();
  });
});
