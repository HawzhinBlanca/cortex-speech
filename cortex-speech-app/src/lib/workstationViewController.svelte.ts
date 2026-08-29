import { fromStore, get } from 'svelte/store';
import { flushReviewDrafts } from './reviewDraftFlush';
import { sharedDurableReviewUndo } from './durableReviewUndo.svelte';
import { locale, t } from './i18n';
import { notifications } from './stores/notificationStore';
import { segmentStats } from './stores/segmentStore';
import { openSettings } from './stores/settingsStore';
import {
  isProcessing,
  showDatasetMerge,
  showReviewInbox,
  showSpeakerPanel,
  showValidationPanel,
  showWslConsole,
} from './stores/uiStore';

export type WorkstationViewMode = 'curate' | 'insights' | 'review';

const SIDEBAR_MEDIA_QUERY = '(min-width: 900px)';
const STATS_MEDIA_QUERY = '(min-width: 1200px)';

type ReviewPanelSnapshot = {
  sidebarOpen: boolean;
  statsOpen: boolean;
  sidebarWide: boolean;
  statsWide: boolean;
};

function loadPanelWidth(key: string, fallback: number): number {
  if (typeof localStorage === 'undefined') return fallback;
  const value = Number(localStorage.getItem(key));
  return Number.isFinite(value) && value >= 200 && value <= 600 ? value : fallback;
}

