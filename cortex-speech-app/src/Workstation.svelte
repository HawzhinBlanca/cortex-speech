<script lang="ts">
  import CircleCheckBig from '@lucide/svelte/icons/circle-check-big';
  import LoaderCircle from '@lucide/svelte/icons/loader-circle';
  import Mic from '@lucide/svelte/icons/mic';
  import Search from '@lucide/svelte/icons/search';
  import SquarePen from '@lucide/svelte/icons/square-pen';
  import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
  import X from '@lucide/svelte/icons/x';
  import { onMount, onDestroy, untrack } from 'svelte';
  import { get } from 'svelte/store';
  import * as api from './lib/commands';
  import { createAutosaveController, flushAutosaveForIds } from './lib/autosave';
  import { createSegmentMetadataCoordinator } from './lib/segmentMetadataCoordinator';
  import { registerDurableCloseGuard } from './lib/closeGuard';
  import { chooseDirectory, saveFile } from './lib/fileDialogs';
  import { flushReviewDrafts } from './lib/reviewDraftFlush';
  import { formatPublicErrorReference, formatUnknownError } from './lib/errorText';
  import type {
    AgenticReadiness,
    AgentImportReport,
    AgentOrchestrationStage,
    AgentStageEvent,
  } from './lib/commands';
  import type { SpeechSegment, WordTimestamp } from './lib/types';
  import {
    segments,
    selectedSegmentId,
    filterVerified,
    wordTimestamps,
    searchQuery,
    sortOrder,
    type SortOrder,
    selectedSegment,
    filteredSegments,
    segmentStats,
    libraryLoadError,
    libraryTruncated,
  } from './lib/stores/segmentStore';
  import { settings, openSettings } from './lib/stores/settingsStore';
  import {
    showKeyboardHelp,
    showConfirmDialog,
    isProcessing,
    statusMessage,
  } from './lib/stores/uiStore';
  import { notifications } from './lib/stores/notificationStore';
  import { isVerifiedGood } from './lib/segmentQuality';
  import { historyStore } from './lib/stores/historyStore';
  import {
    initKeyboardManager,
    globalKeyboardManager,
    modKeyLabel,
    type Shortcut,
  } from './lib/keyboard';
  // Platform-aware modifier label (Ctrl on Windows, ⌘ on Mac) for the hardcoded kbd hints.
  const modKey = modKeyLabel();
  import {
    startEventListeners,
    stopEventListeners,
    setImportCompleteHandler,
    setBatchCompleteHandler,
  } from './lib/events';
  import { parseActionableError } from './lib/errors';
  import { locale, t } from './lib/i18n';
  import { activeOperations, startOperation, endOperation } from './lib/invoke';
  import {
    batchProgress,
    pipelinePhase,
    filesProcessed,
    pipelineTotal,
    pipelineCurrentFile,
    pipelineStatus,
    agentPipelineStages,
    showValidationPanel,
    showSpeakerPanel,
    showDatasetMerge,
    showWslConsole,
    showReviewInbox,
  } from './lib/stores/uiStore';
  import { isTauriRuntime } from './lib/runtime';
  import AudioPlayer from './lib/AudioPlayer.svelte';
  import Waveform from './lib/Waveform.svelte';
  import ErrorBoundary from './lib/ErrorBoundary.svelte';
  import LazyComponent from './lib/LazyComponent.svelte';
  import AgentReportPanel from './lib/AgentReportPanel.svelte';
  import SearchBar from './lib/SearchBar.svelte';
  import StatusBar from './lib/StatusBar.svelte';
  import VirtualList from './lib/VirtualList.svelte';
  import DiffView from './lib/DiffView.svelte';
  import EmptyState from './lib/EmptyState.svelte';
  import ActivityRail from './lib/ActivityRail.svelte';
  import ProcessingProgress from './lib/ProcessingProgress.svelte';
  import PanelSplitter from './lib/PanelSplitter.svelte';
  import HistoryPanel from './lib/HistoryPanel.svelte';
  import WorkstationOverlays from './lib/WorkstationOverlays.svelte';
  import WorkstationRecoveryNotices from './lib/WorkstationRecoveryNotices.svelte';
  import WorkstationHeader from './lib/WorkstationHeader.svelte';
  import {
    parseSourceMeta,
    parseWordTimestamps,
    chunkPlaybackRange,
    segmentSourceFilename,
    truncateFilename,
    segmentChunkLabel,
  } from './lib/alignment';
  import { wordPlayBounds } from './lib/wordEdit';

  type HistoryPanelApi = {
    recordAction: (description: string, type: 'edit' | 'verify' | 'delete' | 'import') => void;
  };

  // Secondary workspaces are isolated chunks. These stable loader functions are intentionally
  // declared outside reactive work so a parent update cannot restart an in-flight import.
  const loadSettingsPanel = () => import('./lib/SettingsPanel.svelte');
  const loadStatsDashboard = () => import('./lib/StatsDashboard.svelte');
  const loadRefineryPanel = () => import('./lib/RefineryPanel.svelte');
  const loadReviewMode = () => import('./lib/ReviewMode.svelte');
  const loadKeyboardShortcuts = () => import('./lib/KeyboardShortcuts.svelte');
  const loadValidationPanel = () => import('./lib/ValidationPanel.svelte');
  const loadReviewInbox = () => import('./lib/ReviewInbox.svelte');
  const loadSpeakerPanel = () => import('./lib/SpeakerPanel.svelte');
  const loadDatasetMerge = () => import('./lib/DatasetMerge.svelte');
  const loadWslConsolePanel = () => import('./lib/WslConsolePanel.svelte');
  const loadCommandPalette = () => import('./lib/CommandPalette.svelte');
  const lazyLabels = $derived({
    loadingLabel: $t('loading'),
    failedLabel: $t('workspace.loadFailed'),
    retryLabel: $t('retry'),
    closeLabel: $t('close'),
  });

  let waveformData = $state<number[]>([]);
  // Non-null ONLY when the decode failed — an empty array alone cannot distinguish "unreadable" from
  // "quiet". See loadWaveform.
  let waveformError = $state<string | null>(null);
  let currentTime = $state(0);
  let playerDuration = $state(0);
  let isAudioPlaying = $state(false);
  let segmentsLoading = $state(true);
  let sidebarOpen = $state(true);
  let statsOpen = $state(true);
  const SIDEBAR_MEDIA_QUERY = '(min-width: 900px)';
  const STATS_MEDIA_QUERY = '(min-width: 1200px)';
  type ReviewPanelSnapshot = {
    sidebarOpen: boolean;
    statsOpen: boolean;
    sidebarWide: boolean;
    statsWide: boolean;
  };
  let reviewPanelSnapshot: ReviewPanelSnapshot | null = null;
  function loadPanelWidth(key: string, fallback: number): number {
    if (typeof localStorage === 'undefined') return fallback;
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v >= 200 && v <= 600 ? v : fallback;
  }
  let sidebarWidth = $state(loadPanelWidth('cortex.sidebarWidth', 288));
  let statsWidth = $state(loadPanelWidth('cortex.statsWidth', 288));
  $effect(() => {
    if (typeof localStorage === 'undefined') return;
    localStorage.setItem('cortex.sidebarWidth', String(sidebarWidth));
    localStorage.setItem('cortex.statsWidth', String(statsWidth));
  });
  let batchSpeakerId = $state('');
  let editorTab = $state<'interactive' | 'raw'>('interactive');
  let historyPanel = $state<HistoryPanelApi | null>(null);
  let latestAgentReport = $state<AgentImportReport | null>(null);
  let latestAgentStageEvents = $state<AgentStageEvent[]>([]);

  let metadataReadinessEpoch = $state(0);
  const metadataCoordinator = createSegmentMetadataCoordinator({
    save: api.updateSegmentMetadataV1,
    applyServerTruth: (updated) =>
      segments.update((rows) =>
        rows.map((row) =>
          row.id === updated.segmentId
            ? { ...row, speakerId: updated.speakerId, alignmentJson: updated.alignmentJson }
            : row,
        ),
      ),
    onReadinessChanged: () => (metadataReadinessEpoch += 1),
  });
  let selectedMetadataReady = $derived.by(() => {
    void metadataReadinessEpoch;
    return metadataCoordinator.isReady($selectedSegment?.id ?? null);
  });

  const autosave = createAutosaveController<SpeechSegment>({
    targetId: () => get(selectedSegment)?.id ?? null,
    getRow: (id) => get(segments).find((s) => s.id === id) ?? null,
    save: (_row, fields, id) => metadataCoordinator.saveFields(id, fields),
    onError: (e) => notifications.error($t('notifications.saveFailed'), { cause: e }),
  });
  let tauriAvailable = $state(false);
  // Session view-state persistence: only start saving once the prior session has been restored, so
  // the initial restore->apply does not race a default-valued save over it.
  let sessionRestored = false;
  let sessionSaveTimeout: ReturnType<typeof setTimeout> | null = null;
  const VALID_SORT_ORDERS: SortOrder[] = [
    'newest',
    'oldest',
    'duration',
    'verified',
    'confidence',
    'activeLearning',
  ];

  // P3.2: a crashed directory import offered for resume at startup.
  let interruptedImport = $state<import('./lib/commands').ImportJob | null>(null);
  // B2: non-null when a corruption quarantine happened; the banner stays until dismissed this session.
  let quarantineNotice = $state<import('./lib/commands').QuarantineNotice | null>(null);
  async function resumeImport() {
    const job = interruptedImport;
    if (!job) return;
    interruptedImport = null;
    try {
      await api.resumeInterruptedImport();
      notifications.success($t('import.resumeStarted'));
    } catch (e) {
      notifications.error($t('import.resumeFailed'), { cause: e });
    }
  }
  async function dismissInterruptedImport() {
    const job = interruptedImport;
    if (!job) return;
    interruptedImport = null;
    try {
      await api.discardInterruptedImport(job.id);
    } catch (e) {
      console.error('Discard interrupted import failed:', e);
    }
  }

  async function acknowledgeQuarantine() {
    try {
      const moved = await api.acknowledgeQuarantine();
      notifications.success($t('db.quarantineAcknowledged', { count: String(moved) }));
      quarantineNotice = null;
    } catch (error) {
      notifications.error($t('db.quarantineAcknowledgeFailed'), { cause: error });
    }
  }

  async function restoreAndApplySession() {
    if (!tauriAvailable) return;
    try {
      const restored = await api.restoreSession();
      if (restored) {
        if (restored.search_query) searchQuery.set(restored.search_query);
        if (restored.sort_order && VALID_SORT_ORDERS.includes(restored.sort_order as SortOrder)) {
          sortOrder.set(restored.sort_order as SortOrder);
        }
        // M2.6/P1.5: restore the review filter + cursor (segments are already loaded at this point).
        // Only reselect a cursor that still exists in the loaded set, so a deleted segment is a no-op.
        if (restored.filter_verified !== null && restored.filter_verified !== undefined) {
          filterVerified.set(restored.filter_verified);
        }
        if (
          restored.selected_segment_id &&
          get(segments).some((s) => s.id === restored.selected_segment_id)
        ) {
          selectedSegmentId.set(restored.selected_segment_id);
        }
      }
    } catch (e) {
      console.error('Session restore failed:', e);
    } finally {
      sessionRestored = true;
    }
  }

  // Debounced persistence of the user's search query + sort order (survives a restart). Gated on
  // sessionRestored so applying the restored values on launch does not immediately re-save defaults.
  $effect(() => {
    const q = $searchQuery;
    const s = $sortOrder;
    const fv = $filterVerified;
    if (!sessionRestored || !tauriAvailable) return;
    if (sessionSaveTimeout) clearTimeout(sessionSaveTimeout);
    sessionSaveTimeout = setTimeout(() => {
      void api.saveSession(q, s, fv).catch((e) => console.error('Session save failed:', e));
    }, 800);
  });
  let datasetPromotionStage = $derived.by(
    () =>
      latestAgentReport?.summary.orchestrationStages?.find(
        (stage) => stage.stage === 'dataset_promotion',
      ) ?? null,
  );
  let trainingExportBlocked = $derived.by(() => datasetPromotionStage?.status === 'blocked');
  let trainingExportTitle = $derived.by(() => {
    if (!tauriAvailable) return $t('desktopRuntimeRequired');
    if (trainingExportBlocked) {
      return `${$t('exportHuggingface.blocked')}: ${datasetPromotionStage?.summary ?? ''}`;
    }
    if (datasetPromotionStage?.status === 'needs_review') {
      return `${$t('exportHuggingface.needsReview')}: ${datasetPromotionStage.summary}`;
    }
    return $t('exportHuggingface.label');
  });

  function requireDesktopRuntime(): boolean {
    if (tauriAvailable) return true;
    notifications.info($t('desktopRuntimeRequired'));
    return false;
  }

  function scheduleAutoSave(edits: api.SegmentMetadataFields) {
    autosave.schedule(edits);
  }

  // Transient word-bounded playback window: while set, the player plays exactly
  // [wordStartOverride, wordEndOverride] — tap a word, hear that word (and Loop loops the word, since
  // BOTH bounds are overridden). Cleared when playback stops, on a manual seek, or on a selection
  // change (chunks of one source share an audioPath, so the player does NOT reset on switch — a stale
  // word window would otherwise bleed across chunks).
  let wordStartOverride = $state<number | null>(null);
  let wordEndOverride = $state<number | null>(null);
  function clearWordOverride() {
    wordStartOverride = null;
    wordEndOverride = null;
  }
  $effect(() => {
    if (!isAudioPlaying) clearWordOverride();
  });
  $effect(() => {
    const id = $selectedSegmentId; // any selection change ends word-playback mode AND reseats the view
    clearWordOverride();
    if (id) untrack(() => metadataCoordinator.forget(id));
    // Per-selection VIEW setup for EVERY selection path. selectSegment() used to set these inline, but
    // Store-only jumps set the selectedSegmentId store and bypassed selectSegment(). When
    // chunks share ONE source audioPath (single-file import), the AudioPlayer then kept playing from the OLD
    // position straight through the new clip's endTime (the renderer advanced through the wrong clip while
    // the UI showed the new one), and the waveform bars / tap-a-word data stayed on the previous chunk. Centralizing it
    // here fixes every path. `get(selectedSegment)` reads WITHOUT subscribing, so this fires once per
    // selection IDENTITY, not on unrelated segment-data updates (e.g. a mid-review re-alignment).
    const seg = id ? get(selectedSegment) : null;
    if (seg) {
      currentTime = chunkPlaybackRange(parseSourceMeta(seg.alignmentJson)).startTime;
      wordTimestamps.set(parseWordTimestamps(seg.alignmentJson));
      loadWaveform(seg.audioPath, seg.alignmentJson);
      // List pages deliberately omit large alignment/evidence JSON. Hydrate only the selected row,
      // then reseat timing/waveform state if the user has not moved to another clip meanwhile.
      void segments
        .hydrate(seg.id)
        .then((full) => {
          if (get(selectedSegmentId) !== full.id) return;
          metadataCoordinator.remember(full.id, {
            speakerId: full.speakerId,
            alignmentJson: full.alignmentJson,
          });
          metadataCoordinator.pruneExcept([full.id, ...autosave.retainedIds()]);
          currentTime = chunkPlaybackRange(parseSourceMeta(full.alignmentJson)).startTime;
          wordTimestamps.set(parseWordTimestamps(full.alignmentJson));
          void loadWaveform(full.audioPath, full.alignmentJson);
        })
        .catch((error) => {
          if (get(selectedSegmentId) === seg.id) {
            notifications.error($t('notifications.loadSegmentsFailed'), {
              cause: error,
            });
          }
        });
    }
  });
  // Tap a word → play exactly that word. Human corrections belong exclusively to Review Mode.
  function playWordClip(w: WordTimestamp) {
    const b = wordPlayBounds(w, chunkStartTime, chunkEndTime);
    // Idempotent play: a double-click dispatches click,click,dblclick — don't hard-reseek the same
    // word 2–3× (a stutter).
    if (!(isAudioPlaying && wordStartOverride === b.start && wordEndOverride === b.end)) {
      wordStartOverride = b.start;
      wordEndOverride = b.end;
      currentTime = b.start;
      isAudioPlaying = true;
    }
  }

  let chunkStartTime = $derived.by(() => {
    const meta = parseSourceMeta($selectedSegment?.alignmentJson);
    return chunkPlaybackRange(meta).startTime;
  });

  let chunkEndTime = $derived.by(() => {
    const meta = parseSourceMeta($selectedSegment?.alignmentJson);
    return chunkPlaybackRange(meta).endTime;
  });

  // Clip-relative time for the waveform playhead: the waveform bars are the selected chunk's
  // window, so the playhead must run 0 → chunk-length, not against the whole source file.
  let chunkClipLength = $derived(
    chunkEndTime > chunkStartTime ? chunkEndTime - chunkStartTime : playerDuration,
  );
  let chunkClipPosition = $derived(
    chunkEndTime > chunkStartTime
      ? Math.max(0, Math.min(currentTime - chunkStartTime, chunkClipLength))
      : currentTime,
  );

  let chunkLabel = $derived.by(() => {
    const meta = parseSourceMeta($selectedSegment?.alignmentJson);
    if (!meta || meta.chunkCount <= 1) return null;
    return `${meta.chunkIndex + 1} / ${meta.chunkCount}`;
  });

  let showHotkeyOverlay = $state(false);

  $effect(() => {
    const isRtl = $locale === 'ckb';
    document.documentElement.dir = isRtl ? 'rtl' : 'ltr';
    document.documentElement.lang = $locale;
  });

  $effect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Alt') {
        showHotkeyOverlay = true;
      }
    };
    const handleKeyUp = (e: KeyboardEvent) => {
      if (e.key === 'Alt') {
        showHotkeyOverlay = false;
      }
    };
    const handleBlur = () => {
      showHotkeyOverlay = false;
    };
    window.addEventListener('keydown', handleKeyDown);
    window.addEventListener('keyup', handleKeyUp);
    window.addEventListener('blur', handleBlur);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
      window.removeEventListener('keyup', handleKeyUp);
      window.removeEventListener('blur', handleBlur);
    };
  });

  $effect(() => {
    const mqStats = window.matchMedia(STATS_MEDIA_QUERY);
    const mqSidebar = window.matchMedia(SIDEBAR_MEDIA_QUERY);

    function onStatsChange(e: MediaQueryListEvent | MediaQueryList) {
      if (reviewPanelSnapshot !== null) return;
      statsOpen = e.matches;
    }
    function onSidebarChange(e: MediaQueryListEvent | MediaQueryList) {
      if (reviewPanelSnapshot !== null) return;
      sidebarOpen = e.matches;
    }

    onStatsChange(mqStats);
    onSidebarChange(mqSidebar);

    mqStats.addEventListener('change', onStatsChange);
    mqSidebar.addEventListener('change', onSidebarChange);

    return () => {
      mqStats.removeEventListener('change', onStatsChange);
      mqSidebar.removeEventListener('change', onSidebarChange);
    };
  });

  // Unlisten for the native window close-request hook (registered in onMount, cleared in onDestroy).
  let closeUnlisten: (() => void) | undefined;
  let healthInterval: ReturnType<typeof setInterval> | undefined;

  // Surface the safety-net failures that were previously invisible (the auto-snapshot net can go down
  // for months and disk exhaustion was computed then dropped). Runs at startup and on an interval; the
  // file log (data_dir/logs) has the detail. Best-effort — a failed health probe never disrupts the app.
  async function checkHealthAndWarn() {
    try {
      const h = await api.appHealth();
      if (!h) return; // defensive: a null health report must never crash the health loop
      const GiB = 1024 ** 3;
      if ((h.snapshot_consecutive_failures ?? 0) >= 3) {
        notifications.error(
          $t('notifications.snapshotFailing', { count: String(h.snapshot_consecutive_failures) }),
        );
      }
      // Staleness (true-10 audit): the failure counter only sees Err — a wedged/restarting backup
      // that never errors, or a dead loop thread, showed nothing. last_success aging past 3
      // intervals (30 min) on a non-empty library is the honest stall signal.
      const lastOk = h.snapshot_last_success_epoch_secs;
      if (
        lastOk != null &&
        Date.now() / 1000 - lastOk > 3 * 600 &&
        ($segmentStats?.total ?? 0) > 0
      ) {
        notifications.error(
          $t('notifications.snapshotStale', {
            minutes: String(Math.round((Date.now() / 1000 - lastOk) / 60)),
          }),
        );
      }
      if (h.free_disk_bytes != null && h.free_disk_bytes < 2 * GiB) {
        notifications.error(
          $t('notifications.lowDisk', { gb: (h.free_disk_bytes / GiB).toFixed(1) }),
        );
      }
      if ((h.missing_models?.length ?? 0) > 0) {
        notifications.error(
          $t('notifications.missingModels', { models: h.missing_models.join(', ') }),
        );
      }
    } catch (e) {
      console.error('health check failed', e);
    }
  }

  onMount(async () => {
    tauriAvailable = isTauriRuntime();
    const km = initKeyboardManager();
    // True-10 audit BLOCKER fix: while a review surface owns the keyboard (Review & Correct mode or
    // the Review Inbox overlay), every global shortcut acts on the HIDDEN curate selection —
    // Ctrl+Enter/Ctrl+D silently verified an invisible segment with no human decision (export-
    // eligible gold nobody reviewed), Ctrl+T machine-overwrote it, Delete opened a confirm dialog
    // UNDER the inbox. The manager suppresses all non-allowInReview shortcuts whenever this probe is
    // true; probing at dispatch time keeps it correct for future shortcuts too.
    km.setReviewSurfaceProbe(() => viewMode === 'review' || $showReviewInbox);
    registerShortcuts(km);
    setImportCompleteHandler(async (payload) => {
      try {
        await loadSegments();
        await loadLatestAgentReport();
        await loadLatestAgentStageEvents();
        if (payload.segmentIds?.length) {
          selectedSegmentId.set(payload.segmentIds[0]);
        }
        statusMessage.set($t('ready'));
        if (payload.source === 'file') {
          if (payload.failed > 0) {
            notifications.error($t('openFile.failed'));
          } else if (payload.segmentCount && payload.segmentCount > 1) {
            notifications.success(
              $t('openFile.multiChunk', { count: String(payload.segmentCount) }),
            );
          } else if (payload.succeeded > 0) {
            notifications.success($t('openFile.imported'));
          }
        } else {
          statusMessage.set($t('importComplete'));
        }
      } catch (e) {
        console.error('Import complete handler error:', e);
        notifications.error($t('notify.refreshFailedImport'), { cause: e });
      } finally {
        if (payload.source === 'file') {
          endOperation('open-file');
        } else {
          endOperation('import');
        }
        isProcessing.set(false);
      }
    });
    setBatchCompleteHandler(async (payload) => {
      try {
        if (payload.operation === 'transcribe') {
          await loadSegments();
          statusMessage.set($t('ready'));
          endOperation('batch-transcribe');
        } else if (payload.operation === 'verify') {
          await loadSegments();
          statusMessage.set($t('ready'));
          endOperation('batch-verify');
        } else if (payload.operation === 'normalize') {
          await loadSegments();
          statusMessage.set($t('ready'));
          endOperation('batch-normalize');
        }
      } catch (e) {
        console.error('Batch complete handler error:', e);
        notifications.error($t('notify.refreshFailedBatch'), { cause: e });
      } finally {
        isProcessing.set(false);
      }
    });
    if (isTauriRuntime()) {
      try {
        await startEventListeners();
      } catch (e) {
        notifications.error($t('eventListenersFailed'), { cause: e });
      }
      await loadSegments();
      await loadLatestAgentReport();
      await loadLatestAgentStageEvents();
      await loadSettings();
      await restoreAndApplySession();
      // P3.2: a still-'running' import job at startup means a crash interrupted a directory import.
      interruptedImport = await api.getInterruptedImport().catch(() => null);
      // B2: a past corruption event quarantined a database file — say so LOUDLY, with the restore
      // count, instead of letting the owner work on silently in an empty library.
      quarantineNotice = await api
        .getQuarantineNotice()
        .then((n) => (n.quarantinedFiles.length > 0 ? n : null))
        .catch(() => null);
      // Surface silent safety-net failures (auto-snapshot down, low disk, missing models) at startup
      // and every 5 minutes thereafter.
      void checkHealthAndWarn();
      healthInterval = setInterval(() => void checkHealthAndWarn(), 5 * 60 * 1000);
      // Surface a crash from the PREVIOUS session (shown once) — a mid-review panic otherwise relaunches
      // to a normal-looking app with no hint that a crash (and possible lost unsaved edit) happened.
      api
        .takeLastCrash()
        .then((crash) => {
          if (crash) {
            notifications.error(
              $t('notifications.previousCrash', {
                summary: formatPublicErrorReference(crash) ?? $t('errors.unknown'),
              }),
              { cause: crash },
            );
          }
        })
        .catch((e) => console.error('crash check failed', e));
      // Flush pending metadata AND revision-bound review drafts before native close. onDestroy does
      // not reliably run on a Tauri window close. A bounded failure keeps the window open and visible
      // instead of reporting a successful close after silently losing human text.
      try {
        closeUnlisten = await registerDurableCloseGuard({
          flush: async () => {
            await Promise.all([flushReviewDrafts(), autosave.flushAsync()]);
          },
          timeoutMs: 10_000,
          onFlushError: (flushError) => {
            notifications.error($t('review.closeDraftFailed'), {
              cause: flushError,
              publicDetail: $t('review.closeDraftFailedHint'),
            });
          },
          onCloseError: (closeError) => {
            notifications.error($t('review.closeFailed'), {
              cause: closeError,
              publicDetail: $t('review.closeFailedHint'),
            });
          },
        });
      } catch (e) {
        console.error('Failed to register close-request autosave flush:', e);
      }
    } else {
      segments.set([]);
      segmentsLoading = false;
      statusMessage.set($t('ready'));
    }
  });

  onDestroy(() => {
    stopEventListeners();
    globalKeyboardManager?.destroy();
    closeUnlisten?.();
    if (healthInterval) clearInterval(healthInterval);
    // Flush (not just cancel) pending non-review metadata on teardown.
    autosave.flush();
    if (sessionSaveTimeout) clearTimeout(sessionSaveTimeout);
  });

  function navigateSegment(direction: 'up' | 'down') {
    const list = $filteredSegments;
    if (list.length === 0) return;
    const currentId = $selectedSegmentId;
    const currentIndex = list.findIndex((s) => s.id === currentId);
    const startIdx = currentIndex < 0 ? (direction === 'down' ? -1 : list.length) : currentIndex;
    const targetIndex =
      direction === 'down' ? Math.min(list.length - 1, startIdx + 1) : Math.max(0, startIdx - 1);
    selectSegment(list[targetIndex]);
  }

  let viewMode = $state<'curate' | 'insights' | 'review'>('curate');
  // One-time dismiss for the "N clips ready to review" nudge banner (resets each app launch).
  let reviewNudgeDismissed = $state(false);
  let showCommandPalette = $state(false);
  function registerShortcuts(km: ReturnType<typeof initKeyboardManager>) {
    const shortcuts: Shortcut[] = [
      {
        key: 'o',
        ctrl: true,
        description: 'Open audio file',
        descriptionKey: 'openAudioFile',
        action: handleOpenFile,
        category: 'file',
      },
      {
        key: 'i',
        ctrl: true,
        description: 'Import directory',
        descriptionKey: 'importDirectory',
        action: handleImport,
        category: 'file',
      },
      {
        key: 't',
        ctrl: true,
        description: 'Transcribe selected',
        descriptionKey: 'transcribe',
        action: handleTranscribe,
        category: 'file',
      },
      {
        // True-10 audit: the primary daily workspace (Review & Correct) was mouse-only — no hotkey,
        // absent from the palette and the Ctrl+/ help.
        key: 'e',
        ctrl: true,
        shift: true,
        description: 'Review & correct',
        descriptionKey: 'reviewCorrect.label',
        action: enterReviewMode,
        category: 'navigation',
      },
      {
        key: 'z',
        ctrl: true,
        description: 'Undo',
        descriptionKey: 'undo',
        action: () => handleUndo(),
        category: 'edit',
        // handleUndo self-guards on the review surfaces with a helpful "use Backspace here" notice —
        // let it through so the reviewer learns the paired-undo model instead of silence.
        allowInReview: true,
      },
      {
        key: 'z',
        ctrl: true,
        shift: true,
        description: 'Redo',
        descriptionKey: 'redo',
        action: () => handleRedo(),
        category: 'edit',
        allowInReview: true, // handleRedo self-guards on review surfaces (no-op there)
      },
      {
        key: 'Delete',
        description: 'Delete segment',
        descriptionKey: 'deleteSegment',
        action: handleDeleteWithConfirm,
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
        // Don't open Settings UNDER the Review Inbox overlay (z-50 vs z-[100]) where it would be
        // invisible while its close-time auto-save writes a stale snapshot. Require the inbox closed.
        action: () => {
          if (!$showReviewInbox) openSettings();
        },
        category: 'navigation',
      },
      {
        key: 'v',
        ctrl: true,
        shift: true,
        description: 'Validate dataset',
        descriptionKey: 'validateDataset',
        action: openValidationPanel,
        category: 'navigation',
      },
      {
        key: 'r',
        ctrl: true,
        shift: true,
        description: 'Open Review Inbox',
        descriptionKey: 'reviewInbox',
        action: openReviewInbox,
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
        action: () => (sidebarOpen = !sidebarOpen),
        category: 'navigation',
      },
      {
        key: 'd',
        shift: true,
        description: 'Toggle stats dashboard',
        descriptionKey: 'toggleStats',
        action: () => (statsOpen = !statsOpen),
        category: 'navigation',
      },
      {
        key: 'j',
        description: 'Next segment',
        descriptionKey: 'nextSegment',
        action: () => navigateSegment('down'),
        category: 'navigation',
      },
      {
        key: 'k',
        description: 'Previous segment',
        descriptionKey: 'prevSegment',
        action: () => navigateSegment('up'),
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
        action: () => (isAudioPlaying = !isAudioPlaying),
        category: 'playback',
      },
      {
        key: 'ArrowLeft',
        description: 'Rewind 5s',
        descriptionKey: 'rewind',
        action: () => {
          clearWordOverride(); // a manual scrub leaves word-playback mode
          currentTime = Math.max(0, currentTime - 5);
        },
        category: 'playback',
      },
      {
        key: 'ArrowRight',
        description: 'Forward 5s',
        descriptionKey: 'forward',
        action: () => {
          clearWordOverride();
          currentTime = Math.min(playerDuration, currentTime + 5);
        },
        category: 'playback',
      },
      {
        key: 'k',
        ctrl: true,
        description: 'Command palette',
        descriptionKey: 'cmdk.title',
        action: () => (showCommandPalette = true),
        category: 'general',
        allowInEditable: true,
        // The palette renders above both review surfaces and its commands re-check their own
        // preconditions — the one global that is genuinely review-safe.
        allowInReview: true,
      },
    ];
    km.registerAll(shortcuts);
  }

  function notifyActionableError(error: unknown, fallbackMessage: string) {
    const parsed = parseActionableError(error, fallbackMessage);
    notifications.error(parsed.message, {
      cause: error,
      publicDetail: parsed.detail,
      action: parsed.action,
    });
  }

  function openValidationPanel() {
    if (!requireDesktopRuntime()) return;
    if ($isProcessing || $segmentStats.total === 0) return;
    showValidationPanel.set(true);
  }

  function openReviewInbox() {
    if (!requireDesktopRuntime()) return;
    showReviewInbox.set(true);
  }

  // Enter the focused, one-clip-at-a-time Review & Correct workspace (the fast lane for
  // fixing transcripts). Mirrors the ActivityRail 'review' behaviour: collapse the side
  // panels so the reviewer sees only the clip + the edit box.
  function enterReviewMode() {
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

  let reviewExitSeq = 0;
  async function leaveReviewMode(nextView: 'curate' | 'insights' = 'curate') {
    const exitSeq = ++reviewExitSeq;
    try {
      // Keep the review surface (and its exact clip/editor state) mounted until the visible draft is
      // durable. onDestroy cannot be awaited; exit then close could unregister an in-flight flusher.
      await flushReviewDrafts();
    } catch (error) {
      if (exitSeq === reviewExitSeq) {
        notifications.error($t('review.closeDraftFailed'), {
          cause: error,
          publicDetail: $t('review.closeDraftFailedHint'),
        });
      }
      return;
    }
    // A second workspace choice made during the flush is the user's current intent. Only that latest
    // request may unmount ReviewMode; this also prevents two concurrent exits from racing the view.
    if (exitSeq !== reviewExitSeq || viewMode !== 'review') return;
    const sidebarWide = window.matchMedia(SIDEBAR_MEDIA_QUERY).matches;
    const statsWide = window.matchMedia(STATS_MEDIA_QUERY).matches;
    const snapshot = reviewPanelSnapshot;

    // Preserve the user's exact pre-review panel choices when the viewport class is unchanged. If
    // they resized during review, apply the current responsive defaults instead of restoring a panel
    // that no longer fits.
    sidebarOpen = snapshot?.sidebarWide === sidebarWide ? snapshot.sidebarOpen : sidebarWide;
    statsOpen = snapshot?.statsWide === statsWide ? snapshot.statsOpen : statsWide;
    reviewPanelSnapshot = null;
    viewMode = nextView;
  }

  function selectWorkspace(id: string) {
    if (id === 'settings') openSettings();
    else if (id === 'review') enterReviewMode();
    else if (viewMode === 'review') leaveReviewMode(id as 'curate' | 'insights');
    else viewMode = id as 'curate' | 'insights';
  }

  function openWslConsole() {
    if (!requireDesktopRuntime()) return;
    showWslConsole.set(true);
  }

  function openSpeakerPanel() {
    if (!requireDesktopRuntime()) return;
    showSpeakerPanel.set(true);
  }

  function openDatasetMerge() {
    if (!requireDesktopRuntime()) return;
    showDatasetMerge.set(true);
  }

  async function loadSettings() {
    try {
      settings.set(await api.getSettings());
    } catch (e) {
      notifications.error($t('settingsLoadFailed'), { cause: e });
    }
  }

  async function loadSegments() {
    // Route through the store's guarded load() (it owns a loadSeq counter + conformal refresh) so
    // this refresh path can't interleave-overwrite a concurrent segments.load() — e.g. from a WSL
    // refinement completing while a batch op finishes — with an older getSegments() result.
    segmentsLoading = true;
    try {
      await segments.load();
    } finally {
      segmentsLoading = false;
    }
  }

  async function loadLatestAgentReport() {
    if (!tauriAvailable) {
      latestAgentReport = null;
      return;
    }
    try {
      const reports = await api.listAgentImportReports(1);
      latestAgentReport = reports[0] ?? null;
    } catch (e) {
      notifications.error($t('agentReport.loadFailed'), { cause: e });
    }
  }

  async function loadLatestAgentStageEvents() {
    if (!tauriAvailable) {
      latestAgentStageEvents = [];
      agentPipelineStages.set([]);
      return;
    }
    try {
      const events = await api.listAgentStageEvents(latestAgentReport?.agentRunId ?? null, 25);
      latestAgentStageEvents = Array.isArray(events) ? events : [];
      agentPipelineStages.set(
        latestAgentStageEvents.slice(-8).map((event) => ({
          stage: event.stage,
          status: event.status,
          file: event.file,
          detail: event.detail,
          current: event.current,
          total: event.total,
          updatedAt: new Date(event.createdAt).getTime() || Date.now(),
        })),
      );
    } catch (e) {
      latestAgentStageEvents = [];
      notifications.error($t('agentReport.stageLoadFailed'), { cause: e });
    }
  }

  function trainingExportBlockDetail(stage: AgentOrchestrationStage | null): string | undefined {
    if (!stage) return undefined;
    const blockers = stage.blockers.slice(0, 4).join(', ');
    return blockers ? `${stage.summary} (${blockers})` : stage.summary;
  }

  function agenticReadinessDetail(readiness: AgenticReadiness): string {
    return readiness.checks
      .filter((check) => check.status !== 'ready')
      .slice(0, 3)
      .map((check) => `${check.label}: ${check.detail}`)
      .join(' ');
  }

  async function warnAgenticReadinessBeforeImport() {
    if (!tauriAvailable) return;
    try {
      const readiness = await api.checkAgenticReadiness();
      // WARN only when the pipeline genuinely CAN'T run (blocked). "degraded" still works (e.g. a
      // partial config) so surface it as a gentle INFO, not an alarming warning on every import. The
      // common offline case now reports "ready" (cloud cross-check is opt-in, not a degradation).
      if (readiness.status === 'blocked') {
        notifications.warning($t('agenticReadiness.blocked'), {
          detail: agenticReadinessDetail(readiness),
        });
      } else if (readiness.status === 'degraded') {
        notifications.info($t('agenticReadiness.degraded'), {
          detail: agenticReadinessDetail(readiness),
        });
      }
    } catch (e) {
      notifications.warning($t('agenticReadiness.checkFailed'), { cause: e });
    }
  }

  // Round-25 #10: a synchronous re-entry guard that covers the window BEFORE isProcessing is set —
  // i.e. while the native picker and the (awaited) agentic-readiness IPC are pending. Without it, the
  // Ctrl+O/Ctrl+I shortcuts (not gated by DOM state) could fire a second import that the backend then
  // rejects with a confusing "Import already in progress" toast.
  let importStarting = false;

  async function handleOpenFile() {
    if ($isProcessing || importStarting) return;
    if (!requireDesktopRuntime()) return;
    importStarting = true;
    try {
      const path = await api.openAudioFile();
      if (!path) return;
      await warnAgenticReadinessBeforeImport();
      startOperation('open-file');
      isProcessing.set(true);
      pipelinePhase.set('importing');
      filesProcessed.set(0);
      pipelineTotal.set(0);
      pipelineCurrentFile.set('');
      pipelineStatus.set('');
      statusMessage.set($t('pipeline.importing'));
      await api.importAudioFile(path);
    } catch (e) {
      notifyActionableError(e, $t('openFile.failed'));
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('open-file');
    } finally {
      importStarting = false;
    }
  }

  async function handleImport() {
    if ($isProcessing || importStarting) return;
    if (!requireDesktopRuntime()) return;
    importStarting = true;
    startOperation('import');
    try {
      await warnAgenticReadinessBeforeImport();
      isProcessing.set(true);
      pipelinePhase.set('importing');
      filesProcessed.set(0);
      pipelineTotal.set(0);
      pipelineCurrentFile.set('');
      pipelineStatus.set('');
      statusMessage.set($t('pipeline.importing'));
      await api.importDirectory();
    } catch (e) {
      // Cancelling the native folder picker rejects with "No directory selected" — a routine
      // cancel, not a failure. Reset quietly instead of flashing a red "Import failed" toast.
      const cancelled = formatUnknownError(e).includes('No directory selected');
      if (!cancelled) {
        notifyActionableError(e, $t('importFailed'));
      }
      statusMessage.set(cancelled ? $t('ready') : $t('importFailed'));
      isProcessing.set(false);
      pipelinePhase.set('idle');
      pipelineCurrentFile.set('');
      pipelineStatus.set('');
      pipelineTotal.set(0);
      filesProcessed.set(0);
      endOperation('import');
    } finally {
      importStarting = false;
    }
  }

  // When the OmniASR-7B champion is unavailable, stop and retry only that engine. Optional local
  // engines remain available after explicitly selecting a non-champion mode in Settings; a runtime
  // failure must never turn into a convenient one-click downgrade of the production transcript.
  function promptChampionRetry(retryChampion: () => void) {
    showConfirmDialog.set({
      title: $t('asr.championUnavailableTitle'),
      message: $t('asr.championUnavailableMessage'),
      confirmLabel: $t('asr.tryAgain'),
      danger: false,
      onConfirm: retryChampion,
    });
  }

  async function handleTranscribe() {
    const seg = $selectedSegment;
    if (!seg || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
    if (seg.verified || seg.humanDecision) {
      notifications.info($t('asr.reopenBeforeRetranscribe'));
      return;
    }
    startOperation('transcribe');
    isProcessing.set(true);
    pipelinePhase.set('transcribing');
    statusMessage.set($t('transcribing'));
    try {
      const result = await api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id);
      // `transcribe_segment` commits the complete champion result server-side after every enabled
      // refinement succeeds. Never whole-row-upsert the pre-inference UI row here: alignment,
      // provenance, or a concurrent review decision may have changed during the long 7B call.
      if ($settings.autoAlign) {
        // VERBATIM LAW: align against the champion's verbatim output — timing the refined
        // paraphrase would stamp confident word timings onto words the speaker never said.
        const alignText = result.rawTranscript;
        if (alignText?.trim()) {
          try {
            const ts = await api.alignSegment(seg.audioPath, alignText, seg.alignmentJson, seg.id);
            wordTimestamps.set(ts);
          } catch (alignError) {
            notifications.error($t('notifications.alignmentFailed'), {
              cause: alignError,
            });
          }
        }
      }
      await loadSegments();
      notifications.success($t('notifications.transcriptionComplete'));
    } catch (e) {
      // The champion (7B) is the production engine. If it is down, fail closed and retry only it.
      if (api.is7bUnavailableError(e)) {
        promptChampionRetry(handleTranscribe);
      } else {
        notifyActionableError(e, $t('errors.transcriptionFailed'));
      }
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('transcribe');
    }
  }

  async function handleUndo() {
    if (!requireDesktopRuntime()) return;
    // The global history stack records only updateSegment, NOT record_human_decision. On the review
    // surfaces (Review & Correct, and the Inbox overlay) a global Ctrl+Z would revert `verified` but
    // leave the human_decision row — splitting state (the clip re-enters the queue while the DB says it
    // was decided, and the confidence flywheel was fed a decision that is now retracted). Those surfaces
    // have their OWN paired undo (Backspace = clearHumanDecision + restore); defer to it here.
    if (viewMode === 'review' || $showReviewInbox) {
      notifications.info($t('notifications.undoInReview'));
      return;
    }
    try {
      const description = await historyStore.undo();
      notifications.info(
        $t('notifications.undone', { what: description ?? $t('notifications.lastActionReverted') }),
      );
      await loadSegments();
      if (historyPanel) {
        historyPanel.recordAction(`Reverted: ${description ?? 'action'}`, 'edit');
      }
    } catch (e) {
      notifications.error($t('notifications.undoFailed'), { cause: e });
    }
  }

  async function handleRedo() {
    if (!requireDesktopRuntime()) return;
    // Same split-state hazard as handleUndo: the global redo has no meaning on the review surfaces.
    if (viewMode === 'review' || $showReviewInbox) return;
    try {
      const description = await historyStore.redo();
      notifications.info(
        $t('notifications.redone', {
          what: description ?? $t('notifications.lastActionReapplied'),
        }),
      );
      await loadSegments();
      if (historyPanel) {
        historyPanel.recordAction(`Redone: ${description ?? 'action'}`, 'edit');
      }
    } catch (e) {
      notifications.error($t('notifications.redoFailed'), { cause: e });
    }
  }

  function handleDeleteWithConfirm() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;
    showConfirmDialog.set({
      title: $t('deleteSegment'),
      message: $t('deleteSegmentConfirm').replace(
        '{name}',
        seg.audioPath.split(/[/\\]/).pop() ?? '',
      ),
      onConfirm: handleDelete,
    });
  }

  async function handleSaveSpeaker() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;
    const hadPendingSave = autosave.pendingId() === seg.id;
    try {
      await autosave.flushAsync();
      if (!hadPendingSave) {
        await metadataCoordinator.saveFields(seg.id, { speakerId: seg.speakerId });
      }
      notifications.success($t('speaker.saved'));
    } catch (e) {
      if (!hadPendingSave) notifications.error($t('notifications.saveFailed'), { cause: e });
    }
  }

  async function handleExport() {
    if (!requireDesktopRuntime()) return;
    try {
      const format = $settings.exportFormat;
      const ext =
        format === 'parquet'
          ? 'parquet'
          : format === 'csv'
            ? 'csv'
            : format === 'jsonl'
              ? 'jsonl'
              : 'json';
      const filters: Array<{ name: string; extensions: string[] }> = [
        { name: 'JSON', extensions: ['json'] },
        { name: 'JSONL', extensions: ['jsonl'] },
        { name: 'CSV', extensions: ['csv'] },
      ];
      filters.push({ name: 'Parquet', extensions: ['parquet'] });
      const path = await saveFile({
        filters,
        defaultPath: `cortex-dataset.${ext}`,
      });
      if (path) {
        const lower = path.toLowerCase();
        const resolvedFormat = lower.endsWith('.parquet')
          ? 'parquet'
          : lower.endsWith('.csv')
            ? 'csv'
            : lower.endsWith('.jsonl')
              ? 'jsonl'
              : 'json';
        await api.exportDataset(path, resolvedFormat);
        notifications.success($t('exportDataset.success'), { detail: path });
      }
    } catch (e) {
      notifications.error($t('exportDataset.failed'), { cause: e });
    }
  }

  async function handleExportTranscript() {
    if ($isProcessing || $segmentStats.total === 0) return;
    if (!requireDesktopRuntime()) return;
    try {
      const path = await saveFile({
        filters: [
          { name: 'SubRip subtitles', extensions: ['srt'] },
          { name: 'WebVTT subtitles', extensions: ['vtt'] },
          { name: 'Plain text', extensions: ['txt'] },
        ],
        defaultPath: 'cortex-transcript.srt',
      });
      if (path) {
        const lower = path.toLowerCase();
        const format: 'txt' | 'srt' | 'vtt' = lower.endsWith('.vtt')
          ? 'vtt'
          : lower.endsWith('.txt')
            ? 'txt'
            : 'srt';
        await api.exportTranscript(path, format);
        notifications.success($t('exportTranscript.success'), { detail: path });
      }
    } catch (e) {
      notifications.error($t('exportTranscript.failed'), { cause: e });
    }
  }

  async function handleExportHuggingface() {
    if ($isProcessing || $segmentStats.total === 0) return;
    if (!requireDesktopRuntime()) return;
    if (trainingExportBlocked) {
      notifications.warning($t('exportHuggingface.blocked'), {
        detail: trainingExportBlockDetail(datasetPromotionStage),
      });
      return;
    }
    if (datasetPromotionStage?.status === 'needs_review') {
      notifications.warning($t('exportHuggingface.needsReview'), {
        detail: trainingExportBlockDetail(datasetPromotionStage),
      });
    }
    try {
      const dir = await chooseDirectory();
      if (!dir) return;
      await api.exportHuggingfaceDataset(dir);
      notifications.success($t('exportHuggingface.success'), { detail: dir });
    } catch (e) {
      notifications.error($t('exportHuggingface.failed'), { cause: e });
    }
  }

  async function handleExportAudio() {
    if (!requireDesktopRuntime()) return;
    // isVerifiedGood, NOT raw s.verified: markBad finalizes a REJECTED clip with verified=true (to pull
    // it out of the review queue) + humanDecision='reject', so a plain s.verified filter would ship
    // human-rejected clips and their bad transcripts into the "verified audio" dataset as if human-gold.
    // Mirrors the SettingsPanel export and the Rust export_dataset (!is_human_rejected) so counts match.
    const verifiedIds = $segments.filter((s) => isVerifiedGood(s)).map((s) => s.id);
    if (verifiedIds.length === 0) {
      notifications.warning($t('exportAudio.noVerified'));
      return;
    }
    try {
      const dir = await chooseDirectory();
      if (!dir) return;

      startOperation('export-audio');
      isProcessing.set(true);
      statusMessage.set($t('exportAudio.progress'));
      const result = await api.exportAudio(verifiedIds, {
        output_dir: dir,
        format: api.AudioExportFormat.Wav,
        sample_rate: 16000,
        include_metadata: true,
      });

      if (result.failed > 0) {
        notifications.warning(
          $t('exportAudio.partial', {
            succeeded: String(result.succeeded),
            failed: String(result.failed),
          }),
          { detail: result.output_dir },
        );
      } else {
        notifications.success($t('exportAudio.success', { count: String(result.succeeded) }), {
          detail: result.output_dir,
        });
      }
    } catch (e) {
      notifications.error($t('exportAudio.failed'), { cause: e });
    } finally {
      isProcessing.set(false);
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set($t('ready'));
      endOperation('export-audio');
    }
  }

  async function resolveViewIds(
    transcriptState: 'any' | 'real' | 'missing' = 'any',
    verified: boolean | null = $filterVerified,
    query: string | null = $searchQuery.trim() || null,
  ): Promise<string[] | null> {
    try {
      return await api.getSegmentIdsForView({ verified, query, transcriptState });
    } catch (error) {
      notifications.error($t('notifications.loadSegmentsFailed'), {
        cause: error,
      });
      return null;
    }
  }

  async function handleBatchTranscribe(mode: 'empty' | 'selected' | 'filtered') {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;

    const ids =
      mode === 'empty'
        ? await resolveViewIds('missing', null, null)
        : mode === 'selected'
          ? $selectedSegmentId
            ? [$selectedSegmentId]
            : []
          : await resolveViewIds();
    if (ids === null) return;

    if (mode === 'selected' && !$selectedSegmentId) {
      notifications.warning($t('batchTranscribe.noSelection'));
      return;
    }
    if (ids.length === 0) {
      notifications.info($t('batchTranscribe.nothingToTranscribe'));
      return;
    }

    startOperation('batch-transcribe');
    pipelinePhase.set('transcribing');
    isProcessing.set(true);
    statusMessage.set($t('batchTranscribe.progress', { n: String(ids.length) }));
    try {
      await api.batchTranscribe(ids);
    } catch (e) {
      notifyActionableError(e, $t('batchTranscribe.failed'));
      pipelinePhase.set('idle');
      isProcessing.set(false);
      statusMessage.set($t('ready'));
      endOperation('batch-transcribe');
    }
  }

  async function handleBatchAssignSpeaker() {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
    const speaker = batchSpeakerId.trim();
    if (!speaker) {
      notifications.warning($t('batchAssignSpeaker.noSpeaker'));
      return;
    }
    const ids = await resolveViewIds();
    if (ids === null) return;
    if (ids.length === 0) {
      notifications.info($t('batchAssignSpeaker.nothingToAssign'));
      return;
    }
    startOperation('batch-assign-speaker');
    isProcessing.set(true);
    batchProgress.set({ status: 'running', completed: 0, total: ids.length, percent: 0 });
    statusMessage.set($t('batchAssignSpeaker.progress', { n: String(ids.length) }));
    try {
      const result = await api.assignSpeakersV1({ ids, targetSpeakerId: speaker });
      notifications.success($t('events.speakerAssigned', { n: String(result.changedCount) }));
      await historyStore.refresh();
      await loadSegments();
    } catch (e) {
      notifications.error($t('batchAssignSpeaker.failed'), { cause: e });
    } finally {
      isProcessing.set(false);
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set($t('ready'));
      endOperation('batch-assign-speaker');
    }
  }

  async function handleBatchNormalize() {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
    const ids = await resolveViewIds('real');
    if (ids === null) return;
    if (ids.length === 0) {
      notifications.info($t('batchNormalize.nothingToNormalize'));
      return;
    }
    startOperation('batch-normalize');
    isProcessing.set(true);
    statusMessage.set($t('batchNormalize.progress', { n: String(ids.length) }));
    try {
      await api.batchNormalize(ids);
    } catch (e) {
      notifications.error($t('batchNormalize.failed'), { cause: e });
      isProcessing.set(false);
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set($t('ready'));
      endOperation('batch-normalize');
    }
  }

  async function handleRediarize(mode: 'selected' | 'filtered') {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
    const ids =
      mode === 'selected'
        ? $selectedSegmentId
          ? [$selectedSegmentId]
          : []
        : await resolveViewIds();
    if (ids === null) return;
    if (mode === 'selected' && !$selectedSegmentId) {
      notifications.warning($t('rediarize.noSelection'));
      return;
    }
    if (ids.length === 0) {
      notifications.info($t('rediarize.nothingToRediarize'));
      return;
    }
    startOperation('rediarize');
    isProcessing.set(true);
    statusMessage.set($t('rediarize.progress', { n: String(ids.length) }));
    try {
      const updated = await api.rediarizeSegments(ids);
      await loadSegments();
      notifications.success($t('rediarize.success', { n: String(updated) }));
    } catch (e) {
      notifications.error($t('rediarize.failed'), { cause: e });
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('rediarize');
    }
  }

  async function handleDeleteFilteredWithConfirm() {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
    const ids = await resolveViewIds();
    if (ids === null) return;
    if (ids.length === 0) {
      notifications.info($t('batchDelete.nothingToDelete'));
      return;
    }
    showConfirmDialog.set({
      title: $t('batchDelete.confirmTitle'),
      message: $t('batchDelete.confirmMessage', { n: String(ids.length) }),
      onConfirm: () => handleDeleteFiltered(ids),
    });
  }

  async function handleDeleteFiltered(ids: string[]) {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
    if (!(await flushAutosaveForIds(autosave, ids))) return;
    startOperation('batch-delete');
    isProcessing.set(true);
    statusMessage.set($t('batchDelete.progress', { n: String(ids.length) }));
    try {
      await api.deleteSegmentsBatch(ids);
      if ($selectedSegmentId && ids.includes($selectedSegmentId)) {
        selectedSegmentId.set(null);
        wordTimestamps.set([]);
      }
      await loadSegments();
      notifications.success($t('batchDelete.success', { n: String(ids.length) }));
    } catch (e) {
      notifications.error($t('batchDelete.failed'), { cause: e });
    } finally {
      isProcessing.set(false);
      statusMessage.set($t('ready'));
      endOperation('batch-delete');
    }
  }

  async function handleDelete() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;

    // Metadata is committed first; if it conflicts, deletion aborts and the retry remains recoverable.
    if (!(await flushAutosaveForIds(autosave, [seg.id]))) return;

    // Optimistic Update
    const originalSegments = $segments;
    const segmentName = truncateFilename(segmentSourceFilename(seg.audioPath));
    segments.update((list) => list.filter((s) => s.id !== seg.id));
    selectedSegmentId.set(null);
    wordTimestamps.set([]);
    if (historyPanel) {
      historyPanel.recordAction(`Deleted segment: ${segmentName}`, 'delete');
    }

    try {
      await api.deleteSegment(seg.id);
      await historyStore.refresh();
      notifications.info($t('notifications.segmentDeleted'));
    } catch (e) {
      // Revert on error
      segments.set(originalSegments);
      selectedSegmentId.set(seg.id);
      notifications.error($t('notifications.deleteFailed'), { cause: e });
    }
  }

  async function handleAlign() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;
    const text = seg.annotatedTranscript ?? seg.rawTranscript; // VERBATIM LAW: human else champion-raw
    if (!text) return;
    startOperation('align');
    isProcessing.set(true);
    pipelinePhase.set('detecting');
    statusMessage.set($t('pipeline.detecting'));
    try {
      // Pass the segment id: align_segment persists the merged word timings AND stamps the honest
      // alignment_quality (ctc_forced vs energy_heuristic) in one backend transaction. Without the id
      // (the old call) the backend skipped persistence entirely — the client-side updateSegment saved
      // the timings but the quality stayed stamped "energy_heuristic" even for a REAL CTC alignment,
      // which wrongly kept the segment penalized as review-risk in the training grade.
      const ts = await api.alignSegment(seg.audioPath, text, seg.alignmentJson, seg.id);
      // Update UI only AFTER the backend persist succeeds; reload the store from the DB so the fresh
      // alignmentJson AND alignmentQuality both arrive (same race-safe pattern as ReviewMode).
      wordTimestamps.set(ts);
      await loadSegments();
      notifications.success($t('notifications.alignmentComplete'));
    } catch (e) {
      notifications.error($t('notifications.alignmentFailed'), { cause: e });
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('align');
    }
  }

  // Request-sequence guard for waveform loads. Selecting clip A then B raced: A's slower response
  // resolved last and overwrote B's waveform (or B's error state) — the reviewer then annotated text
  // against the WRONG clip's visual evidence. Only the newest request may write (external audit
  // 2026-08-17). A counter, not an AbortController: getWaveform is a Tauri IPC call with no abort
  // signal, so the response cannot be cancelled — only ignored.
  let waveformRequest = 0;

  async function loadWaveform(path: string, alignmentJson?: string | null) {
    const seq = ++waveformRequest;
    if (!tauriAvailable) {
      // Browser preview, not a failure — no waveform backend exists to fail.
      waveformData = [];
      waveformError = null;
      return;
    }
    try {
      const data = await api.getWaveform(path, 200, alignmentJson);
      if (seq !== waveformRequest) return; // a newer selection superseded this one
      waveformData = data;
      waveformError = null;
    } catch (e) {
      if (seq !== waveformRequest) return; // stale failure must not clobber the current clip
      // Sibling of the ReviewMode fix (audit 2026-08-05 #5, commit b554515). An empty array renders
      // identically to genuinely quiet audio, so a FAILED decode read as "this clip is silent" with
      // nothing said. Found by grepping for the class after fixing the review-mode instance — fixing
      // one caller and leaving the other is how a class-of-bug survives its own fix.
      waveformData = [];
      waveformError = formatPublicErrorReference(e) ?? $t('errors.unknown');
      notifications.error($t('review.waveformFailed'), { cause: e });
    }
  }

  function selectSegment(seg: SpeechSegment) {
    // Persist any edit queued for the segment we're LEAVING before switching, so its debounced save
    // is never dropped by the switch (round-16 data-loss fix). The per-selection VIEW setup (playhead
    // reset, word timestamps, waveform) is centralized in the $selectedSegmentId effect above so store-only
    // selections (ValidationPanel "Go to segment", the active-learning / signal-anomaly jumps) get it too.
    autosave.flush();
    selectedSegmentId.set(seg.id);
  }

  function onSeek(time: number) {
    clearWordOverride(); // a waveform scrub leaves word-playback mode; play on to the chunk end
    // The waveform emits clip-relative time (its bars are the chunk window); map back to file time.
    currentTime = chunkEndTime > chunkStartTime ? chunkStartTime + time : time;
  }

  function fmtDuration(ms: number) {
    const m = Math.floor(ms / 60000);
    const s = Math.floor((ms % 60000) / 1000);
    return `${m}:${s.toString().padStart(2, '0')}`;
  }
