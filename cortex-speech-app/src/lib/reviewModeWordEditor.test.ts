import { describe, expect, it, vi } from 'vitest';
import { createReviewModeWordEditor } from './reviewModeWordEditor.svelte';
import type { WordTimestamp } from './types';

const words: WordTimestamp[] = [
  { word: 'one', start: 0.2, end: 0.5, confidence: 0.95 },
  { word: 'two', start: 0.6, end: 0.9, confidence: 0.5 },
];

function setup(text = 'one two') {
  let editText = text;
  let playing = false;
  const setEditText = vi.fn((value: string) => (editText = value));
  const setPlaying = vi.fn((value: boolean) => (playing = value));
  const setCurrentTime = vi.fn();
  const editor = createReviewModeWordEditor({
    words: () => words,
    range: () => ({ startTime: 10, endTime: 11 }),
    editText: () => editText,
    setEditText,
    playing: () => playing,
    setPlaying,
    setCurrentTime,
    playStart: () => 10.25,
  });
  return { editor, setEditText, setPlaying, setCurrentTime, text: () => editText };
}

describe('ReviewMode word editor controller', () => {
  it('binds word playback to the clip and avoids restarting the same active word', () => {
    const { editor, setPlaying, setCurrentTime } = setup();
    editor.playWord(words[0]);
    expect(editor.state.startOverride).toBeCloseTo(10.08);
    expect(editor.state.endOverride).toBeCloseTo(10.62);
    expect(setCurrentTime).toHaveBeenCalledWith(expect.closeTo(10.08));
    expect(setPlaying).toHaveBeenCalledWith(true);

    editor.playWord(words[0]);
    expect(setCurrentTime).toHaveBeenCalledOnce();
    editor.startWordEdit(words[1], 1);
    expect(editor.state.editingIndex).toBe(1);
    expect(setCurrentTime).toHaveBeenLastCalledWith(expect.closeTo(10.48));
  });

  it('commits only the selected token and records its visible chip', () => {
    const { editor, setEditText, text } = setup();
    editor.state.editingIndex = 1;
    expect(editor.commitWordEdit(0, words[0], 'changed')).toBe(false);
    expect(editor.commitWordEdit(1, words[1], 'دوو')).toBe(true);
    expect(text()).toBe('one دوو');
    expect(setEditText).toHaveBeenCalledWith('one دوو');
    expect(editor.chipText(words[1], 1)).toBe('دوو');
    editor.resetChips();
    expect(editor.chipText(words[1], 1)).toBe('two');
  });

  it('treats blank and unchanged edits as a non-destructive close', () => {
    const { editor, setEditText } = setup();
    editor.state.editingIndex = 0;
    expect(editor.commitWordEdit(0, words[0], '   ')).toBe(true);
    expect(editor.state.editingIndex).toBeNull();
    editor.state.editingIndex = 0;
    expect(editor.commitWordEdit(0, words[0], 'one')).toBe(true);
    expect(setEditText).not.toHaveBeenCalled();
  });

  it('falls back to an exact manual selection when repeated-word identity is ambiguous', () => {
    const repeated = setup('one one extra');
    const textarea = document.createElement('textarea');
    textarea.value = 'one one extra';
    document.body.append(textarea);
    const focus = vi.spyOn(textarea, 'focus');
    const selection = vi.spyOn(textarea, 'setSelectionRange');
    repeated.editor.state.editElement = textarea;
    repeated.editor.state.editingIndex = 0;

    expect(repeated.editor.commitWordEdit(0, words[0], 'changed')).toBe(false);
    expect(repeated.setEditText).not.toHaveBeenCalled();
    expect(focus).toHaveBeenCalledOnce();
    expect(selection).toHaveBeenCalledWith(0, 3);

    repeated.editor.state.editingIndex = 0;
    focus.mockClear();
    expect(repeated.editor.commitWordEdit(0, words[0], 'changed', true)).toBe(false);
    expect(focus).not.toHaveBeenCalled();
    textarea.remove();
  });

  it('selects a unique matching token, cancels safely, and replays the whole clip', () => {
    const { editor, setPlaying, setCurrentTime } = setup('prefix one suffix');
    const textarea = document.createElement('textarea');
    textarea.value = 'prefix one suffix';
    const selection = vi.spyOn(textarea, 'setSelectionRange');
    editor.state.editElement = textarea;
    editor.editWord(words[0], 0);
    expect(selection).toHaveBeenCalledWith(7, 10);

    editor.state.editingIndex = 1;
    editor.cancelWordEdit(0);
    expect(editor.state.editingIndex).toBe(1);
    editor.cancelWordEdit(1);
    expect(editor.state.editingIndex).toBeNull();

    editor.state.startOverride = 10.1;
    editor.state.endOverride = 10.2;
    editor.replay();
    expect(editor.state.startOverride).toBeNull();
    expect(editor.state.endOverride).toBeNull();
    expect(setCurrentTime).toHaveBeenLastCalledWith(10.25);
    expect(setPlaying).toHaveBeenLastCalledWith(true);
  });

  it('classifies only actionable confidence bands', () => {
    const { editor } = setup();
    expect(editor.confidenceClass(null)).toBe('');
    expect(editor.confidenceClass(0.59)).toBe('conf-low');
    expect(editor.confidenceClass(0.6)).toBe('conf-mid');
    expect(editor.confidenceClass(0.849)).toBe('conf-mid');
    expect(editor.confidenceClass(0.85)).toBe('');
  });
});
