import { replaceWordToken, wordPlayBounds } from './wordEdit';
import type { WordTimestamp } from './types';

interface WordEditorDependencies {
  words: () => readonly WordTimestamp[];
  range: () => { startTime: number; endTime: number };
  editText: () => string;
  setEditText: (text: string) => void;
  playing: () => boolean;
  setPlaying: (playing: boolean) => void;
  setCurrentTime: (time: number) => void;
  playStart: () => number;
  mutationBlocked?: () => boolean;
}

export function createReviewModeWordEditor(deps: WordEditorDependencies) {
  const state = $state({
    startOverride: null as number | null,
    endOverride: null as number | null,
    editingIndex: null as number | null,
    editedChips: {} as Record<number, string>,
    editElement: undefined as HTMLTextAreaElement | undefined,
  });
  const spokenPadding = 0.12;

  function clearOverride() {
    state.startOverride = null;
    state.endOverride = null;
  }

  function resetChips() {
    state.editedChips = {};
  }

  function chipText(word: WordTimestamp, index: number): string {
    return state.editedChips[index] ?? word.word;
  }

  function playWord(word: WordTimestamp) {
    if (deps.mutationBlocked?.()) return;
    const range = deps.range();
    const bounds = wordPlayBounds(word, range.startTime, range.endTime, spokenPadding);
    if (
      deps.playing() &&
      state.startOverride === bounds.start &&
      state.endOverride === bounds.end
    ) {
      return;
    }
    state.startOverride = bounds.start;
    state.endOverride = bounds.end;
    deps.setCurrentTime(bounds.start);
    deps.setPlaying(true);
  }

  function startWordEdit(word: WordTimestamp, index: number) {
    if (deps.mutationBlocked?.()) return;
    playWord(word);
    state.editingIndex = index;
  }

  function commitWordEdit(
    index: number,
    word: WordTimestamp,
    value: string,
    viaBlur = false,
  ): boolean {
    if (state.editingIndex !== index) return false;
    if (deps.mutationBlocked?.()) {
      state.editingIndex = null;
      return false;
    }
    state.editingIndex = null;
    const current = chipText(word, index);
    const fix = value.trim();
    if (!fix || fix === current) return true;
    const words = deps.words();
    const replaced = replaceWordToken(deps.editText(), index, current, fix, words.length);
    if (replaced === null) {
      if (!viaBlur) editWord(word, index);
      return false;
    }
    deps.setEditText(replaced);
    state.editedChips = { ...state.editedChips, [index]: fix };
    return true;
  }

  function cancelWordEdit(index: number) {
    if (state.editingIndex === index) state.editingIndex = null;
  }

  function editWord(word: WordTimestamp, index: number) {
    if (deps.mutationBlocked?.()) return;
    const element = state.editElement;
    if (!element) return;
    element.focus();
    const text = deps.editText();
    const tokens: Array<{ start: number; len: number; word: string }> = [];
    const expression = /\S+/g;
    let match: RegExpExecArray | null;
    while ((match = expression.exec(text)) !== null) {
      tokens.push({ start: match.index, len: match[0].length, word: match[0] });
    }
    const wanted = chipText(word, index);
    const target =
      tokens[index] && tokens[index].word === wanted
        ? tokens[index]
        : tokens.find((token) => token.word === wanted);
    if (target) element.setSelectionRange(target.start, target.start + target.len);
  }

  function replay() {
    if (deps.mutationBlocked?.()) return;
    clearOverride();
    deps.setCurrentTime(deps.playStart());
    deps.setPlaying(true);
  }

  function confidenceClass(confidence: number | undefined | null): string {
    if (confidence == null) return '';
    if (confidence < 0.6) return 'conf-low';
    if (confidence < 0.85) return 'conf-mid';
    return '';
  }

  return {
    state,
    clearOverride,
    resetChips,
    chipText,
    playWord,
    startWordEdit,
    commitWordEdit,
    cancelWordEdit,
    editWord,
    replay,
    confidenceClass,
  };
}

export type ReviewModeWordEditor = ReturnType<typeof createReviewModeWordEditor>;
