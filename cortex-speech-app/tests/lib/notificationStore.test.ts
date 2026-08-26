import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { notifications } from '../../src/lib/stores/notificationStore';

describe('notificationStore', () => {
  beforeEach(() => {
    notifications.clear();
  });

  it('starts empty', () => {
    expect(get(notifications)).toHaveLength(0);
  });

  it('adds a notification via .info()', () => {
    const id = notifications.info('test message');
    expect(id).toBeDefined();
    const state = get(notifications);
    expect(state).toHaveLength(1);
    expect(state[0].message).toBe('test message');
    expect(state[0].type).toBe('info');
  });

  it('adds success notification', () => {
    notifications.success('done!');
    const state = get(notifications);
    expect(state[0].type).toBe('success');
  });

  it('adds error notification', () => {
    notifications.error('error!');
    const state = get(notifications);
    expect(state[0].type).toBe('error');
  });

  it('fails closed on backend prose while retaining typed recovery metadata', () => {
    notifications.error('Save failed', {
      cause: {
        schema: 1,
        code: 'WRITE_FAILED',
        message: 'SQL failed at C:\\private\\library.db',
        retryable: true,
        suggestedAction: 'retry',
        operationId: '018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10',
      },
    });
    const notification = get(notifications)[0];
    expect(notification.detail).toBe('WRITE_FAILED · 018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10');
    expect(notification.retryable).toBe(true);
    expect(notification.suggestedAction).toBe('retry');
    expect(JSON.stringify(notification)).not.toMatch(/SQL|private|library\.db/);

    notifications.clear();
    notifications.error('Save failed', { detail: 'stack: C:\\private\\library.db' });
    expect(get(notifications)[0].detail).toBeUndefined();
  });

  it('adds warning notification', () => {
    notifications.warning('warning!');
    const state = get(notifications);
    expect(state[0].type).toBe('warning');
  });

  it('dismisses notification', () => {
    const id = notifications.info('dismiss me');
    expect(get(notifications)).toHaveLength(1);
    notifications.dismiss(id);
    expect(get(notifications)).toHaveLength(0);
  });

  it('auto-dismisses info notifications', async () => {
    vi.useFakeTimers();
    notifications.info('auto dismiss');
    expect(get(notifications)).toHaveLength(1);
    vi.advanceTimersByTime(4000);
    expect(get(notifications)).toHaveLength(0);
    vi.useRealTimers();
  });
});
