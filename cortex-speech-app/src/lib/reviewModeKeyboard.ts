import { physicalKey } from './keyboard';

interface ReviewModeKeyboardActions {
  inboxOpen: () => boolean;
  submit: (acceptAsIs: boolean) => void;
  focusEditor: () => void;
  blurEditor: () => void;
  markBad: () => void;
  togglePlayback: () => void;
  replay: () => void;
  navigate: (delta: number) => void;
  undo: () => void;
}

export function handleReviewModeKeydown(event: KeyboardEvent, actions: ReviewModeKeyboardActions) {
  if (actions.inboxOpen()) return;
  if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
    event.preventDefault();
    actions.submit(false);
    return;
  }
  const element = event.target as HTMLElement | null;
  const typing =
    !!element &&
    (element.tagName === 'TEXTAREA' ||
      element.tagName === 'INPUT' ||
      element.tagName === 'SELECT' ||
      element.isContentEditable);
  if (typing) {
    if (event.key === 'Escape') {
      event.preventDefault();
      actions.blurEditor();
    }
    return;
  }
  if (event.ctrlKey || event.metaKey || event.altKey) return;
  if (
    (element?.tagName === 'BUTTON' || element?.tagName === 'A') &&
    (event.key === ' ' || event.key === 'Enter')
  )
    return;
  switch (physicalKey(event)) {
    case 'a':
      event.preventDefault();
      actions.submit(true);
      break;
    case 'e':
      event.preventDefault();
      actions.focusEditor();
      break;
    case 'x':
      event.preventDefault();
      actions.markBad();
      break;
    case ' ':
      event.preventDefault();
      actions.togglePlayback();
      break;
    case 'r':
      event.preventDefault();
      actions.replay();
      break;
    case 'n':
    case 'ArrowRight':
    case 'ArrowDown':
      event.preventDefault();
      actions.navigate(1);
      break;
    case 'p':
    case 'ArrowLeft':
    case 'ArrowUp':
      event.preventDefault();
      actions.navigate(-1);
      break;
    case 'Backspace':
      event.preventDefault();
      actions.undo();
      break;
  }
}
