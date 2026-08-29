import { describe, expect, it, vi } from 'vitest';
import { handleReviewInboxKeydown } from './reviewInboxKeyboard';
import { handleReviewModeKeydown } from './reviewModeKeyboard';

function keyEvent(
  key: string,
  code: string,
  options: KeyboardEventInit & { target?: HTMLElement } = {},
) {
  const { target, ...init } = options;
  const event = new KeyboardEvent('keydown', { key, code, cancelable: true, ...init });
  if (target) Object.defineProperty(event, 'target', { value: target });
  return event;
}

function modeActions() {
  return {
    inboxOpen: vi.fn(() => false),
    submit: vi.fn(),
    focusEditor: vi.fn(),
    blurEditor: vi.fn(),
    markBad: vi.fn(),
    togglePlayback: vi.fn(),
    replay: vi.fn(),
    navigate: vi.fn(),
    undo: vi.fn(),
  };
}

function inboxActions() {
  return {
    editing: vi.fn(() => false),
    queueLength: vi.fn(() => 3),
    currentIndex: vi.fn(() => 1),
    commitEdit: vi.fn(),
    cancelEdit: vi.fn(),
    accept: vi.fn(),
    startEdit: vi.fn(),
    reject: vi.fn(),
    togglePlayback: vi.fn(),
    replay: vi.fn(),
    skip: vi.fn(),
    flag: vi.fn(),
    undo: vi.fn(),
    close: vi.fn(),
    select: vi.fn(),
  };
}

describe('review keyboard controllers', () => {
  it('keeps ReviewMode commit available in the editor while suppressing every other typing shortcut', () => {
    const actions = modeActions();
    const textarea = document.createElement('textarea');
    const commit = keyEvent('Enter', 'Enter', { ctrlKey: true, target: textarea });
    handleReviewModeKeydown(commit, actions);
    expect(commit.defaultPrevented).toBe(true);
    expect(actions.submit).toHaveBeenCalledWith(false);

    const escape = keyEvent('Escape', 'Escape', { target: textarea });
    handleReviewModeKeydown(escape, actions);
    expect(escape.defaultPrevented).toBe(true);
    expect(actions.blurEditor).toHaveBeenCalledOnce();

    handleReviewModeKeydown(keyEvent('ا', 'KeyA', { target: textarea }), actions);
    expect(actions.submit).toHaveBeenCalledTimes(1);
  });

  it('maps every ReviewMode physical shortcut and leaves native button/modifier keys alone', () => {
    const actions = modeActions();
    const cases: Array<[string, string, keyof ReturnType<typeof modeActions>, unknown[]]> = [
      ['ا', 'KeyA', 'submit', [true]],
      ['ب', 'KeyE', 'focusEditor', []],
      ['خ', 'KeyX', 'markBad', []],
      [' ', 'Space', 'togglePlayback', []],
      ['ڕ', 'KeyR', 'replay', []],
      ['ن', 'KeyN', 'navigate', [1]],
      ['ArrowDown', 'ArrowDown', 'navigate', [1]],
      ['پ', 'KeyP', 'navigate', [-1]],
      ['ArrowUp', 'ArrowUp', 'navigate', [-1]],
      ['Backspace', 'Backspace', 'undo', []],
    ];
    for (const [key, code, action, args] of cases) {
      const event = keyEvent(key, code);
      handleReviewModeKeydown(event, actions);
      expect(event.defaultPrevented, `${code} should be owned by review`).toBe(true);
      expect(actions[action]).toHaveBeenLastCalledWith(...args);
    }

    const button = document.createElement('button');
    handleReviewModeKeydown(keyEvent(' ', 'Space', { target: button }), actions);
    handleReviewModeKeydown(keyEvent('x', 'KeyX', { altKey: true }), actions);
    expect(actions.togglePlayback).toHaveBeenCalledOnce();
    expect(actions.markBad).toHaveBeenCalledOnce();

    actions.inboxOpen.mockReturnValue(true);
    handleReviewModeKeydown(keyEvent('a', 'KeyA'), actions);
    expect(actions.submit).toHaveBeenCalledOnce();
  });

  it('commits or cancels Inbox editing without leaking bare review shortcuts', () => {
    const actions = inboxActions();
    actions.editing.mockReturnValue(true);
    const commit = keyEvent('Enter', 'Enter', { metaKey: true });
    handleReviewInboxKeydown(commit, actions);
    expect(commit.defaultPrevented).toBe(true);
    expect(actions.commitEdit).toHaveBeenCalledOnce();

    const cancel = keyEvent('Escape', 'Escape');
    handleReviewInboxKeydown(cancel, actions);
    expect(cancel.defaultPrevented).toBe(true);
    expect(actions.cancelEdit).toHaveBeenCalledOnce();

    handleReviewInboxKeydown(keyEvent('a', 'KeyA'), actions);
    expect(actions.accept).not.toHaveBeenCalled();
  });

  it('maps Inbox actions, navigation aliases and bounded number shortcuts', () => {
    const actions = inboxActions();
    const cases: Array<[string, string, keyof ReturnType<typeof inboxActions>]> = [
      ['ا', 'KeyA', 'accept'],
      ['ب', 'KeyE', 'startEdit'],
      ['خ', 'KeyX', 'reject'],
      [' ', 'Space', 'togglePlayback'],
      ['ڕ', 'KeyR', 'replay'],
      ['س', 'KeyS', 'skip'],
      ['ف', 'KeyF', 'flag'],
      ['Backspace', 'Backspace', 'undo'],
      ['Escape', 'Escape', 'close'],
    ];
    for (const [key, code, action] of cases) {
      const event = keyEvent(key, code);
      handleReviewInboxKeydown(event, actions);
      expect(event.defaultPrevented, `${code} should be owned by the inbox`).toBe(true);
      expect(actions[action]).toHaveBeenCalledOnce();
    }

    for (const [key, code, expected] of [
      ['ن', 'KeyN', 2],
      ['ArrowRight', 'ArrowRight', 2],
      ['ArrowDown', 'ArrowDown', 2],
      ['پ', 'KeyP', 0],
      ['ArrowLeft', 'ArrowLeft', 0],
      ['ArrowUp', 'ArrowUp', 0],
    ] as const) {
      handleReviewInboxKeydown(keyEvent(key, code), actions);
      expect(actions.select).toHaveBeenLastCalledWith(expected);
    }
    handleReviewInboxKeydown(keyEvent('٣', 'Digit3'), actions);
    expect(actions.select).toHaveBeenLastCalledWith(2);
    const calls = actions.select.mock.calls.length;
    handleReviewInboxKeydown(keyEvent('٩', 'Digit9'), actions);
    expect(actions.select).toHaveBeenCalledTimes(calls);
  });

  it('preserves native Inbox controls, text editing and modifier chords', () => {
    const actions = inboxActions();
    const button = document.createElement('button');
    const input = document.createElement('input');
    handleReviewInboxKeydown(keyEvent(' ', 'Space', { target: button }), actions);
    handleReviewInboxKeydown(keyEvent('a', 'KeyA', { target: input }), actions);
    handleReviewInboxKeydown(keyEvent('x', 'KeyX', { ctrlKey: true }), actions);
    expect(actions.togglePlayback).not.toHaveBeenCalled();
    expect(actions.accept).not.toHaveBeenCalled();
    expect(actions.reject).not.toHaveBeenCalled();
  });
});
