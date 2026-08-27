import { beforeEach, describe, expect, it, vi } from 'vitest';

const { currentDesktopWindowMock } = vi.hoisted(() => ({
  currentDesktopWindowMock: vi.fn(),
}));

vi.mock('./adapters/desktop', () => ({
  currentDesktopWindow: currentDesktopWindowMock,
}));

import { registerDurableCloseGuard } from './closeGuard';

type CloseHandler = (event: { preventDefault(): void }) => void | Promise<void>;

function windowFixture() {
  let handler: CloseHandler | null = null;
  const unlisten = vi.fn();
  const window = {
    onCloseRequested: vi.fn(async (next: CloseHandler) => {
      handler = next;
      return unlisten;
    }),
    destroy: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
  };
  currentDesktopWindowMock.mockResolvedValue(window);
  return {
    window,
    unlisten,
    dispatch: async () => {
      if (!handler) throw new Error('close handler was not registered');
      const preventDefault = vi.fn();
      await handler({ preventDefault });
      return preventDefault;
    },
  };
}

describe('durable desktop close guard', () => {
  beforeEach(() => {
    currentDesktopWindowMock.mockReset();
  });

  it('flushes visible drafts before destroying the native window', async () => {
    const fixture = windowFixture();
    const flush = vi.fn(async () => {});
    const unlisten = await registerDurableCloseGuard({
      flush,
      timeoutMs: 1_000,
      onFlushError: vi.fn(),
      onCloseError: vi.fn(),
    });

    expect(unlisten).toBe(fixture.unlisten);
    const preventDefault = await fixture.dispatch();
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(flush).toHaveBeenCalledOnce();
    expect(fixture.window.destroy).toHaveBeenCalledOnce();
    expect(fixture.window.close).not.toHaveBeenCalled();
  });

  it('keeps the window open when draft durability fails', async () => {
    const fixture = windowFixture();
    const failure = new Error('disk full');
    const onFlushError = vi.fn();
    await registerDurableCloseGuard({
      flush: async () => Promise.reject(failure),
      timeoutMs: 1_000,
      onFlushError,
      onCloseError: vi.fn(),
    });

    await fixture.dispatch();
    expect(onFlushError).toHaveBeenCalledWith(failure);
    expect(fixture.window.destroy).not.toHaveBeenCalled();
    expect(fixture.window.close).not.toHaveBeenCalled();
  });

  it('falls back to close and surfaces a double failure', async () => {
    const fixture = windowFixture();
    const destroyFailure = new Error('destroy denied');
    const closeFailure = new Error('close denied');
    fixture.window.destroy.mockRejectedValueOnce(destroyFailure);
    fixture.window.close.mockRejectedValueOnce(closeFailure);
    const onCloseError = vi.fn();
    const logError = vi.fn();
    await registerDurableCloseGuard({
      flush: async () => {},
      timeoutMs: 1_000,
      onFlushError: vi.fn(),
      onCloseError,
      logError,
    });

    await fixture.dispatch();
    expect(fixture.window.close).toHaveBeenCalledOnce();
    expect(onCloseError).toHaveBeenCalledWith(closeFailure);
    expect(logError).toHaveBeenCalledWith(
      'window.destroy failed; falling back to close():',
      destroyFailure,
    );
    expect(logError).toHaveBeenCalledWith('window.close fallback also failed:', closeFailure);
  });
});
