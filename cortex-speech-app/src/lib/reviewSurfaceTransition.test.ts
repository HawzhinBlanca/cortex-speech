import { render } from '@testing-library/svelte';
import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sharedDurableReviewUndo } from './durableReviewUndo.svelte';
import { locale } from './i18n';
import { registerReviewDraftFlusher } from './reviewDraftFlush';
import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';
import type { ReviewInboxQueueController } from './reviewInboxQueue.svelte';
import { createReviewInboxRuntimeController } from './reviewInboxRuntime.svelte';
import ReviewSurfaceTransitionHarness from './reviewSurfaceTransitionHarness.test.svelte';
import { createWorkstationViewController } from './workstationViewController.svelte';
import { showReviewInbox } from './stores/uiStore';

type WorkstationViewController = ReturnType<typeof createWorkstationViewController>;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

type TransitionBarrier = 'truth write' | 'projection reconciliation';

function installBarrier(barrier: TransitionBarrier) {
  if (barrier === 'truth write') {
    Object.assign(sharedDurableReviewUndo.state, {
      status: 'none',
      inFlight: false,
      truthWriteInFlight: true,
      truthWriteAmbiguous: false,
      truthProjectionPending: false,
      projectionOutcome: null,
    });
  } else {
    Object.assign(sharedDurableReviewUndo.state, {
      status: 'reconciling',
      inFlight: true,
      truthWriteInFlight: false,
      truthWriteAmbiguous: false,
      truthProjectionPending: true,
      projectionOutcome: 'applied',
    });
  }
}

function clearBarrier() {
  Object.assign(sharedDurableReviewUndo.state, {
    status: 'none',
    target: null,
    operationId: null,
    blockedReason: null,
    errorCode: null,
    inFlight: false,
    truthWriteInFlight: false,
    truthWriteAmbiguous: false,
    truthProjectionPending: false,
    projectionOutcome: null,
  });
}

const teardownViews: Array<() => void> = [];
const unregisterFlushers: Array<() => void> = [];

function viewController(): WorkstationViewController {
  let controller!: WorkstationViewController;
  const view = render(ReviewSurfaceTransitionHarness, {
    props: { onReady: (value: WorkstationViewController) => (controller = value) },
  });
  teardownViews.push(view.unmount);
  return controller;
}

function registerFlusher(flush: () => Promise<void>) {
  unregisterFlushers.push(registerReviewDraftFlusher(flush));
}

function inboxRuntime(flush: () => Promise<void>) {
  const focus = vi.fn();
  const onClose = vi.fn();
  const draft = {
    state: { textarea: { focus } },
    flush,
  } as unknown as ReviewInboxDraftController;
  const queue = {} as ReviewInboxQueueController;
  return {
    runtime: createReviewInboxRuntimeController({ queue, draft, onClose }),
    focus,
    onClose,
  };
}