</script>

<div
  class="h-screen flex flex-col bg-app text-default"
  data-testid="app-root"
  inert={$showReviewInbox}
  aria-hidden={$showReviewInbox ? 'true' : undefined}
>
  <WorkstationRecoveryNotices
    {quarantineNotice}
    {interruptedImport}
    onAcknowledgeQuarantine={() => void acknowledgeQuarantine()}
    onDismissQuarantine={() => (quarantineNotice = null)}
    onResumeImport={() => void resumeImport()}
    onDismissImport={() => void dismissInterruptedImport()}
  />
  <WorkstationHeader
    {tauriAvailable}
    bind:sidebarOpen
    bind:statsOpen
    {showHotkeyOverlay}
    {trainingExportBlocked}
    {trainingExportTitle}
    {modKey}
    onSelectWorkspace={selectWorkspace}
    onOpenCommandPalette={() => (showCommandPalette = true)}
    onOpenFile={() => void handleOpenFile()}
    onImport={() => void handleImport()}
    onExport={() => void handleExport()}
    onExportTranscript={() => void handleExportTranscript()}
    onExportHuggingface={() => void handleExportHuggingface()}
    onExportAudio={() => void handleExportAudio()}
    onOpenWsl={openWslConsole}
    onEnterReview={enterReviewMode}
    onValidate={openValidationPanel}
    onOpenInbox={openReviewInbox}
    onOpenSettings={() => openSettings()}
  />

  {#if $activeOperations.size > 0}
    {@const pct =
      $batchProgress.total > 0
        ? $batchProgress.percent
        : $pipelineTotal > 0
          ? Math.round(($filesProcessed / $pipelineTotal) * 100)
          : -1}
    <div
      class="h-0.5 shrink-0 overflow-hidden bg-accent-soft"
      role="progressbar"
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={pct >= 0 ? pct : undefined}
    >
      {#if pct >= 0}
        <div
          class="h-full rounded-full bg-accent transition-[width] duration-300 ease-smooth"
          style="width: {Math.min(100, Math.max(2, pct))}%"
        ></div>
      {:else}
        <!-- Unknown total: a true sliding indeterminate bar, not a frozen 30% -->
        <div class="h-full w-2/5 rounded-full bg-accent animate-progress-indeterminate"></div>
      {/if}
    </div>
  {/if}

  <!-- Prominent processing indicator: real % bar + elapsed + ETA + stages, under the toolbar. -->
  <ProcessingProgress />

  <div class="flex flex-1 overflow-hidden">
    <ActivityRail view={viewMode} onSelect={selectWorkspace} />
    <!-- Left Panel: Segment List -->
    <ErrorBoundary>
      <aside
        data-testid="left-panel"
        class="shrink-0 flex flex-col border-r border-cortex-800/30 bg-cortex-900/40 backdrop-blur-md transition-all duration-200 overflow-hidden"
        class:panel-collapsed={!sidebarOpen}
        style="width: {sidebarWidth}px;"
      >
        {#if sidebarOpen}
          <div class="p-2 space-y-2 relative">
            <SearchBar />
            {#if showHotkeyOverlay}
              <span
                class="absolute top-4 right-4 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
                >^F</span
              >
            {/if}
            <div class="flex flex-wrap gap-1">
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={() => handleBatchTranscribe('empty')}
                disabled={!tauriAvailable ||
                  $isProcessing ||
                  $segments.every((s) => s.rawTranscript?.trim())}
                title={tauriAvailable ? $t('batchTranscribe.empty') : $t('desktopRuntimeRequired')}
                >{$t('batchTranscribe.empty')}</button
              >
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={() => handleBatchTranscribe('selected')}
                disabled={!tauriAvailable || $isProcessing || !$selectedSegmentId}
                title={tauriAvailable
                  ? $t('batchTranscribe.selected')
                  : $t('desktopRuntimeRequired')}>{$t('batchTranscribe.selected')}</button
              >
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={() => handleBatchTranscribe('filtered')}
                disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
                title={tauriAvailable
                  ? $t('batchTranscribe.filtered')
                  : $t('desktopRuntimeRequired')}>{$t('batchTranscribe.filtered')}</button
              >
            </div>
            <div class="flex flex-wrap gap-1 items-center">
              <input
                class="input !text-[10px] flex-1 !py-1 !px-2 font-mono"
                placeholder={$t('batchAssignSpeaker.placeholder')}
                bind:value={batchSpeakerId}
                aria-label={$t('batchAssignSpeaker.placeholder')}
              />
              <button
                class="btn btn-secondary btn-sm !text-[10px] shrink-0"
                onclick={handleBatchAssignSpeaker}
                disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
                title={tauriAvailable
                  ? $t('batchAssignSpeaker.label')
                  : $t('desktopRuntimeRequired')}>{$t('batchAssignSpeaker.label')}</button
              >
            </div>
            <div class="flex flex-wrap gap-1">
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={handleBatchNormalize}
                disabled={!tauriAvailable ||
                  $isProcessing ||
                  !$filteredSegments.some((s) => s.rawTranscript?.trim())}
                title={tauriAvailable ? $t('batchNormalize.label') : $t('desktopRuntimeRequired')}
                >{$t('batchNormalize.label')}</button
              >
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={() => handleRediarize('filtered')}
                disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
                title={tauriAvailable ? $t('rediarize.filtered') : $t('desktopRuntimeRequired')}
                >{$t('rediarize.filtered')}</button
              >
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={() => handleRediarize('selected')}
                disabled={!tauriAvailable || $isProcessing || !$selectedSegmentId}
                title={tauriAvailable ? $t('rediarize.selected') : $t('desktopRuntimeRequired')}
                >{$t('rediarize.selected')}</button
              >
            </div>

            <!-- Data & AI Tools -->
            <div class="flex flex-wrap gap-1 border-t border-cortex-800/30 pt-2">
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={openSpeakerPanel}
                disabled={!tauriAvailable || $isProcessing}
                title={tauriAvailable ? $t('speaker.title') : $t('desktopRuntimeRequired')}
                >{$t('speakers')}</button
              >
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={openDatasetMerge}
                disabled={!tauriAvailable || $isProcessing}
                title={tauriAvailable ? $t('merge.title') : $t('desktopRuntimeRequired')}
                >{$t('merge')}</button
              >
              <button
                class="btn btn-danger btn-sm !text-[10px] flex-1"
                onclick={handleDeleteFilteredWithConfirm}
                disabled={!tauriAvailable || $isProcessing || $filteredSegments.length === 0}
                title={tauriAvailable ? $t('batchDelete.filtered') : $t('desktopRuntimeRequired')}
                >{$t('batchDelete.filtered')}</button
              >
            </div>
          </div>
          <div class="flex-1 overflow-hidden p-2 pt-0">
            <VirtualList
              items={$filteredSegments}
              itemHeight={56}
              selectedId={$selectedSegmentId}
              onSelect={selectSegment}
              hasMore={$libraryTruncated}
              onEndReached={() => void segments.loadMore()}
            >
              {#snippet children(item: SpeechSegment)}
                {@const sourceName = truncateFilename(segmentSourceFilename(item.audioPath))}
                {@const chunkBadge = segmentChunkLabel(item.alignmentJson)}
                <button
                  data-testid="segment-card"
                  data-id={item.id}
                  class="w-full text-start p-2.5 rounded-xl transition-all duration-300 h-full flex items-start group
                {item.id === $selectedSegmentId
                    ? 'bg-gradient-to-br from-cortex-800/80 to-cortex-900/80 ring-1 ring-cortex-400 shadow-[0_0_15px_rgba(56,189,248,0.15)] scale-[1.02] transform'
                    : 'hover:bg-cortex-800/40 hover:scale-[1.01] transform'}"
                  onclick={() => selectSegment(item)}
                >
                  <div class="flex-1 min-w-0">
                    <div class="flex items-center gap-2 min-w-0">
                      <span
                        class="text-xs font-semibold text-cortex-200 truncate flex-1 min-w-0 transition-colors
                    {item.id === $selectedSegmentId
                          ? 'text-cortex-100'
                          : 'group-hover:text-cortex-300'}"
                        title={item.audioPath}
                      >
                        {sourceName}
                      </span>
                      {#if chunkBadge}
                        <span
                          class="text-[9px] text-cortex-400 bg-cortex-900/80 border border-cortex-800/50 px-1.5 py-0.5 rounded shadow-sm shrink-0 font-mono"
                          title="{$t('chunk')} {chunkBadge}"
                        >
                          {chunkBadge}
                        </span>
                      {/if}
                      {#if item.verified}
                        <CircleCheckBig
                          class="h-3 w-3 shrink-0 text-emerald-400 drop-shadow-[0_0_5px_rgba(52,211,153,0.5)]"
                          role="img"
                          aria-label={$t('verified')}
                        />
                      {/if}
                    </div>
                    <div class="flex items-center gap-2 mt-1">
                      <span
                        class="text-[10px] text-cortex-400 font-medium bg-cortex-950/50 px-1.5 rounded-sm shrink-0"
                        >{fmtDuration(item.durationMs)}</span
                      >
                      {#if item.confidence !== undefined && item.confidence !== null}
                        <span
                          class="text-[10px] font-mono font-medium px-1.5 rounded-sm border shrink-0
                      {item.confidence < 0.5
                            ? 'text-red-400 bg-red-950/30 border-red-900/30'
                            : item.confidence < 0.85
                              ? 'text-amber-400 bg-amber-950/30 border-amber-900/30'
                              : 'text-emerald-400 bg-emerald-950/30 border-emerald-900/30'}"
                          title={$t('validation.activeLearning.confidence')}
                        >
                          {Math.round(item.confidence * 100)}%
                        </span>
                      {/if}
                      {#if item.speakerId}
                        <span
                          class="text-[10px] text-indigo-300 font-medium bg-indigo-950/30 border border-indigo-900/50 px-1.5 rounded-sm truncate max-w-24 shrink-0"
                        >
                          {item.speakerId}
                        </span>
                      {/if}
                      <span
                        class="text-[11px] text-cortex-500 truncate mt-0.5"
                        dir="rtl"
                        lang="ckb"
                      >
                        {item.annotatedTranscript ?? item.rawTranscript ?? '...'}
                      </span>
                    </div>
                  </div>
                </button>
              {/snippet}
            </VirtualList>

            {#if segmentsLoading}
              <div class="space-y-2 p-2">
                {#each [1, 2, 3, 4, 5] as _}
                  <div
                    class="p-2 rounded-xl bg-cortex-950/20 border border-cortex-900/10 space-y-1.5 animate-pulse"
                  >
                    <div class="flex items-center justify-between">
                      <div class="h-3 bg-cortex-800/30 rounded-md w-2/3"></div>
                      <div class="h-3 bg-cortex-800/30 rounded-md w-8"></div>
                    </div>
                    <div class="flex gap-2">
                      <div class="h-2.5 bg-cortex-800/15 rounded-md w-10"></div>
                      <div class="h-2.5 bg-cortex-800/15 rounded-md w-1/2"></div>
                    </div>
                  </div>
                {/each}
              </div>
            {:else if $filteredSegments.length === 0}
              <!-- See EmptyState.svelte: h-full + plain centering made this box's top unreachable at
                   200 % zoom. Same fix, same reason. -->
              <div
                data-testid="segments-empty-state"
                class="flex min-h-full flex-col items-center [justify-content:safe_center] gap-3 px-6 text-center animate-fade-in"
              >
                {#if $libraryLoadError}
                  <!-- P2.1: a DB/IPC read failure is NOT an empty library — show it distinctly, with the
                       real error and a Retry, so the user never mistakes a load error for wiped data. -->
                  <div
                    data-testid="segments-load-error"
                    class="flex h-14 w-14 items-center justify-center rounded-2xl bg-surface-2 text-danger"
                  >
                    <TriangleAlert size={26} strokeWidth={1.5} aria-hidden="true" />
                  </div>
                  <div class="max-w-[16rem]">
                    <p class="text-sm font-semibold text-default">
                      {$t('notifications.loadSegmentsFailed')}
                    </p>
                    <p class="mt-1 break-words text-xs leading-relaxed text-muted">
                      {$libraryLoadError}
                    </p>
                  </div>
                  <div class="mt-1">
                    <button class="btn btn-primary !text-xs" onclick={loadSegments}
                      >{$t('retry')}</button
                    >
                  </div>
                {:else if $searchQuery}
                  <div
                    class="flex h-12 w-12 items-center justify-center rounded-full bg-surface-2 text-subtle"
                  >
                    <Search size={22} strokeWidth={1.5} aria-hidden="true" />
                  </div>
                  <div>
                    <p class="text-sm font-medium text-default">{$t('noResultsFound')}</p>
                    <p class="mt-1 max-w-[14rem] truncate text-xs text-subtle">“{$searchQuery}”</p>
                  </div>
                {:else}
                  <div
                    class="flex h-14 w-14 items-center justify-center rounded-2xl bg-accent-soft text-accent"
                  >
                    <Mic size={26} strokeWidth={1.5} aria-hidden="true" />
                  </div>
                  <div class="max-w-[15rem]">
                    <p class="text-sm font-semibold text-default">{$t('noSegmentsLoaded')}</p>
                    <p class="mt-1 text-xs leading-relaxed text-muted">{$t('emptyStateHint')}</p>
                  </div>
                  {#if tauriAvailable}
                    <div class="mt-1 flex gap-2">
                      <button class="btn btn-primary !text-xs" onclick={handleImport}
                        >{$t('import')}</button
                      >
                      <button class="btn btn-secondary !text-xs" onclick={handleOpenFile}
                        >{$t('open')}</button
                      >
                    </div>
                  {/if}
                {/if}
              </div>
            {/if}
          </div>
        {/if}
      </aside>
    </ErrorBoundary>
    <PanelSplitter
      direction="horizontal"
      label={$t('resizeSegmentsPanel')}
      value={sidebarWidth}
      onResize={(delta) => (sidebarWidth = Math.max(200, Math.min(600, sidebarWidth + delta)))}
    />

    <!-- Center: Transcription Work Area -->
    <ErrorBoundary>
      <main
        data-testid="center-panel"
        class="flex-1 flex flex-col gap-3 p-4 overflow-y-auto min-w-0"
      >
        {#if viewMode === 'curate' && $segmentStats.pending > 0 && !reviewNudgeDismissed}
          <!-- Friendly nudge: surface the fast Review & Correct flow so it's never hidden. -->
          <div
            data-testid="review-nudge"
            class="shrink-0 flex items-center justify-between gap-3 rounded-lg border border-amber-400/40 bg-amber-400/10 px-4 py-3"
          >
            <div class="flex items-center gap-2.5 text-sm text-amber-100">
              <SquarePen class="h-5 w-5 shrink-0" aria-hidden="true" />
              <span
                >{$t($segmentStats.pending === 1 ? 'reviewCorrect.ctaOne' : 'reviewCorrect.cta', {
                  n: String($segmentStats.pending),
                })}</span
              >
            </div>
            <div class="flex items-center gap-2 shrink-0">
              <button
                data-testid="review-nudge-start"
                class="btn btn-primary !text-xs"
                onclick={enterReviewMode}
              >
                {$t('reviewCorrect.start')}
              </button>
              <button
                class="text-cortex-400 hover:text-cortex-200 text-sm leading-none px-1"
                aria-label={$t('reviewCorrect.dismiss')}
                title={$t('reviewCorrect.dismiss')}
                onclick={() => (reviewNudgeDismissed = true)}
              >
                <X class="h-4 w-4" aria-hidden="true" />
              </button>
            </div>
          </div>
        {/if}
        {#if viewMode === 'insights'}
          <!-- P2.3: the readiness card's "N clips still awaiting review" blocker becomes a button that
               actually goes there, instead of naming a problem and leaving the reviewer to find it. -->
          <LazyComponent
            load={loadStatsDashboard}
            componentProps={{ onOpenReview: enterReviewMode }}
            {...lazyLabels}
          />
          <LazyComponent load={loadRefineryPanel} {...lazyLabels} />
        {:else if viewMode === 'review'}
          <LazyComponent
            load={loadReviewMode}
            componentProps={{ onExport: handleExport, onDone: () => void leaveReviewMode() }}
            {...lazyLabels}
            onClose={() => leaveReviewMode()}
          />
        {:else if $selectedSegment}
          <div class="card overflow-hidden">
            {#if waveformError}
              <!-- Say which it is: unreadable audio, not a quiet clip. Same reasoning as ReviewMode. -->
              <div
                class="flex items-center justify-between gap-3 p-3 text-xs text-amber-300"
                data-testid="curate-waveform-error"
                role="status"
              >
                <span class="min-w-0 truncate">{$t('review.waveformFailed')}</span>
                <button
                  type="button"
                  class="btn btn-secondary shrink-0 !text-xs"
                  onclick={() =>
                    $selectedSegment &&
                    loadWaveform($selectedSegment.audioPath, $selectedSegment.alignmentJson)}
                >
                  {$t('retry')}
                </button>
              </div>
            {:else}
              <Waveform
                waveform={waveformData}
                currentTime={chunkClipPosition}
                duration={chunkClipLength}
                wordTimestamps={$wordTimestamps}
                {onSeek}
              />
            {/if}
          </div>

          <ErrorBoundary>
            <!-- endTime honours the transient tap-a-word override so a tapped word stops at ITS end. -->
            <AudioPlayer
              audioPath={$selectedSegment.audioPath}
              startTime={wordStartOverride ?? chunkStartTime}
              endTime={wordEndOverride ?? chunkEndTime}
              displayStart={chunkStartTime}
              displayEnd={chunkEndTime}
              bind:currentTime
              bind:duration={playerDuration}
              bind:playing={isAudioPlaying}
              autoplay={$settings.autoplaySegments}
            />
          </ErrorBoundary>

          <div class="card p-4 space-y-3">
            <div class="flex items-center justify-between">
              <h2 class="text-sm font-semibold text-cortex-200 uppercase tracking-wider">
                {$t('transcript')}
                {#if chunkLabel}
                  <span
                    class="ms-2 text-[10px] font-normal normal-case text-cortex-500 bg-cortex-900 px-1.5 py-0.5 rounded"
                  >
                    {$t('chunk')}
                    {chunkLabel}
                  </span>
                {/if}
              </h2>
              <div class="flex gap-2">
                <button
                  data-testid="transcribe-btn"
                  class="btn btn-secondary !text-xs relative"
                  onclick={handleTranscribe}
                  disabled={$isProcessing}
                >
                  {#if $isProcessing}
                    <span class="flex items-center gap-1">
                      <LoaderCircle class="h-3 w-3 animate-spin" aria-hidden="true" />
                      {$t('transcribing')}
                    </span>
                  {:else}
                    {$t('transcribe')}
                  {/if}
                  {#if showHotkeyOverlay}
                    <span
                      class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
                      >^T</span
                    >
                  {/if}
                </button>
              </div>
            </div>

            <div class="grid grid-cols-2 gap-3">
              <div class="space-y-1">
                <label for="raw-ts" class="text-[11px] text-cortex-400">{$t('rawAsr')}</label>
                <textarea
                  id="raw-ts"
                  dir="rtl"
                  lang="ckb"
                  class="input h-28 resize-none font-mono text-xs text-end"
                  value={$selectedSegment.rawTranscript}
                  readonly
                ></textarea>
              </div>
              <div class="space-y-1">
                <label for="norm-ts" class="text-[11px] text-cortex-400">{$t('normalized')}</label>
                <textarea
                  id="norm-ts"
                  dir="rtl"
                  lang="ckb"
                  class="input h-28 resize-none font-mono text-xs text-end"
                  value={$selectedSegment.normalizedTranscript ?? ''}
                  readonly
                ></textarea>
              </div>
            </div>
          </div>

          <div class="card p-4 space-y-3">
            <div class="flex items-center justify-between">
              <div class="flex items-center gap-2">
                <h2 class="text-sm font-semibold text-cortex-200 uppercase tracking-wider">
                  {$t('annotation')}
                </h2>
                {#if $selectedSegment.verified}
                  <span class="badge-verified">{$t('verified')}</span>
                {:else}
                  <span class="badge-pending">{$t('pending')}</span>
                {/if}
              </div>
              <div class="flex bg-cortex-950 p-0.5 rounded-lg border border-cortex-800/40">
                <button
                  class="px-2.5 py-1 text-[10px] uppercase font-bold tracking-wider rounded-md transition-colors
                  {editorTab === 'interactive'
                    ? 'bg-cortex-700 text-default shadow-sm'
                    : 'text-cortex-400 hover:text-cortex-200'}"
                  onclick={() => (editorTab = 'interactive')}
                >
                  {$t('editorInteractive')}
                </button>
                <button
                  class="px-2.5 py-1 text-[10px] uppercase font-bold tracking-wider rounded-md transition-colors
                  {editorTab === 'raw'
                    ? 'bg-cortex-700 text-default shadow-sm'
                    : 'text-cortex-400 hover:text-cortex-200'}"
                  onclick={() => (editorTab = 'raw')}
                >
                  {$t('annotation')}
                </button>
              </div>
            </div>

            {#if editorTab === 'interactive'}
              <div
                class="p-5 rounded-2xl bg-gradient-to-b from-cortex-900/50 to-cortex-950/80 border border-white/5 shadow-inner font-mono text-[15px] leading-loose min-h-32 select-text transition-all duration-300 hover:border-cortex-500/30 hover:shadow-[inset_0_0_20px_rgba(56,189,248,0.05)]"
              >
                {#if $wordTimestamps.length > 0}
                  <!-- Kurdish is RTL: this flex row of word-chips must be dir=rtl so the chips
                       lay out right-to-left; otherwise the first spoken word sits leftmost and the
                       words read reversed. -->
                  <div class="flex flex-wrap gap-x-1.5 gap-y-2" dir="rtl" lang="ckb">
                    {#each $wordTimestamps as w}
                      <!-- Word times are CLIP-relative; compare/seek against the clip offset so an
                           offset chunk highlights + seeks correctly (not at the whole-file position). -->
                      {@const isActive =
                        currentTime - chunkStartTime >= w.start &&
                        currentTime - chunkStartTime <= w.end}
                      <!-- Library chips are playback-only. Human corrections are committed through
                           Review Mode's atomic decision path. -->
                      <span
                        class="relative inline-block px-1.5 py-0.5 rounded cursor-pointer transition-all duration-150 group
                        {isActive
                          ? 'bg-cortex-700 text-default font-bold border-b border-yellow-400'
                          : 'text-cortex-200 hover:bg-cortex-800 hover:text-white'}"
                        onclick={() => playWordClip(w)}
                        title="{w.word} ({w.start.toFixed(2)}s - {w.end.toFixed(2)}s)"
                        role="button"
                        tabindex="0"
                        aria-keyshortcuts="Enter Space"
                        onkeydown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            playWordClip(w);
                            e.preventDefault();
                          }
                        }}
                      >
                        <span class="select-text">{w.word}</span>
                        <span
                          class="absolute -top-6 left-1/2 -translate-x-1/2 px-1.5 py-0.5 text-[8px] bg-cortex-950 text-cortex-400 rounded opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none whitespace-nowrap z-10 border border-cortex-850 shadow-md"
                        >
                          {w.start.toFixed(2)}s
                        </span>
                      </span>
                    {/each}
                  </div>
                {:else}
                  <p class="text-cortex-500 italic">
                    {$t('editor.noWordTimestamps')}
                  </p>
                {/if}
              </div>
            {:else}
              <textarea
                dir="rtl"
                lang="ckb"
                class="input h-32 resize-none font-mono text-sm text-end"
                value={$selectedSegment.annotatedTranscript ?? ''}
                readonly
              ></textarea>
            {/if}

            <div class="flex items-end gap-2">
              <div class="flex-1 space-y-1">
                <label for="speaker-id" class="text-[11px] text-cortex-400">{$t('speaker')}</label>
                <input
                  id="speaker-id"
                  class="input !text-xs font-mono"
                  value={$selectedSegment.speakerId ?? ''}
                  placeholder={$t('batchAssignSpeaker.placeholder')}
                  disabled={$isProcessing || !selectedMetadataReady}
                  aria-describedby={!selectedMetadataReady ? 'speaker-metadata-loading' : undefined}
                  oninput={(e) => {
                    const seg = $selectedSegment;
                    if (seg) {
                      const speakerId = (e.target as HTMLInputElement).value;
                      segments.update((arr) =>
                        arr.map((s) => (s.id === seg.id ? { ...s, speakerId } : s)),
                      );
                      // Speaker attribution is non-review metadata and may autosave independently.
                      scheduleAutoSave({ speakerId });
                    }
                  }}
                />
              </div>
              <button
                class="btn btn-secondary !text-xs shrink-0"
                onclick={handleSaveSpeaker}
                disabled={$isProcessing || !selectedMetadataReady}>{$t('speaker.save')}</button
              >
              {#if !selectedMetadataReady}
                <span id="speaker-metadata-loading" class="sr-only">{$t('loading')}</span>
              {/if}
            </div>

            <DiffView
              raw={$selectedSegment.rawTranscript ?? ''}
              annotated={$selectedSegment.annotatedTranscript ?? ''}
            />

            {#if $wordTimestamps.length > 0}
              <div class="space-y-1">
                <span class="text-[11px] text-cortex-400">{$t('wordTimestamps')}</span>
                <div
                  class="flex flex-wrap gap-1 max-h-20 overflow-y-auto"
                  role="group"
                  aria-label={$t('wordTimestamps')}
                  dir="rtl"
                  lang="ckb"
                >
                  {#each $wordTimestamps as w}
                    <button
                      type="button"
                      class="px-1.5 py-0.5 text-[10px] rounded bg-cortex-800 text-cortex-300 font-mono cursor-pointer hover:bg-cortex-700 transition-colors border-0"
                      title="{w.word}: {w.start.toFixed(2)}s - {w.end.toFixed(2)}s"
                      onclick={() => playWordClip(w)}
                      onkeydown={(e) => {
                        if (e.key === 'Enter' || e.key === ' ') {
                          playWordClip(w);
                          e.preventDefault();
                        }
                      }}
                      aria-label={$t('review.playWordAria').replace('{word}', w.word)}
                      >{w.word}</button
                    >
                  {/each}
                </div>
              </div>
            {/if}

            <div class="flex gap-2 pt-1">
              <button
                class="btn btn-secondary !text-xs"
                onclick={handleAlign}
                disabled={$isProcessing}
              >
                {$t('align')}
              </button>
              <button
                class="btn btn-danger !text-xs ms-auto relative"
                onclick={handleDeleteWithConfirm}
              >
                {$t('delete')}
                {#if showHotkeyOverlay}
                  <span
                    class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
                    >{$t('app.deleteHint')}</span
                  >
                {/if}
              </button>
            </div>
          </div>
        {:else}
          <EmptyState variant="mic" title={$t('selectSegment')}>
            {#if $segmentStats.pending > 0}
              <!-- Center empty but review work remains: surface the summary + the next recommended
                   action instead of a mostly blank canvas (audit P2). -->
              <div class="flex flex-col items-center gap-2 mb-4">
                <p class="text-sm text-default">
                  {$segmentStats.pending === 1
                    ? $t('reviewCorrect.ctaOne')
                    : $t('reviewCorrect.cta', { n: String($segmentStats.pending) })}
                </p>
                <button
                  class="btn btn-primary"
                  onclick={openReviewInbox}
                  data-testid="empty-start-review">{$t('reviewCorrect.start')}</button
                >
              </div>
            {/if}
            <div class="flex flex-wrap justify-center gap-x-3 gap-y-1 text-xs text-subtle">
              <span><kbd>{modKey}+O</kbd> {$t('openFile')}</span>
              <span><kbd>{modKey}+I</kbd> {$t('import')}</span>
              <span><kbd>{modKey}+T</kbd> {$t('transcribe')}</span>
              <span><kbd>{modKey}+K</kbd> {$t('shortcuts')}</span>
            </div>
          </EmptyState>
        {/if}
      </main>
    </ErrorBoundary>

    <!-- Right Panel: Stats -->
    {#if $filteredSegments.length > 0 && viewMode !== 'insights'}
      <PanelSplitter
        direction="horizontal"
        label={$t('resizeStatsPanel')}
        value={statsWidth}
        onResize={(delta) => (statsWidth = Math.max(200, Math.min(600, statsWidth - delta)))}
      />
      <ErrorBoundary>
        <aside
          data-testid="right-panel"
          class="shrink-0 flex flex-col border-l border-cortex-800/30 bg-cortex-900/40 backdrop-blur-md transition-all duration-200 overflow-hidden"
          class:panel-collapsed={!statsOpen}
          style="width: {statsWidth}px;"
        >
          {#if statsOpen}
            <!-- A scrollable region must be keyboard-focusable (WCAG 2.1.1 / axe scrollable-region-focusable); role=region + aria-label give it a name. -->
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <div
              class="p-2 flex flex-col gap-3 h-full overflow-y-auto"
              role="region"
              aria-label={$t('stats.title')}
              tabindex="0"
              style="scrollbar-width: thin; scrollbar-color: #475569 transparent;"
            >
              <AgentReportPanel report={latestAgentReport} stageEvents={latestAgentStageEvents} />
              <LazyComponent load={loadStatsDashboard} {...lazyLabels} />
              <HistoryPanel bind:this={historyPanel} {showHotkeyOverlay} />
            </div>
          {/if}
        </aside>
      </ErrorBoundary>
    {/if}
  </div>

  <StatusBar />
</div>

<WorkstationOverlays
  bind:showCommandPalette
  reviewActive={viewMode === 'review' || $showReviewInbox}
  {...lazyLabels}
  {loadSegments}
  {loadSettingsPanel}
  {loadKeyboardShortcuts}
  {loadCommandPalette}
  {loadValidationPanel}
  {loadReviewInbox}
  {loadSpeakerPanel}
  {loadDatasetMerge}
  {loadWslConsolePanel}
/>