export function createWorkstationViewController(requireDesktopRuntime: () => boolean) {
  const currentLocale = fromStore(locale);
  let sidebarOpen = $state(true);
  let statsOpen = $state(true);
  let sidebarWidth = $state(loadPanelWidth('cortex.sidebarWidth', 288));
  let statsWidth = $state(loadPanelWidth('cortex.statsWidth', 288));
  let batchSpeakerId = $state('');
  let editorTab = $state<'interactive' | 'raw'>('interactive');
  let viewMode = $state<WorkstationViewMode>('curate');
  let reviewNudgeDismissed = $state(false);
  let showCommandPalette = $state(false);
  let showHotkeyOverlay = $state(false);
  let reviewPanelSnapshot: ReviewPanelSnapshot | null = null;
  // One monotonic intent owns all asynchronous workspace changes. Separate counters let an older
  // Inbox-open continuation outlive a newer Insights/Library selection and reopen over it.
  let surfaceIntentSequence = 0;

  $effect(() => {
    if (typeof localStorage === 'undefined') return;
    localStorage.setItem('cortex.sidebarWidth', String(sidebarWidth));
    localStorage.setItem('cortex.statsWidth', String(statsWidth));
  });

  $effect(() => {
    document.documentElement.dir = currentLocale.current === 'ckb' ? 'rtl' : 'ltr';
    document.documentElement.lang = currentLocale.current;
  });

  $effect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Alt') showHotkeyOverlay = true;
    };
    const onKeyUp = (event: KeyboardEvent) => {
      if (event.key === 'Alt') showHotkeyOverlay = false;
    };
    const onBlur = () => (showHotkeyOverlay = false);
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('keyup', onKeyUp);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('keyup', onKeyUp);
      window.removeEventListener('blur', onBlur);
    };
  });

  $effect(() => {
    const statsMedia = window.matchMedia(STATS_MEDIA_QUERY);
    const sidebarMedia = window.matchMedia(SIDEBAR_MEDIA_QUERY);
    const updateStats = (event: MediaQueryListEvent | MediaQueryList) => {
      if (reviewPanelSnapshot === null) statsOpen = event.matches;
    };
    const updateSidebar = (event: MediaQueryListEvent | MediaQueryList) => {
      if (reviewPanelSnapshot === null) sidebarOpen = event.matches;
    };
    updateStats(statsMedia);
    updateSidebar(sidebarMedia);
    statsMedia.addEventListener('change', updateStats);
    sidebarMedia.addEventListener('change', updateSidebar);
    return () => {
      statsMedia.removeEventListener('change', updateStats);
      sidebarMedia.removeEventListener('change', updateSidebar);
    };
  });

  function enterReviewModeForIntent(intent: number): void {
    if (intent !== surfaceIntentSequence) return;
    if (!requireDesktopRuntime()) return;
    if (viewMode !== 'review') {
      reviewPanelSnapshot = {
        sidebarOpen,
        statsOpen,
        sidebarWide: window.matchMedia(SIDEBAR_MEDIA_QUERY).matches,
        statsWide: window.matchMedia(STATS_MEDIA_QUERY).matches,
      };
    }
    viewMode = 'review';
    sidebarOpen = false;
    statsOpen = false;
  }

  function enterReviewMode(): void {
    enterReviewModeForIntent(++surfaceIntentSequence);
  }

  async function leaveReviewModeForIntent(
    nextView: 'curate' | 'insights',
    intent: number,
  ): Promise<void> {
    if (intent !== surfaceIntentSequence) return;
    if (sharedDurableReviewUndo.blocksSurfaceTransition()) {
      const translate = get(t);
      notifications.error(
        translate(
          sharedDurableReviewUndo.state.truthWriteAmbiguous
            ? 'review.truthWriteUncertainRestart'
            : 'inbox.disabled.saving',
        ),
      );
      return;
    }
    try {
      await flushReviewDrafts();
    } catch (error) {
      if (intent === surfaceIntentSequence) {
        const translate = get(t);
        notifications.error(translate('review.closeDraftFailed'), {
          cause: error,
          publicDetail: translate('review.closeDraftFailedHint'),
        });
      }
      return;
    }
    if (
      intent !== surfaceIntentSequence ||
      viewMode !== 'review' ||
      sharedDurableReviewUndo.blocksSurfaceTransition()
    )
      return;
    const sidebarWide = window.matchMedia(SIDEBAR_MEDIA_QUERY).matches;
    const statsWide = window.matchMedia(STATS_MEDIA_QUERY).matches;
    const snapshot = reviewPanelSnapshot;
    sidebarOpen = snapshot?.sidebarWide === sidebarWide ? snapshot.sidebarOpen : sidebarWide;
    statsOpen = snapshot?.statsWide === statsWide ? snapshot.statsOpen : statsWide;
    reviewPanelSnapshot = null;
    viewMode = nextView;
  }

  function leaveReviewMode(nextView: 'curate' | 'insights' = 'curate'): Promise<void> {
    return leaveReviewModeForIntent(nextView, ++surfaceIntentSequence);
  }

  function selectWorkspace(id: string): void {
    const intent = ++surfaceIntentSequence;
    if (id === 'settings') openSettings();
    else if (id === 'review') enterReviewModeForIntent(intent);
    else if (viewMode === 'review')
      void leaveReviewModeForIntent(id as 'curate' | 'insights', intent);
    else viewMode = id as 'curate' | 'insights';
  }

  function openValidationPanel(): void {
    if (!requireDesktopRuntime() || get(isProcessing) || get(segmentStats).total === 0) return;
    showValidationPanel.set(true);
  }
  async function openReviewInbox(): Promise<void> {
    if (!requireDesktopRuntime() || get(showReviewInbox)) return;
    const intent = ++surfaceIntentSequence;
    if (viewMode === 'review') {
      // Do not keep two independent editors alive for one draft. Leaving ReviewMode first performs
      // its durable flush and unmounts its cache; closing Inbox therefore returns to Library instead
      // of reviving a stale hidden editor that could overwrite the Inbox's newer correction.
      await leaveReviewModeForIntent('curate', intent);
      if (viewMode === 'review' || intent !== surfaceIntentSequence) return;
    } else {
      try {
        await flushReviewDrafts();
      } catch (error) {
        if (intent === surfaceIntentSequence) {
          const translate = get(t);
          notifications.error(translate('review.closeDraftFailed'), {
            cause: error,
            publicDetail: translate('review.closeDraftFailedHint'),
          });
        }
        return;
      }
    }
    if (intent === surfaceIntentSequence) showReviewInbox.set(true);
  }
  function openWslConsole(): void {
    if (requireDesktopRuntime()) showWslConsole.set(true);
  }
  function openSpeakerPanel(): void {
    if (requireDesktopRuntime()) showSpeakerPanel.set(true);
  }
  function openDatasetMerge(): void {
    if (requireDesktopRuntime()) showDatasetMerge.set(true);
  }

  return {
    get sidebarOpen() {
      return sidebarOpen;
    },
    set sidebarOpen(value: boolean) {
      sidebarOpen = value;
    },
    get statsOpen() {
      return statsOpen;
    },
    set statsOpen(value: boolean) {
      statsOpen = value;
    },
    get sidebarWidth() {
      return sidebarWidth;
    },
    set sidebarWidth(value: number) {
      sidebarWidth = value;
    },
    get statsWidth() {
      return statsWidth;
    },
    set statsWidth(value: number) {
      statsWidth = value;
    },
    get batchSpeakerId() {
      return batchSpeakerId;
    },
    set batchSpeakerId(value: string) {
      batchSpeakerId = value;
    },
    get editorTab() {
      return editorTab;
    },
    set editorTab(value: 'interactive' | 'raw') {
      editorTab = value;
    },
    get viewMode() {
      return viewMode;
    },
    get reviewNudgeDismissed() {
      return reviewNudgeDismissed;
    },
    set reviewNudgeDismissed(value: boolean) {
      reviewNudgeDismissed = value;
    },
    get showCommandPalette() {
      return showCommandPalette;
    },
    set showCommandPalette(value: boolean) {
      showCommandPalette = value;
    },
    get showHotkeyOverlay() {
      return showHotkeyOverlay;
    },
    enterReviewMode,
    leaveReviewMode,
    openDatasetMerge,
    openReviewInbox,
    openSpeakerPanel,
    openValidationPanel,
    openWslConsole,
    selectWorkspace,
  };
}
