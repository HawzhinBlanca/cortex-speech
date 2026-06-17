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

  it('adds warning notification', () => {
    notifications.warning('warning!');
    const state = get(notifications);
    expect(state[0].type).toBe('warning');
  });

  it('adds progress notification', () => {
    const id = notifications.progress('processing', 50);
    const state = get(notifications);
    const n = state.find(x => x.id === id);
    expect(n?.progress).toBe(50);
  });

  it('updates progress', () => {
    const id = notifications.progress('processing', 0);
    notifications.updateProgress(id, 75);
    const state = get(notifications);
    const n = state.find(x => x.id === id);
    expect(n?.progress).toBe(75);
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
