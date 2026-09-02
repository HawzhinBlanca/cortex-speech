import { physicalKey } from './keyboard';

interface InboxKeyboardActions {
  editing: () => boolean;
  queueLength: () => number;
  currentIndex: () => number;
  commitEdit: () => void;
  cancelEdit: () => void;
  accept: () => void;
  startEdit: () => void;
  reject: () => void;
  togglePlayback: () => void;
  replay: () => void;
  skip: () => void;
  flag: () => void;
  undo: () => void;
  close: () => void;
  select: (index: number) => void;
}

export function handleReviewInboxKeydown(event: KeyboardEvent, actions: InboxKeyboardActions) {
  if (actions.editing()) {
    if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      actions.commitEdit();
    }
    if (event.key === 'Escape') {
      event.preventDefault();
      actions.cancelEdit();
    }
    return;
  }
  if (event.ctrlKey || event.metaKey || event.altKey) return;
  const target = event.target as HTMLElement | null;
  if (
    (target?.tagName === 'BUTTON' || target?.tagName === 'A') &&
    (event.key === ' ' || event.key === 'Enter')
  )
    return;
  if (
    target &&
    (target.tagName === 'INPUT' ||
      target.tagName === 'TEXTAREA' ||
      target.tagName === 'SELECT' ||
      target.isContentEditable)
  )
    return;
  const key = physicalKey(event);
  const action = (callback: () => void) => {
    event.preventDefault();
    callback();
  };
  switch (key) {
    case 'a':
      action(actions.accept);
      break;
    case 'e':
      action(actions.startEdit);
      break;
    case 'x':
      action(actions.reject);
      break;
    case ' ':
      action(actions.togglePlayback);
      break;
    case 'r':
      action(actions.replay);
      break;
    case 's':
      action(actions.skip);
      break;
    case 'f':
      action(actions.flag);
      break;
    case 'Backspace':
      action(actions.undo);
      break;
    case 'Escape':
      action(actions.close);
      break;
    case 'n':
    case 'ArrowRight':
    case 'ArrowDown':
      action(() => actions.select(actions.currentIndex() + 1));
      break;
    case 'p':
    case 'ArrowLeft':
    case 'ArrowUp':
      action(() => actions.select(actions.currentIndex() - 1));
      break;
    default:
      if (key >= '1' && key <= '9') {
        const index = Number.parseInt(key) - 1;
        if (index < actions.queueLength()) actions.select(index);
      }
  }
}