describe('review surface transition barriers', () => {
  beforeEach(() => {
    locale.set('en');
    showReviewInbox.set(false);
    clearBarrier();
  });

  afterEach(() => {
    clearBarrier();
    showReviewInbox.set(false);
    while (unregisterFlushers.length > 0) unregisterFlushers.pop()?.();
    while (teardownViews.length > 0) teardownViews.pop()?.();
  });

  it.each(['truth write', 'projection reconciliation'] as const)(
    'keeps Review mounted while a %s already owns transition authority',
    async (barrier) => {
      const flush = vi.fn(async () => undefined);
      registerFlusher(flush);
      const view = viewController();
      view.enterReviewMode();
      installBarrier(barrier);

      await view.leaveReviewMode('curate');

      expect(view.viewMode).toBe('review');
      expect(flush).not.toHaveBeenCalled();

      clearBarrier();
      await view.leaveReviewMode('curate');
      expect(view.viewMode).toBe('curate');
      expect(flush).toHaveBeenCalledOnce();
    },
  );

  it.each(['truth write', 'projection reconciliation'] as const)(
    'rechecks a %s that starts while Review exit is waiting for draft durability',
    async (barrier) => {
      const pendingFlush = deferred<void>();
      const flush = vi.fn(() => pendingFlush.promise);
      registerFlusher(flush);
      const view = viewController();
      view.enterReviewMode();

      const leaving = view.leaveReviewMode('insights');
      await vi.waitFor(() => expect(flush).toHaveBeenCalledOnce());
      installBarrier(barrier);
      pendingFlush.resolve();
      await leaving;

      expect(view.viewMode).toBe('review');
    },
  );

  it.each(['truth write', 'projection reconciliation'] as const)(
    'does not open Inbox by unmounting Review during a %s',
    async (barrier) => {
      const flush = vi.fn(async () => undefined);
      registerFlusher(flush);
      const view = viewController();
      view.enterReviewMode();
      installBarrier(barrier);

      await view.openReviewInbox();

      expect(view.viewMode).toBe('review');
      expect(get(showReviewInbox)).toBe(false);
      expect(flush).not.toHaveBeenCalled();
    },
  );

  it('keeps Inbox closed when projection reconciliation starts during Review-to-Inbox draft flush', async () => {
    const pendingFlush = deferred<void>();
    const flush = vi.fn(() => pendingFlush.promise);
    registerFlusher(flush);
    const view = viewController();
    view.enterReviewMode();

    const opening = view.openReviewInbox();
    await vi.waitFor(() => expect(flush).toHaveBeenCalledOnce());
    installBarrier('projection reconciliation');
    pendingFlush.resolve();
    await opening;

    expect(view.viewMode).toBe('review');
    expect(get(showReviewInbox)).toBe(false);
  });

  it('does not stale-open Inbox after a newer Insights workspace intent', async () => {
    const pendingFlush = deferred<void>();
    const flush = vi.fn(() => pendingFlush.promise);
    registerFlusher(flush);
    const view = viewController();
    view.enterReviewMode();

    const openingInbox = view.openReviewInbox();
    await vi.waitFor(() => expect(flush).toHaveBeenCalledOnce());
    view.selectWorkspace('insights');
    await vi.waitFor(() => expect(flush).toHaveBeenCalledTimes(2));
    pendingFlush.resolve();
    await openingInbox;
    await vi.waitFor(() => expect(view.viewMode).toBe('insights'));

    expect(get(showReviewInbox)).toBe(false);
  });

  it.each(['truth write', 'projection reconciliation'] as const)(
    'keeps Inbox mounted while a %s already owns transition authority',
    async (barrier) => {
      const flush = vi.fn(async () => undefined);
      const { runtime, onClose } = inboxRuntime(flush);
      installBarrier(barrier);

      await runtime.requestClose();

      expect(onClose).not.toHaveBeenCalled();
      expect(flush).not.toHaveBeenCalled();
      expect(runtime.state.closePending).toBe(false);
    },
  );

  it.each(['truth write', 'projection reconciliation'] as const)(
    'rechecks a %s that starts while Inbox close is waiting for draft durability',
    async (barrier) => {
      const pendingFlush = deferred<void>();
      const flush = vi.fn(() => pendingFlush.promise);
      const { runtime, onClose } = inboxRuntime(flush);

      const closing = runtime.requestClose();
      await vi.waitFor(() => expect(flush).toHaveBeenCalledOnce());
      expect(runtime.state.closePending).toBe(true);
      installBarrier(barrier);
      pendingFlush.resolve();
      await closing;

      expect(onClose).not.toHaveBeenCalled();
      expect(runtime.state.closePending).toBe(false);
    },
  );

  it('keeps an ambiguously saved Inbox mounted and restores its editor focus', async () => {
    const flush = vi.fn(async () => undefined);
    const { runtime, focus, onClose } = inboxRuntime(flush);
    Object.assign(sharedDurableReviewUndo.state, {
      status: 'failed',
      truthWriteAmbiguous: true,
    });

    await runtime.requestClose();
    await Promise.resolve();

    expect(onClose).not.toHaveBeenCalled();
    expect(flush).not.toHaveBeenCalled();
    expect(focus).toHaveBeenCalledOnce();
    expect(runtime.state.status).toContain('uncertain');
  });
});
