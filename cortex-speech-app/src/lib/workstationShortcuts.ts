import { get } from 'svelte/store';
import type { Shortcut } from './keyboard';
import { openSettings } from './stores/settingsStore';
import { showKeyboardHelp, showReviewInbox } from './stores/uiStore';

type ShortcutRegistrar = { registerAll: (shortcuts: Shortcut[]) => void };

type WorkstationShortcutActions = {
  openFile: () => void;
  importDirectory: () => void;
  transcribe: () => void;
  enterReview: () => void;
  undo: () => void;
  redo: () => void;
  deleteSegment: () => void;
  validate: () => void;
  openReviewInbox: () => void;
  toggleSidebar: () => void;
  toggleStats: () => void;
  navigate: (direction: 'up' | 'down') => void;
  togglePlayback: () => void;
  rewind: () => void;
  forward: () => void;
  openCommandPalette: () => void;
};

export function registerWorkstationShortcuts(
  keyboard: ShortcutRegistrar,
  actions: WorkstationShortcutActions,
): void {
  keyboard.registerAll([
    {
      key: 'o',
      ctrl: true,
      description: 'Open audio file',
      descriptionKey: 'openAudioFile',
      action: actions.openFile,
      category: 'file',
    },
    {
      key: 'i',
      ctrl: true,
      description: 'Import directory',
      descriptionKey: 'importDirectory',
      action: actions.importDirectory,
      category: 'file',
    },
    {
      key: 't',
      ctrl: true,
      description: 'Transcribe selected',
      descriptionKey: 'transcribe',
      action: actions.transcribe,
      category: 'file',
    },
    {
      key: 'e',
      ctrl: true,
      shift: true,
      description: 'Review & correct',
      descriptionKey: 'reviewCorrect.label',
      action: actions.enterReview,
      category: 'navigation',
    },
    {
      key: 'z',
      ctrl: true,
      description: 'Undo',
      descriptionKey: 'undo',
      action: actions.undo,
      category: 'edit',
      allowInReview: true,
    },
    {
      key: 'z',
      ctrl: true,
      shift: true,
      description: 'Redo',
      descriptionKey: 'redo',
      action: actions.redo,
      category: 'edit',
      allowInReview: true,
    },
    {
      key: 'Delete',
      description: 'Delete segment',
      descriptionKey: 'deleteSegment',
      action: actions.deleteSegment,
      category: 'edit',
    },
    {
      key: 'f',
      ctrl: true,
      description: 'Focus search',
      descriptionKey: 'focusSearch',
      action: () => document.querySelector<HTMLInputElement>('[type=search]')?.focus(),
      category: 'navigation',
      allowInEditable: true,
    },
    {
      key: ',',
      ctrl: true,
      description: 'Open settings',
      descriptionKey: 'openSettings',
      action: () => {
        if (!get(showReviewInbox)) openSettings();
      },
      category: 'navigation',
    },
    {
      key: 'v',
      ctrl: true,
      shift: true,
      description: 'Validate dataset',
      descriptionKey: 'validateDataset',
      action: actions.validate,
      category: 'navigation',
    },
    {
      key: 'r',
      ctrl: true,
      shift: true,
      description: 'Open Review Inbox',
      descriptionKey: 'reviewInbox',
      action: actions.openReviewInbox,
      category: 'navigation',
    },
    {
      key: '/',
      ctrl: true,
      description: 'Keyboard shortcuts',
      descriptionKey: 'keyboardShortcuts',
      action: () => showKeyboardHelp.set(true),
      category: 'navigation',
    },
    {
      key: 's',
      shift: true,
      description: 'Toggle sidebar panel',
      descriptionKey: 'toggleSidebar',
      action: actions.toggleSidebar,
      category: 'navigation',
    },
    {
      key: 'd',
      shift: true,
      description: 'Toggle stats dashboard',
      descriptionKey: 'toggleStats',
      action: actions.toggleStats,
      category: 'navigation',
    },
    {
      key: 'j',
      description: 'Next segment',
      descriptionKey: 'nextSegment',
      action: () => actions.navigate('down'),
      category: 'navigation',
    },
    {
      key: 'k',
      description: 'Previous segment',
      descriptionKey: 'prevSegment',
      action: () => actions.navigate('up'),
      category: 'navigation',
    },
    {
      key: '/',
      shift: true,
      description: 'Keyboard shortcuts (? key)',
      descriptionKey: 'keyboardShortcuts',
      action: () => showKeyboardHelp.set(true),
      category: 'navigation',
    },
    {
      key: '?',
      description: 'Keyboard shortcuts (? key)',
      descriptionKey: 'keyboardShortcuts',
      action: () => showKeyboardHelp.set(true),
      category: 'navigation',
    },
    {
      key: ' ',
      ctrl: true,
      description: 'Play/pause',
      descriptionKey: 'playPause',
      action: actions.togglePlayback,
      category: 'playback',
    },
    {
      key: 'ArrowLeft',
      description: 'Rewind 5s',
      descriptionKey: 'rewind',
      action: actions.rewind,
      category: 'playback',
    },
    {
      key: 'ArrowRight',
      description: 'Forward 5s',
      descriptionKey: 'forward',
      action: actions.forward,
      category: 'playback',
    },
    {
      key: 'k',
      ctrl: true,
      description: 'Command palette',
      descriptionKey: 'cmdk.title',
      action: actions.openCommandPalette,
      category: 'general',
      allowInEditable: true,
      allowInReview: true,
    },
  ]);
}
