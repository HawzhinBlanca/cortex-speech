<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { get } from 'svelte/store';
  import * as api from './lib/commands';
  import { createAutosaveController } from './lib/autosave';
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
  } from './lib/stores/segmentStore';
  import { settings, showSettings, openSettings } from './lib/stores/settingsStore';
  import {
    showKeyboardHelp,
    showConfirmDialog,
    isProcessing,
    statusMessage,
  } from './lib/stores/uiStore';
  import { notifications } from './lib/stores/notificationStore';
  import { isVerifiedGood, hasRealTranscript } from './lib/segmentQuality';
  import { historyStore } from './lib/stores/historyStore';
  import { initKeyboardManager, globalKeyboardManager, modKeyLabel } from './lib/keyboard';
  import { focusTrap } from './lib/actions/focusTrap';
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
  import { cancelOperation } from './lib/commands';
  import { isTauriRuntime } from './lib/runtime';
  import AudioPlayer from './lib/AudioPlayer.svelte';
  import EngineStatusPill from './lib/EngineStatusPill.svelte';
  import JobsActivityPill from './lib/JobsActivityPill.svelte';
  import Waveform from './lib/Waveform.svelte';
  import ErrorBoundary from './lib/ErrorBoundary.svelte';
  import Toast from './lib/Toast.svelte';
  import SettingsPanel from './lib/SettingsPanel.svelte';
  import StatsDashboard from './lib/StatsDashboard.svelte';
  import RefineryPanel from './lib/RefineryPanel.svelte';
  import AgentReportPanel from './lib/AgentReportPanel.svelte';
  import SearchBar from './lib/SearchBar.svelte';
  import VirtualList from './lib/VirtualList.svelte';
  import KeyboardShortcuts from './lib/KeyboardShortcuts.svelte';
  import ConfirmDialog from './lib/ConfirmDialog.svelte';
  import ValidationPanel from './lib/ValidationPanel.svelte';
  import SpeakerPanel from './lib/SpeakerPanel.svelte';
  import DatasetMerge from './lib/DatasetMerge.svelte';
  import WslConsolePanel from './lib/WslConsolePanel.svelte';
  import ReviewInbox from './lib/ReviewInbox.svelte';
  import DiffView from './lib/DiffView.svelte';
  import CommandPalette from './lib/CommandPalette.svelte';
  import EmptyState from './lib/EmptyState.svelte';
  import ActivityRail from './lib/ActivityRail.svelte';
  import ProcessingProgress from './lib/ProcessingProgress.svelte';
  import ReviewMode from './lib/ReviewMode.svelte';
  import PanelSplitter from './lib/PanelSplitter.svelte';
  import HistoryPanel from './lib/HistoryPanel.svelte';
  import {
    parseSourceMeta,
    parseWordTimestamps,
    mergeWordTimestamps,
    chunkPlaybackRange,
    segmentSourceFilename,
    truncateFilename,
    segmentChunkLabel,
  } from './lib/alignment';
  import { wordPlayBounds, replaceWordToken } from './lib/wordEdit';

  type HistoryPanelApi = {
    recordAction: (description: string, type: 'edit' | 'verify' | 'delete' | 'import') => void;
  };

  let waveformData = $state<number[]>([]);
  let currentTime = $state(0);
  let playerDuration = $state(0);
  let isAudioPlaying = $state(false);
  let segmentsLoading = $state(true);
  let sidebarOpen = $state(true);
  let statsOpen = $state(true);
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
  let verifyInFlight = $state(false);
  let batchSpeakerId = $state('');
  let editorTab = $state<'interactive' | 'raw'>('interactive');
  let editingWordIndex = $state<number | null>(null);
  let historyPanel = $state<HistoryPanelApi | null>(null);
  let latestAgentReport = $state<AgentImportReport | null>(null);
  let latestAgentStageEvents = $state<AgentStageEvent[]>([]);

  let saveState = $state<'idle' | 'saving' | 'saved'>('idle');
  // Debounced auto-save lives in a standalone, unit-tested controller (lib/autosave.ts). It keys the
  // debounce per target segment and FLUSHES a queued edit before re-keying to a different segment, so
  // switching segments mid-debounce can never silently drop the prior segment's edit (the round-16
  // data-loss fix). The controller re-reads the fresh row at fire time and re-applies only the user's
  // edited fields, so a concurrent background reload (WSL/import/batch completion) or a verify/normalize
  // can't clobber an in-progress keystroke, and a row that has since been DELETED is never resurrected
  // (getRow returns null -> no save). `pendingId()` lets delete/re-transcribe paths drop the queued
  // edit for one specific segment without touching an unrelated segment's pending save.
  const autosave = createAutosaveController<SpeechSegment>({
    targetId: () => get(selectedSegment)?.id ?? null,
    getRow: (id) => get(segments).find((s) => s.id === id) ?? null,
    // F10 root fix: persist ONLY the edited fields via the partial-update IPC — the backend reads the
    // FRESH row under its lock and applies just these fields, so a debounced save during a
    // minutes-long batch can never merge a stale store row over concurrently-written columns.
    save: (_row, fields, id) => api.updateSegmentFields(id, fields),
    onState: (s) => {
      saveState = s;
      if (s === 'saved') {
        setTimeout(() => {
          if (saveState === 'saved') saveState = 'idle';
        }, 2000);
      }
    },
    onError: (e) => notifications.error($t('notifications.saveFailed'), { detail: String(e) }),
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
      notifications.error($t('import.resumeFailed'), { detail: String(e) });
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
        if (restored.selected_segment_id && get(segments).some((s) => s.id === restored.selected_segment_id)) {
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

  function agentStageTone(status: string): string {
    if (status === 'completed') return 'border-emerald-700/40 text-emerald-300 bg-emerald-950/30';
    if (status === 'blocked') return 'border-red-700/40 text-red-300 bg-red-950/30';
    return 'border-amber-700/40 text-amber-300 bg-amber-950/30';
  }

  function compactStageLabel(stage: string): string {
    return stage.replaceAll('_', ' ');
  }

  // Thin delegates over the tested autosave controller (lib/autosave.ts). The controller owns the
  // per-segment debounce, the flush-before-rekey (no dropped edit on segment switch), the fresh-row
  // re-read + field overlay (no clobber of concurrent verify/normalize/background-reload changes), and
  // the deleted-row guard (getRow null -> no resurrecting INSERT-ON-CONFLICT). Keeping these wrappers
  // preserves every existing call site's intent without re-deriving that logic inline.

  // Cancel a pending save WITHOUT persisting — used when the pending segment is deleted, so its
  // debounced edit can't fire after the row is gone and resurrect it via INSERT-ON-CONFLICT.
  function cancelPendingSave() {
    autosave.cancel();
    if (saveState === 'saving') saveState = 'idle';
  }

  function scheduleAutoSave(edits: Partial<SpeechSegment> = {}) {
    autosave.schedule(edits);
  }

  // The interactive chip strip, so an explicit editor close (Enter/Escape) restores focus to the
  // chip (WCAG 2.4.3). Not called on blur — a Tab/click away already moved focus correctly.
  let interactiveStripEl = $state<HTMLElement | undefined>();
  function refocusChip(idx: number) {
    void tick().then(() =>
      interactiveStripEl?.querySelector<HTMLElement>(`[data-chip="${idx}"]`)?.focus(),
    );
  }

  function finishEditingWord(index: number, newValue: string) {
    const fix = newValue.trim();
    const oldWord = $wordTimestamps[index]?.word;
    // Unchanged or empty = cancel.
    if (!fix || fix === oldWord) {
      editingWordIndex = null;
      return;
    }
    const updatedWords = [...$wordTimestamps];
    updatedWords[index] = { ...updatedWords[index], word: fix };
    wordTimestamps.set(updatedWords);

    const seg = $selectedSegment;
    if (seg) {
      const alignmentJson = mergeWordTimestamps(seg.alignmentJson, updatedWords);
      // Apply the ONE word change into the EXISTING transcript (preserving punctuation and any
      // text-editor-tab / LLM-refined divergence) via the strict token replace — NOT a rebuild from
      // the bare alignment-word join, which silently drops that divergence. If the token can't be
      // located confidently (diverged / repeated), update only the alignment word and leave the
      // transcript untouched rather than corrupt the gold.
      const currentText = seg.annotatedTranscript ?? '';
      const replaced = replaceWordToken(currentText, index, oldWord ?? '', fix, updatedWords.length);
      const patch =
        replaced !== null ? { alignmentJson, annotatedTranscript: replaced } : { alignmentJson };
      segments.update((arr) => arr.map((s) => (s.id === seg.id ? { ...s, ...patch } : s)));
      scheduleAutoSave(patch);
    }
    editingWordIndex = null;
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
    // Per-selection VIEW setup for EVERY selection path. selectSegment() used to set these inline, but
    // store-only selections from OTHER components — ValidationPanel "Go to segment" (jumpToSegment) and the
    // active-learning / signal-anomaly jumps — only set the selectedSegmentId store and bypassed it. When
    // chunks share ONE source audioPath (single-file import), the AudioPlayer then kept playing from the OLD
    // position straight through the new clip's endTime (the reviewer HEARD the wrong clip while the UI showed
    // the new one), and the waveform bars / tap-a-word data stayed on the previous chunk. Centralizing it
    // here fixes every path. `get(selectedSegment)` reads WITHOUT subscribing, so this fires once per
    // selection IDENTITY, not on unrelated segment-data updates (e.g. a mid-review re-alignment).
    const seg = id ? get(selectedSegment) : null;
    if (seg) {
      currentTime = chunkPlaybackRange(parseSourceMeta(seg.alignmentJson)).startTime;
      wordTimestamps.set(parseWordTimestamps(seg.alignmentJson));
      loadWaveform(seg.audioPath, seg.alignmentJson);
    }
  });
  // Tap a word → play EXACTLY that word; with an index (double-tap / F2), also open its inline editor
  // so the fix is typed right where the ear just confirmed the error.
  function playWordClip(w: WordTimestamp, idx?: number) {
    const b = wordPlayBounds(w, chunkStartTime, chunkEndTime);
    // Idempotent play: a double-click dispatches click,click,dblclick — don't hard-reseek the same
    // word 2–3× (a stutter). Still open the editor (idx) even when the reseek is skipped.
    if (!(isAudioPlaying && wordStartOverride === b.start && wordEndOverride === b.end)) {
      wordStartOverride = b.start;
      wordEndOverride = b.end;
      currentTime = b.start;
      isAudioPlaying = true;
    }
    if (idx !== undefined) editingWordIndex = idx;
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
    const mqStats = window.matchMedia('(min-width: 1200px)');
    const mqSidebar = window.matchMedia('(min-width: 900px)');

    function onStatsChange(e: MediaQueryListEvent | MediaQueryList) {
      statsOpen = e.matches;
    }
    function onSidebarChange(e: MediaQueryListEvent | MediaQueryList) {
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
      if (lastOk != null && Date.now() / 1000 - lastOk > 3 * 600 && ($segmentStats?.total ?? 0) > 0) {
        notifications.error(
          $t('notifications.snapshotStale', {
            minutes: String(Math.round((Date.now() / 1000 - lastOk) / 60)),
          }),
        );
      }
      if (h.free_disk_bytes != null && h.free_disk_bytes < 2 * GiB) {
        notifications.error($t('notifications.lowDisk', { gb: (h.free_disk_bytes / GiB).toFixed(1) }));
      }
      if ((h.missing_models?.length ?? 0) > 0) {
        notifications.error($t('notifications.missingModels', { models: h.missing_models.join(', ') }));
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
        notifications.error($t('notify.refreshFailedImport'), { detail: String(e) });
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
        } else if (payload.operation === 'assign_speaker') {
          await loadSegments();
          statusMessage.set($t('ready'));
          endOperation('batch-assign-speaker');
        } else if (payload.operation === 'normalize') {
          await loadSegments();
          statusMessage.set($t('ready'));
          endOperation('batch-normalize');
        }
      } catch (e) {
        console.error('Batch complete handler error:', e);
        notifications.error($t('notify.refreshFailedBatch'), { detail: String(e) });
      } finally {
        isProcessing.set(false);
      }
    });
    if (isTauriRuntime()) {
      try {
        await startEventListeners();
      } catch (e) {
        notifications.error($t('eventListenersFailed'), { detail: String(e) });
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
            notifications.error($t('notifications.previousCrash', { summary: crash }));
          }
        })
        .catch((e) => console.error('crash check failed', e));
      // Flush the debounced autosave BEFORE the native window closes so the last correction of a
      // session is never lost — onDestroy does not reliably run on a Tauri window close. The flush is
      // bounded by a 3s timeout so a stuck backend can never prevent the app from closing.
      try {
        const { getCurrentWindow } = await import('@tauri-apps/api/window');
        const appWindow = getCurrentWindow();
        // `closing` lets the SECOND close request pass through un-prevented: if destroy() is ever
        // denied again (the original bug — core:default lacks window:allow-destroy, so preventDefault
        // ran and then destroy() silently rejected, leaving a window that could never be closed), the
        // fallback close() re-triggers this handler, which now steps aside instead of re-preventing.
        let closing = false;
        closeUnlisten = await appWindow.onCloseRequested(async (event) => {
          if (closing) return; // pass-through: let the OS complete the close
          event.preventDefault();
          closing = true;
          try {
            await Promise.race([
              autosave.flushAsync(),
              new Promise<void>((resolve) => setTimeout(resolve, 3000)),
            ]);
          } finally {
            try {
              await appWindow.destroy();
            } catch (destroyErr) {
              // Never trap the user in an uncloseable window: fall back to a plain close request,
              // which the `closing` flag above lets through without interception.
              console.error('window.destroy failed; falling back to close():', destroyErr);
              appWindow.close().catch((closeErr) => {
                closing = false; // both paths failed — restore interception and surface it
                console.error('window.close fallback also failed:', closeErr);
                notifications.error('Close failed — use the tray/task manager', {
                  detail: String(closeErr),
                });
              });
            }
          }
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
    // Flush (not just cancel) a pending edit on teardown so a correction typed in the last debounce
    // window before exit is still persisted, rather than silently dropped by a bare clearTimeout.
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
    const shortcuts = [
      {
        key: 'o',
        ctrl: true,
        description: 'Open audio file',
        action: handleOpenFile,
        category: 'file',
      },
      {
        key: 'i',
        ctrl: true,
        description: 'Import directory',
        action: handleImport,
        category: 'file',
      },
      {
        key: 't',
        ctrl: true,
        description: 'Transcribe selected',
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
        action: enterReviewMode,
        category: 'navigation',
      },
      {
        key: 's',
        ctrl: true,
        description: 'Save annotation',
        action: handleSaveAnnotation,
        category: 'edit',
      },
      {
        key: 'z',
        ctrl: true,
        description: 'Undo',
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
        action: () => handleRedo(),
        category: 'edit',
        allowInReview: true, // handleRedo self-guards on review surfaces (no-op there)
      },
      {
        key: 'd',
        ctrl: true,
        description: 'Toggle verified',
        action: handleToggleVerify,
        category: 'edit',
      },
      {
        key: 'Delete',
        description: 'Delete segment',
        action: handleDeleteWithConfirm,
        category: 'edit',
      },
      {
        key: 'f',
        ctrl: true,
        description: 'Focus search',
        action: () => document.querySelector<HTMLInputElement>('[type=search]')?.focus(),
        category: 'navigation',
        allowInEditable: true,
      },
      {
        key: ',',
        ctrl: true,
        description: 'Open settings',
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
        action: openValidationPanel,
        category: 'navigation',
      },
      {
        key: 'r',
        ctrl: true,
        shift: true,
        description: 'Open Review Inbox',
        action: openReviewInbox,
        category: 'navigation',
      },
      {
        key: '/',
        ctrl: true,
        description: 'Keyboard shortcuts',
        action: () => showKeyboardHelp.set(true),
        category: 'navigation',
      },
      {
        key: 's',
        shift: true,
        description: 'Toggle sidebar panel',
        action: () => (sidebarOpen = !sidebarOpen),
        category: 'navigation',
      },
      {
        key: 'd',
        shift: true,
        description: 'Toggle stats dashboard',
        action: () => (statsOpen = !statsOpen),
        category: 'navigation',
      },
      {
        key: 'j',
        description: 'Next segment',
        action: () => navigateSegment('down'),
        category: 'navigation',
      },
      {
        key: 'k',
        description: 'Previous segment',
        action: () => navigateSegment('up'),
        category: 'navigation',
      },
      {
        key: '/',
        shift: true,
        description: 'Keyboard shortcuts (? key)',
        action: () => showKeyboardHelp.set(true),
        category: 'navigation',
      },
      {
        key: '?',
        description: 'Keyboard shortcuts (? key)',
        action: () => showKeyboardHelp.set(true),
        category: 'navigation',
      },
      {
        key: ' ',
        ctrl: true,
        description: 'Play/pause',
        action: () => (isAudioPlaying = !isAudioPlaying),
        category: 'playback',
      },
      {
        key: 'Enter',
        ctrl: true,
        description: 'Toggle verification',
        action: handleToggleVerify,
        category: 'playback',
      },
      {
        key: 'ArrowLeft',
        description: 'Rewind 5s',
        action: () => {
          clearWordOverride(); // a manual scrub leaves word-playback mode
          currentTime = Math.max(0, currentTime - 5);
        },
        category: 'playback',
      },
      {
        key: 'ArrowRight',
        description: 'Forward 5s',
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
    const parsed = parseActionableError(error);
    notifications.error(parsed.message || fallbackMessage, {
      detail: parsed.detail,
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
    viewMode = 'review';
    sidebarOpen = false;
    statsOpen = false;
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
      notifications.error($t('settingsLoadFailed'), { detail: String(e) });
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
      notifications.error($t('agentReport.loadFailed'), { detail: String(e) });
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
      notifications.error($t('agentReport.stageLoadFailed'), { detail: String(e) });
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
      notifications.warning($t('agenticReadiness.checkFailed'), { detail: String(e) });
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
      const cancelled = String(e).includes('No directory selected');
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

  // When the OmniASR-7B champion is unavailable or fails (its WSL server is down), the app NEVER
  // silently drops to a smaller model. It surfaces a choice: retry the champion once its server is
  // up, or transcribe this one clip with the offline model. The dialog names both engines so the
  // owner always knows which produced the text.
  function promptChampionFallback(retryChampion: () => void, useOffline: () => void) {
    showConfirmDialog.set({
      title: $t('asr.championUnavailableTitle'),
      message: $t('asr.championUnavailableMessage'),
      confirmLabel: $t('asr.tryAgain'),
      danger: false,
      onConfirm: retryChampion,
      secondary: { label: $t('asr.useOfflineModel'), onClick: useOffline },
    });
  }

  async function handleTranscribe() {
    const seg = $selectedSegment;
    if (!seg || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
    // Re-transcription replaces this segment's text with fresh MACHINE output, so drop any pending
    // autosave for it first — otherwise the debounced pre-edit annotation fires AFTER this write and
    // clobbers the new transcript (the same clobber the delete paths already cancel against).
    if (autosave.pendingId() === seg.id) cancelPendingSave();
    startOperation('transcribe');
    isProcessing.set(true);
    pipelinePhase.set('transcribing');
    statusMessage.set($t('transcribing'));
    try {
      const result = await api.transcribeSegment(seg.audioPath, seg.alignmentJson, seg.id);
      const rawTranscript = result.rawTranscript;
      const annotatedTranscript = result.text;
      let normalizedTranscript = seg.normalizedTranscript;
      if ($settings.autoNormalize) {
        // Best-effort: a normalize failure (rate limit, validation) must never discard the
        // just-succeeded multi-second GPU transcription — save the raw result and say what failed.
        try {
          normalizedTranscript = await api.normalizeText(result.text);
        } catch (normalizeError) {
          notifications.error($t('notifications.normalizeFailed'), { detail: String(normalizeError) });
        }
      }
      const updatedSeg = {
        // freshRow-by-id: the store row may have gained columns (speaker id autosave, background
        // align stamps) during the multi-second ASR await — never upsert the stale pre-await copy.
        ...($segments.find((s) => s.id === seg.id) ?? seg),
        rawTranscript,
        annotatedTranscript,
        normalizedTranscript,
        // A re-transcription is machine output — reset verified so it isn't kept as human-verified.
        verified: false,
      };
      await api.updateSegment(updatedSeg);
      // Align LAST, with the segment id: align_segment persists the merged word timings and stamps
      // the honest alignment_quality in the backend. Order matters — updateSegment upserts the WHOLE
      // row including alignment_quality (insert_segment ON CONFLICT), so aligning before it would let
      // the stale quality from `seg` clobber a fresh ctc_forced stamp. Best-effort: a failed align
      // keeps the successfully saved transcript.
      if ($settings.autoAlign) {
        const alignText = normalizedTranscript ?? result.text;
        if (alignText?.trim()) {
          try {
            const ts = await api.alignSegment(seg.audioPath, alignText, seg.alignmentJson, seg.id);
            wordTimestamps.set(ts);
          } catch (alignError) {
            notifications.error($t('notifications.alignmentFailed'), { detail: String(alignError) });
          }
        }
      }
      await loadSegments();
      // A save armed WHILE the multi-second ASR await ran is still pending here and would fire AFTER
      // this write, re-clobbering the fresh machine transcript — drop it too. (JS is single-threaded, so
      // no pending debounce macrotask can interleave between the await resolving and this line.)
      if (autosave.pendingId() === seg.id) cancelPendingSave();
      notifications.success($t('notifications.transcriptionComplete'));
    } catch (e) {
      // The champion (7B) is the primary engine. If it's down, offer retry-or-offline instead of a
      // dead-end error — the app never silently substitutes a smaller model on this path.
      if (api.is7bUnavailableError(e)) {
        promptChampionFallback(handleTranscribe, handleTranscribeFinetuned);
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

  // Opt-in: transcribe the selected segment with the constrained Kurdish-token decode (guarantees
  // Kurdish-script output) via the additive `transcribe_segment_constrained` backend command. The
  // default `handleTranscribe` (sherpa path) is unchanged.
  async function handleTranscribeConstrained() {
    const seg = $selectedSegment;
    if (!seg || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
    // Drop any pending autosave for this segment so it can't clobber the fresh machine transcript.
    if (autosave.pendingId() === seg.id) cancelPendingSave();
    startOperation('transcribe');
    isProcessing.set(true);
    pipelinePhase.set('transcribing');
    statusMessage.set($t('transcribing'));
    try {
      const result = await api.transcribeSegmentConstrained(seg.audioPath, seg.alignmentJson);
      const updatedSeg = {
        // freshRow-by-id: never upsert the stale pre-await copy (it reverts columns persisted during
        // the multi-second ASR await — speaker autosave, background align stamps).
        ...($segments.find((s) => s.id === seg.id) ?? seg),
        rawTranscript: result.rawTranscript,
        annotatedTranscript: result.text,
        // A re-transcription is machine output — reset verified so it isn't kept as human-verified.
        verified: false,
      };
      await api.updateSegment(updatedSeg);
      await loadSegments();
      // A save armed WHILE the multi-second ASR await ran is still pending here and would fire AFTER
      // this write, re-clobbering the fresh machine transcript — drop it too. (JS is single-threaded, so
      // no pending debounce macrotask can interleave between the await resolving and this line.)
      if (autosave.pendingId() === seg.id) cancelPendingSave();
      notifications.success($t('notifications.transcriptionComplete'));
    } catch (e) {
      notifyActionableError(e, $t('errors.transcriptionFailed'));
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('transcribe');
    }
  }

  // Opt-in: transcribe the selected segment with the embedded fine-tuned Kurdish model (measured 21.0% CER, N=900)
  // via `transcribe_segment_finetuned`. The default `handleTranscribe` (sherpa) path is unchanged.
  async function handleTranscribeFinetuned() {
    const seg = $selectedSegment;
    if (!seg || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
    // Drop any pending autosave for this segment so it can't clobber the fresh machine transcript.
    if (autosave.pendingId() === seg.id) cancelPendingSave();
    startOperation('transcribe');
    isProcessing.set(true);
    pipelinePhase.set('transcribing');
    statusMessage.set($t('transcribing'));
    try {
      const result = await api.transcribeSegmentFinetuned(seg.audioPath, seg.alignmentJson);
      const updatedSeg = {
        // freshRow-by-id: same stale-spread guard as the other transcribe handlers.
        ...($segments.find((s) => s.id === seg.id) ?? seg),
        rawTranscript: result.rawTranscript,
        annotatedTranscript: result.text,
        // A re-transcription is machine output — reset verified so it isn't kept as human-verified.
        verified: false,
      };
      await api.updateSegment(updatedSeg);
      await loadSegments();
      // A save armed WHILE the multi-second ASR await ran is still pending here and would fire AFTER
      // this write, re-clobbering the fresh machine transcript — drop it too. (JS is single-threaded, so
      // no pending debounce macrotask can interleave between the await resolving and this line.)
      if (autosave.pendingId() === seg.id) cancelPendingSave();
      notifications.success($t('notifications.transcriptionComplete'));
    } catch (e) {
      notifyActionableError(e, $t('errors.transcriptionFailed'));
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('transcribe');
    }
  }

  // Opt-in cloud STT: re-transcribe the selected segment with ElevenLabs Scribe. Consent is enforced
  // server-side; the button is only shown when cloud-STT opt-in is on, so this never surprises a
  // fully-offline user.
  async function handleTranscribeScribe() {
    const seg = $selectedSegment;
    if (!seg || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
    // Drop any pending autosave for this segment so it can't clobber the fresh machine transcript.
    if (autosave.pendingId() === seg.id) cancelPendingSave();
    startOperation('transcribe');
    isProcessing.set(true);
    pipelinePhase.set('transcribing');
    statusMessage.set($t('transcribing'));
    try {
      const text = await api.transcribeWithScribe(seg.audioPath, seg.alignmentJson);
      // A fresh MACHINE re-transcription: reset verified=false so the new text is never presented as
      // human-verified (the honesty guardrail), and so the human re-reviews the replaced transcript.
      // freshRow-by-id: same stale-spread guard as the other transcribe handlers.
      await api.updateSegment({
        ...($segments.find((s) => s.id === seg.id) ?? seg),
        rawTranscript: text,
        annotatedTranscript: text,
        verified: false,
      });
      await loadSegments();
      // A save armed WHILE the multi-second ASR await ran is still pending here and would fire AFTER
      // this write, re-clobbering the fresh machine transcript — drop it too. (JS is single-threaded, so
      // no pending debounce macrotask can interleave between the await resolving and this line.)
      if (autosave.pendingId() === seg.id) cancelPendingSave();
      notifications.success($t('notifications.transcriptionComplete'));
    } catch (e) {
      notifyActionableError(e, $t('errors.transcriptionFailed'));
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('transcribe');
    }
  }

  // Opt-in cloud STT: add an independent Scribe jury vote for the selected segment. The jury pipeline
  // folds Scribe votes into consensus on its next run.
  async function handleAddScribeVote() {
    const seg = $selectedSegment;
    if (!seg || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
    startOperation('scribe-vote');
    isProcessing.set(true);
    try {
      const count = await api.addScribeVotes([seg.id]);
      if (count > 0) {
        notifications.success($t('scribe.voteAdded'));
      } else {
        notifications.info($t('scribe.voteExists'));
      }
    } catch (e) {
      notifyActionableError(e, $t('scribe.voteFailed'));
    } finally {
      isProcessing.set(false);
      endOperation('scribe-vote');
    }
  }

  async function handleNormalize() {
    const seg = $selectedSegment;
    if (!seg?.rawTranscript) return;
    if (!requireDesktopRuntime()) return;
    try {
      const normalizedTranscript = await api.normalizeText(seg.rawTranscript);
      const updatedSeg = {
        // freshRow-by-id: a concurrent verify/edit/align stamp may have updated the row during the
        // normalize await — never upsert the stale pre-await copy (the whole-row clobber class), same
        // guard as the transcribe handlers above.
        ...($segments.find((s) => s.id === seg.id) ?? seg),
        normalizedTranscript,
      };
      await api.updateSegment(updatedSeg);
      // Update the store only AFTER the persist succeeds. Mutating it first left unsaved state in the
      // UI on a failed save — which a later unrelated auto-save would then silently persist.
      segments.update((arr) =>
        arr.map((s) => (s.id === seg.id ? { ...s, normalizedTranscript } : s)),
      );
      notifications.success($t('notifications.textNormalized'));
    } catch (e) {
      notifications.error($t('notifications.normalizationFailed'), { detail: String(e) });
    }
  }

  async function handleSaveAnnotation() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;
    try {
      // Field-level update, NOT a whole-row updateSegment. The Save button is reachable while a background
      // batch-verify has written verified=true to the DB but the store row is still stale (verified=false);
      // a whole-row upsert of $selectedSegment would revert that concurrent verify — and freshRow-by-id
      // wouldn't help because the store itself is the stale source. update_segment_fields reads the FRESH
      // row under the DB lock, applies ONLY annotatedTranscript, and still records undo history — the same
      // safe path the oninput autosave already uses (scheduleAutoSave({ annotatedTranscript })).
      await api.updateSegmentFields(seg.id, { annotatedTranscript: seg.annotatedTranscript });
      await historyStore.refresh();
      if (historyPanel) {
        historyPanel.recordAction(
          `Saved annotation: "${truncateFilename(segmentSourceFilename(seg.audioPath))}"`,
          'edit',
        );
      }
      notifications.success($t('notifications.annotationSaved'));
    } catch (e) {
      notifications.error($t('notifications.saveFailed'), { detail: String(e) });
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
      notifications.info($t('notifications.undone', { what: description ?? $t('notifications.lastActionReverted') }));
      await loadSegments();
      if (historyPanel) {
        historyPanel.recordAction(`Reverted: ${description ?? 'action'}`, 'edit');
      }
    } catch (e) {
      notifications.error($t('notifications.undoFailedDetail', { error: String(e) }));
    }
  }

  async function handleRedo() {
    if (!requireDesktopRuntime()) return;
    // Same split-state hazard as handleUndo: the global redo has no meaning on the review surfaces.
    if (viewMode === 'review' || $showReviewInbox) return;
    try {
      const description = await historyStore.redo();
      notifications.info($t('notifications.redone', { what: description ?? $t('notifications.lastActionReapplied') }));
      await loadSegments();
      if (historyPanel) {
        historyPanel.recordAction(`Redone: ${description ?? 'action'}`, 'edit');
      }
    } catch (e) {
      notifications.error($t('notifications.redoFailedDetail', { error: String(e) }));
    }
  }

  async function handleToggleVerify() {
    const seg = $selectedSegment;
    if (!seg || verifyInFlight || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
    verifyInFlight = true;

    const originalVerified = seg.verified;
    const nextVerified = !originalVerified;

    // Optimistic Update
    segments.update((list) =>
      list.map((s) => (s.id === seg.id ? { ...s, verified: nextVerified } : s)),
    );
    if (historyPanel) {
      historyPanel.recordAction(
        `${nextVerified ? 'Verified' : 'Unverified'} segment: ${truncateFilename(segmentSourceFilename(seg.audioPath))}`,
        'verify',
      );
    }

    try {
      // Field-level (fresh-read-by-id, verified only) — NOT a whole-row updateSegment upsert. A whole-row
      // spread of the stale `seg` would revert any column a concurrent NO-$isProcessing writer changed on
      // that row — notably the WSL-7B refinement loop's raw_transcript write (it runs on wsl-log events, so
      // the Verify button stays live). Same fix the sibling save handlers already got.
      await api.updateSegmentFields(seg.id, { verified: nextVerified });
      // Invalidate any in-flight load so its stale pre-write data won't clobber the verified write.
      // This closes the race window: a background load dispatched before this write will check its
      // generation and return early rather than applying pre-write data.
      segments.bumpLoadGeneration();
      // Then re-apply verified state for consistency (in case a load started between optimize and bump).
      segments.update((list) =>
        list.map((s) => (s.id === seg.id ? { ...s, verified: nextVerified } : s)),
      );
      // The bump above may have cancelled a legitimate reload (e.g. a WSL batch completion refresh
      // walking its pages) whose data would then never arrive — run a FRESH load, which reads
      // post-write rows and so carries both the batch results and this verify.
      void segments.load();
      await historyStore.refresh();
    } catch (e) {
      // Revert Svelte store state on error
      segments.update((list) =>
        list.map((s) => (s.id === seg.id ? { ...s, verified: originalVerified } : s)),
      );
      notifications.error($t('errors.verifyFailed'), { detail: String(e) });
    } finally {
      verifyInFlight = false;
    }
  }

  function handleDeleteWithConfirm() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;
    showConfirmDialog.set({
      title: $t('deleteSegment'),
      message: $t('deleteSegmentConfirm').replace('{name}', seg.audioPath.split(/[/\\]/).pop() ?? ''),
      onConfirm: handleDelete,
    });
  }

  async function handleSaveSpeaker() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;
    try {
      // Field-level update, same reason as handleSaveAnnotation: a whole-row updateSegment of the stale
      // $selectedSegment would revert a concurrent batch-verify's verified=true. update_segment_fields
      // writes only speakerId onto the fresh row (and records undo history), like the oninput autosave.
      await api.updateSegmentFields(seg.id, { speakerId: seg.speakerId });
      notifications.success($t('speaker.saved'));
    } catch (e) {
      notifications.error($t('notifications.saveFailed'), { detail: String(e) });
    }
  }

  async function handleExport() {
    if (!requireDesktopRuntime()) return;
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
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
      const path = await save({
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
      notifications.error($t('exportDataset.failed'), { detail: String(e) });
    }
  }

  async function handleExportTranscript() {
    if ($isProcessing || $segmentStats.total === 0) return;
    if (!requireDesktopRuntime()) return;
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const path = await save({
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
      notifications.error($t('exportTranscript.failed'), { detail: String(e) });
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
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({ directory: true, multiple: false });
      if (!dir || typeof dir !== 'string') return;
      await api.exportHuggingfaceDataset(dir);
      notifications.success($t('exportHuggingface.success'), { detail: dir });
    } catch (e) {
      notifications.error($t('exportHuggingface.failed'), { detail: String(e) });
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
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({ directory: true, multiple: false });
      if (!dir || typeof dir !== 'string') return;

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
      notifications.error($t('exportAudio.failed'), { detail: String(e) });
    } finally {
      isProcessing.set(false);
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set($t('ready'));
      endOperation('export-audio');
    }
  }

  async function handleBatchTranscribe(mode: 'empty' | 'selected' | 'filtered') {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;

    const ids =
      mode === 'empty'
        ? $segments.filter((s) => !s.rawTranscript?.trim()).map((s) => s.id)
        : mode === 'selected'
          ? $selectedSegmentId
            ? [$selectedSegmentId]
            : []
          : $filteredSegments.map((s) => s.id);

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

  async function handleBatchVerify(mode: 'pending' | 'selected') {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;

    const ids =
      mode === 'pending'
        ? // Exclude no-content rows (placeholder "[Pending WSL 7B ASR]" / "[ASR unavailable…]" or empty):
          // "Verify All Pending" must never mark a placeholder verified. It wouldn't ship (export excludes
          // placeholders) but it would STRAND the row — the WSL-7B refinement loop skips verified rows
          // (update_asr_transcript_if_unreviewed's verified=0 guard), so the placeholder could never be
          // filled — and it dishonestly counts as verified. Root of the recurring class iter 127 only
          // band-aided at the count. Same hasRealTranscript gate the verified tally uses.
          $segments.filter((s) => !s.verified && hasRealTranscript(s)).map((s) => s.id)
        : $selectedSegmentId
          ? [$selectedSegmentId]
          : [];

    if (mode === 'selected' && !$selectedSegmentId) {
      notifications.warning($t('batchVerify.noSelection'));
      return;
    }
    if (ids.length === 0) {
      notifications.info($t('batchVerify.nothingToVerify'));
      return;
    }

    startOperation('batch-verify');
    statusMessage.set($t('batchVerify.progress', { n: String(ids.length) }));
    try {
      await api.batchVerify(ids, true);
    } catch (e) {
      notifications.error($t('batchVerify.failed'), { detail: String(e) });
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set($t('ready'));
      endOperation('batch-verify');
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
    const ids = $filteredSegments.map((s) => s.id);
    if (ids.length === 0) {
      notifications.info($t('batchAssignSpeaker.nothingToAssign'));
      return;
    }
    startOperation('batch-assign-speaker');
    statusMessage.set($t('batchAssignSpeaker.progress', { n: String(ids.length) }));
    try {
      await api.batchAssignSpeaker(ids, speaker);
    } catch (e) {
      notifications.error($t('batchAssignSpeaker.failed'), { detail: String(e) });
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set($t('ready'));
      endOperation('batch-assign-speaker');
    }
  }

  async function handleBatchNormalize() {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
    const ids = $filteredSegments.filter((s) => s.rawTranscript?.trim()).map((s) => s.id);
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
      notifications.error($t('batchNormalize.failed'), { detail: String(e) });
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
        : $filteredSegments.map((s) => s.id);
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
      notifications.error($t('rediarize.failed'), { detail: String(e) });
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('rediarize');
    }
  }

  function handleDeleteFilteredWithConfirm() {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
    const ids = $filteredSegments.map((s) => s.id);
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
    // Cancel a pending autosave for any segment in this batch, so its flush can't resurrect a deleted row.
    if (autosave.pendingId() !== null && ids.includes(autosave.pendingId()!)) cancelPendingSave();
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
      notifications.error($t('batchDelete.failed'), { detail: String(e) });
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

    // Cancel any pending autosave for THIS segment before deleting, so its debounced flush can't fire
    // after the row is gone and resurrect it via update_segment's INSERT-ON-CONFLICT.
    if (autosave.pendingId() === seg.id) cancelPendingSave();

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
      notifications.error($t('notifications.deleteFailed'), { detail: String(e) });
    }
  }

  async function handleAlign() {
    const seg = $selectedSegment;
    if (!seg) return;
    if (!requireDesktopRuntime()) return;
    const text = seg.annotatedTranscript ?? seg.normalizedTranscript ?? seg.rawTranscript;
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
      notifications.error($t('notifications.alignmentFailed'), { detail: String(e) });
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set($t('ready'));
      endOperation('align');
    }
  }

  async function loadWaveform(path: string, alignmentJson?: string | null) {
    if (!tauriAvailable) {
      waveformData = [];
      return;
    }
    try {
      waveformData = await api.getWaveform(path, 200, alignmentJson);
    } catch {
      waveformData = [];
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

  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }
</script>

<div class="h-screen flex flex-col bg-app text-default" data-testid="app-root">
  {#if quarantineNotice}
    <div
      class="flex items-center justify-between gap-3 border-b border-red-600/50 bg-red-950/50 px-4 py-2"
      data-testid="quarantine-banner"
    >
      <span class="text-sm text-red-200">
        {$t('db.quarantined')
          .replace('{files}', String(quarantineNotice.quarantinedFiles.length))
          .replace('{snapshots}', String(quarantineNotice.snapshotCount))}
      </span>
      <div class="flex items-center gap-2">
        <!-- Acknowledge & archive (true-10 audit): the pin had no in-app release — pruning stayed
             refused forever while snapshots accumulated a full DB copy every 10 minutes. Archiving
             the *.corrupt.* files into <data_dir>/quarantine/ releases the pin, keeps the bytes
             salvageable, and is the honest exit from the quarantine state. -->
        <button
          type="button"
          class="btn btn-secondary !text-xs"
          data-testid="acknowledge-quarantine-btn"
          onclick={async () => {
            try {
              const moved = await api.acknowledgeQuarantine();
              notifications.success($t('db.quarantineAcknowledged', { count: String(moved) }));
              quarantineNotice = null;
            } catch (e) {
              notifications.error($t('db.quarantineAcknowledgeFailed'), { detail: String(e) });
            }
          }}
        >
          {$t('db.quarantineAcknowledge')}
        </button>
        <button
          type="button"
          class="btn btn-ghost !text-xs"
          data-testid="dismiss-quarantine-btn"
          onclick={() => (quarantineNotice = null)}
        >
          {$t('db.quarantineDismiss')}
        </button>
      </div>
    </div>
  {/if}
  {#if interruptedImport}
    <div
      class="flex items-center justify-between gap-3 border-b border-amber-600/40 bg-amber-950/40 px-4 py-2"
      data-testid="resume-import-banner"
    >
      <span class="text-sm text-amber-200">
        {$t('import.interrupted')
          .replace('{done}', String(interruptedImport.completedPaths.length))
          .replace('{total}', String(interruptedImport.totalFiles))
          .replace('{dir}', interruptedImport.dir)}
      </span>
      <div class="flex items-center gap-2">
        <button
          type="button"
          class="btn btn-primary !text-xs"
          data-testid="resume-import-btn"
          onclick={resumeImport}
        >
          {$t('import.resume')}
        </button>
        <button
          type="button"
          class="btn btn-ghost !text-xs"
          data-testid="dismiss-import-btn"
          onclick={dismissInterruptedImport}
        >
          {$t('import.discard')}
        </button>
      </div>
    </div>
  {/if}
  <!-- Top Bar -->
  <header
    data-testid="top-bar"
    class="flex items-center justify-between px-4 py-2 glass border-b border-line shrink-0 z-30"
  >
    <div class="flex items-center gap-3">
      <h1 class="text-sm font-bold tracking-tight">
        <span class="text-cortex-400">CORTEX</span>
        <span class="text-cortex-200 ms-1">{$t('app.subtitle')}</span>
      </h1>
      <span
        class="text-[10px] text-cortex-500 bg-cortex-900 px-2 py-0.5 rounded-full border border-cortex-800/50"
        >v2.0</span
      >
      {#if tauriAvailable}
        <EngineStatusPill />
        <JobsActivityPill />
      {/if}
      {#if $isProcessing}
        <span class="flex items-center gap-1 text-xs text-cortex-400">
          <svg class="animate-spin w-3 h-3" fill="none" viewBox="0 0 24 24"
            ><circle
              class="opacity-25"
              cx="12"
              cy="12"
              r="10"
              stroke="currentColor"
              stroke-width="4"
            /><path
              class="opacity-75"
              fill="currentColor"
              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
            /></svg
          >
          {$t('processing')}
        </span>
      {/if}
    </div>

    <div class="flex items-center gap-2">
      <span class="text-xs text-cortex-500"
        >{$segmentStats.total}
        {$t('segments')} · {$segmentStats.verified}
        {$t('verifiedCount')}</span
      >
      <span class="text-[10px] text-cortex-600">|</span>
      {#if !sidebarOpen}
        <button
          class="btn btn-secondary !text-xs relative"
          onclick={() => (sidebarOpen = true)}
          title="Show segments (⇧S)"
          aria-label={$t('showSegments')}
        >
          <svg class="w-3.5 h-3.5 inline" fill="none" stroke="currentColor" viewBox="0 0 24 24"
            ><path
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="2"
              d="M4 6h16M4 12h16M4 18h16"
            /></svg
          >
          {#if showHotkeyOverlay}
            <span
              class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
              >⇧S</span
            >
          {/if}
        </button>
      {/if}
      {#if !statsOpen}
        <button
          class="btn btn-secondary !text-xs relative"
          onclick={() => (statsOpen = true)}
          title="Show stats (⇧D)"
          aria-label={$t('showStats')}
        >
          <svg class="w-3.5 h-3.5 inline" fill="none" stroke="currentColor" viewBox="0 0 24 24"
            ><path
              stroke-linecap="round"
              stroke-linejoin="round"
              stroke-width="2"
              d="M9 19v-6a2 2 0 00-2-2H5a2 2 0 00-2 2v6a2 2 0 002 2h2a2 2 0 002-2zm0 0V9a2 2 0 012-2h2a2 2 0 012 2v10m-6 0a2 2 0 002 2h2a2 2 0 002-2m0 0V5a2 2 0 012-2h2a2 2 0 012 2v14a2 2 0 01-2 2h-2a2 2 0 01-2-2z"
            /></svg
          >
          {#if showHotkeyOverlay}
            <span
              class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
              >⇧D</span
            >
          {/if}
        </button>
      {/if}
      <button
        class="btn btn-secondary !text-xs relative"
        onclick={handleOpenFile}
        disabled={!tauriAvailable || $isProcessing}
        title={tauriAvailable ? 'Ctrl+O' : $t('desktopRuntimeRequired')}
        aria-label={$t('openAudioFile')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M12 4v16m8-8H4"
          /></svg
        >
        {$t('open')}
        {#if showHotkeyOverlay}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^O</span
          >
        {/if}
      </button>
      <button
        class="btn btn-secondary !text-xs relative"
        onclick={handleImport}
        disabled={!tauriAvailable || $isProcessing}
        title={tauriAvailable ? 'Ctrl+I' : $t('desktopRuntimeRequired')}
        aria-label={$t('importDirectory')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M4 16v2a2 2 0 002 2h12a2 2 0 002-2v-2m-4-4l-4 4m0 0l-4-4m4 4V4"
          /></svg
        >
        {$t('import')}
        {#if showHotkeyOverlay}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^I</span
          >
        {/if}
      </button>
      <button
        class="btn btn-secondary !text-xs"
        onclick={handleExport}
        disabled={!tauriAvailable || $isProcessing || $segmentStats.total === 0}
        title={!tauriAvailable ? $t('desktopRuntimeRequired') : $t('export')}
        aria-label={$t('export')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M4 16v2a2 2 0 002 2h12a2 2 0 002-2v-2m-4-4l-4 4m0 0l-4-4m4 4V4"
          /></svg
        >
        {$t('export')}
      </button>
      <button
        data-testid="export-transcript-btn"
        class="btn btn-secondary !text-xs"
        onclick={handleExportTranscript}
        disabled={!tauriAvailable || $isProcessing || $segmentStats.total === 0}
        title={!tauriAvailable ? $t('desktopRuntimeRequired') : $t('exportTranscript.title')}
        aria-label={$t('exportTranscript')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
          /></svg
        >
        {$t('exportTranscript')}
      </button>
      <button
        data-testid="hf-export-btn"
        class="btn btn-secondary !text-xs"
        onclick={handleExportHuggingface}
        disabled={!tauriAvailable ||
          $isProcessing ||
          $segmentStats.total === 0 ||
          trainingExportBlocked}
        aria-label={$t('exportHuggingface.label')}
        title={trainingExportTitle}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M4 7v10c0 2 1 3 3 3h10c2 0 3-1 3-3V7c0-2-1-3-3-3H7C5 4 4 5 4 7z"
          /><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M9 12h6"
          /></svg
        >
        {$t('exportHuggingface.label')}
      </button>
      <button
        class="btn btn-secondary !text-xs"
        onclick={handleExportAudio}
        disabled={!tauriAvailable || $isProcessing || $segmentStats.verified === 0}
        title={!tauriAvailable ? $t('desktopRuntimeRequired') : $t('exportAudio.label')}
        aria-label={$t('exportAudio.label')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M9 19V6l12-3v13M9 19c0 1.105-1.343 2-3 2s-3-.895-3-2 1.343-2 3-2 3 .895 3 2zm12-3c0 1.105-1.343 2-3 2s-3-.895-3-2 1.343-2 3-2 3 .895 3 2z"
          /></svg
        >
        {$t('exportAudio.label')}
      </button>
      <button
        data-testid="wsl-btn"
        class="btn btn-secondary !text-xs relative"
        onclick={openWslConsole}
        disabled={!tauriAvailable || $isProcessing}
        title={tauriAvailable ? 'Local 7B ASR (WSL)' : $t('desktopRuntimeRequired')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z"
          /></svg
        >
        Local 7B ASR (WSL)
      </button>
      <button
        data-testid="review-correct-btn"
        class="btn !text-xs relative {$segmentStats.pending > 0 ? 'btn-primary' : 'btn-secondary'}"
        onclick={enterReviewMode}
        disabled={!tauriAvailable || $segmentStats.total === 0}
        title={tauriAvailable ? $t('reviewCorrect.tooltip') : $t('desktopRuntimeRequired')}
        aria-label={$t('reviewCorrect.label')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z"
          /></svg
        >
        {$t('reviewCorrect.label')}
        {#if $segmentStats.pending > 0}
          <span
            data-testid="review-pending-badge"
            class="absolute -top-2 -right-2 min-w-[18px] h-[18px] flex items-center justify-center bg-amber-400 text-black text-[10px] font-bold px-1 rounded-full shadow border border-amber-500 select-none z-50 pointer-events-none"
            >{$segmentStats.pending}</span
          >
        {/if}
      </button>
      <button
        data-testid="validate-btn"
        class="btn btn-secondary !text-xs relative"
        onclick={openValidationPanel}
        disabled={!tauriAvailable || $isProcessing || $segmentStats.total === 0}
        title={tauriAvailable ? 'Ctrl+Shift+V' : $t('desktopRuntimeRequired')}
        aria-label={$t('validate')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"
          /></svg
        >
        {$t('validate')}
        {#if showHotkeyOverlay}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^+V</span
          >
        {/if}
      </button>
      <button
        data-testid="review-inbox-btn"
        class="btn btn-secondary !text-xs relative"
        onclick={openReviewInbox}
        disabled={!tauriAvailable || $isProcessing}
        title={tauriAvailable ? 'Review Inbox (Ctrl+Shift+R)' : $t('desktopRuntimeRequired')}
        aria-label={$t('reviewInbox')}
      >
        {$t('reviewInbox')}
        {#if showHotkeyOverlay}
          <span
            class="absolute -top-1.5 -right-1.5 bg-purple-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-purple-500 select-none z-50 pointer-events-none"
            >^+R</span
          >
        {/if}
      </button>
      <button
        data-testid="settings-btn"
        class="btn btn-primary !text-xs relative"
        onclick={() => openSettings()}
        title="{modKey}+,"
        aria-label={$t('openSettings')}
      >
        <svg class="w-3.5 h-3.5 inline me-1" fill="none" stroke="currentColor" viewBox="0 0 24 24"
          ><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 002.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 001.066 2.573c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 00-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 00-2.573 1.066c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 00-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 00-1.066-2.573c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 001.066-2.573c-.94-1.543.826-3.31 2.37-2.37.996.608 2.296.07 2.572-1.065z"
          /><path
            stroke-linecap="round"
            stroke-linejoin="round"
            stroke-width="2"
            d="M15 12a3 3 0 11-6 0 3 3 0 016 0z"
          /></svg
        >
        {$t('settings')}
        {#if showHotkeyOverlay}
          <span
            class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
            >^,</span
          >
        {/if}
      </button>
      <button
        data-testid="locale-toggle"
        class="btn btn-secondary !text-xs"
        onclick={() => locale.set($locale === 'en' ? 'ckb' : 'en')}
        title={$t('localeToggle')}
        aria-label={$t('localeToggle')}
      >
        {$locale === 'ckb' ? $t('english') : $t('kurdish')}
      </button>
    </div>
  </header>

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
    <ActivityRail
      view={viewMode}
      onSelect={(id) => {
        if (id === 'settings') openSettings();
        else {
          viewMode = id as 'curate' | 'insights' | 'review';
          // Review is a focused, distraction-free mode — collapse the side panels.
          if (id === 'review') {
            sidebarOpen = false;
            statsOpen = false;
          }
        }
      }}
    />
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
            <div class="flex flex-wrap gap-1">
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={() => handleBatchVerify('pending')}
                disabled={!tauriAvailable || $isProcessing || $segmentStats.pending === 0}
                title={tauriAvailable ? $t('batchVerify.allPending') : $t('desktopRuntimeRequired')}
                >{$t('batchVerify.allPending')}</button
              >
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={() => handleBatchVerify('selected')}
                disabled={!tauriAvailable || $isProcessing || !$selectedSegmentId}
                title={tauriAvailable ? $t('batchVerify.selected') : $t('desktopRuntimeRequired')}
                >{$t('batchVerify.selected')}</button
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
                title={tauriAvailable ? 'Speaker Management' : $t('desktopRuntimeRequired')}
                >{$t('speakers')}</button
              >
              <button
                class="btn btn-secondary btn-sm !text-[10px] flex-1"
                onclick={openDatasetMerge}
                disabled={!tauriAvailable || $isProcessing}
                title={tauriAvailable ? 'Merge Dataset JSON' : $t('desktopRuntimeRequired')}
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
                        <span
                          class="text-emerald-400 text-[10px] shrink-0 drop-shadow-[0_0_5px_rgba(52,211,153,0.5)]"
                          >✓</span
                        >
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
                          title="ASR Confidence Score"
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
                      <span class="text-[11px] text-cortex-500 truncate mt-0.5" dir="rtl" lang="ckb">
                        {item.annotatedTranscript ??
                          item.normalizedTranscript ??
                          item.rawTranscript ??
                          '...'}
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
              <div
                data-testid="segments-empty-state"
                class="flex h-full flex-col items-center justify-center gap-3 px-6 text-center animate-fade-in"
              >
                {#if $searchQuery}
                  <div
                    class="flex h-12 w-12 items-center justify-center rounded-full bg-surface-2 text-subtle"
                  >
                    <svg
                      width="22"
                      height="22"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                      stroke-linecap="round"
                    >
                      <circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" />
                    </svg>
                  </div>
                  <div>
                    <p class="text-sm font-medium text-default">{$t('noResultsFound')}</p>
                    <p class="mt-1 max-w-[14rem] truncate text-xs text-subtle">“{$searchQuery}”</p>
                  </div>
                {:else}
                  <div
                    class="flex h-14 w-14 items-center justify-center rounded-2xl bg-accent-soft text-accent"
                  >
                    <svg
                      width="26"
                      height="26"
                      viewBox="0 0 24 24"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.5"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    >
                      <path d="M12 18a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v10a3 3 0 0 0 3 3Z" />
                      <path d="M19 11a7 7 0 0 1-14 0M12 18v4M8 22h8" />
                    </svg>
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
              <svg class="w-5 h-5 shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24"
                ><path
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  stroke-width="2"
                  d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z"
                /></svg
              >
              <span>{$t($segmentStats.pending === 1 ? 'reviewCorrect.ctaOne' : 'reviewCorrect.cta', { n: String($segmentStats.pending) })}</span>
            </div>
            <div class="flex items-center gap-2 shrink-0">
              <button data-testid="review-nudge-start" class="btn btn-primary !text-xs" onclick={enterReviewMode}>
                {$t('reviewCorrect.start')} →
              </button>
              <button
                class="text-cortex-400 hover:text-cortex-200 text-sm leading-none px-1"
                aria-label={$t('reviewCorrect.dismiss')}
                title={$t('reviewCorrect.dismiss')}
                onclick={() => (reviewNudgeDismissed = true)}>✕</button
              >
            </div>
          </div>
        {/if}
        {#if viewMode === 'insights'}
          <StatsDashboard />
          <RefineryPanel />
        {:else if viewMode === 'review'}
          <ReviewMode onExport={handleExport} onDone={() => (viewMode = 'curate')} />
        {:else if $selectedSegment}
          <div class="card overflow-hidden">
            <Waveform
              waveform={waveformData}
              currentTime={chunkClipPosition}
              duration={chunkClipLength}
              wordTimestamps={$wordTimestamps}
              {onSeek}
            />
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
                      <svg class="animate-spin w-3 h-3" fill="none" viewBox="0 0 24 24"
                        ><circle
                          class="opacity-25"
                          cx="12"
                          cy="12"
                          r="10"
                          stroke="currentColor"
                          stroke-width="4"
                        /><path
                          class="opacity-75"
                          fill="currentColor"
                          d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
                        /></svg
                      >
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
                <button
                  data-testid="transcribe-constrained-btn"
                  class="btn btn-secondary !text-xs"
                  onclick={handleTranscribeConstrained}
                  disabled={$isProcessing}
                  title={$t('transcribeConstrainedTitle')}
                >
                  {$t('transcribeConstrained')}
                </button>
                <button
                  data-testid="transcribe-finetuned-btn"
                  class="btn btn-secondary !text-xs"
                  onclick={handleTranscribeFinetuned}
                  disabled={$isProcessing}
                  title={$t('transcribeFinetunedTitle')}
                >
                  {$t('transcribeFinetuned')}
                </button>
                {#if $settings.cloudSttOptIn}
                  <button
                    data-testid="transcribe-scribe-btn"
                    class="btn btn-secondary !text-xs"
                    onclick={handleTranscribeScribe}
                    disabled={$isProcessing}
                    title={$t('scribe.transcribeTitle')}
                  >
                    {$t('scribe.transcribe')}
                  </button>
                  <button
                    data-testid="add-scribe-vote-btn"
                    class="btn btn-secondary !text-xs"
                    onclick={handleAddScribeVote}
                    disabled={$isProcessing}
                    title={$t('scribe.voteTitle')}
                  >
                    {$t('scribe.vote')}
                  </button>
                {/if}
                <button class="btn btn-secondary !text-xs" onclick={handleNormalize}
                  >{$t('normalize')}</button
                >
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
                  {$t('editorTextEditor')}
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
                  <div class="flex flex-wrap gap-x-1.5 gap-y-2" dir="rtl" lang="ckb" bind:this={interactiveStripEl}>
                    {#each $wordTimestamps as w, idx}
                      <!-- Word times are CLIP-relative; compare/seek against the clip offset so an
                           offset chunk highlights + seeks correctly (not at the whole-file position). -->
                      {@const isActive =
                        currentTime - chunkStartTime >= w.start && currentTime - chunkStartTime <= w.end}
                      <!-- Single click / Enter / Space = play just this word (listen). Double-click /
                           F2 = edit it inline. Decoupled so a listen tap never opens an input under
                           the keyboard and silently rewrites the transcript on blur. -->
                      <span
                        data-chip={idx}
                        class="relative inline-block px-1.5 py-0.5 rounded cursor-pointer transition-all duration-150 group
                        {isActive
                          ? 'bg-cortex-700 text-default font-bold border-b border-yellow-400'
                          : 'text-cortex-200 hover:bg-cortex-800 hover:text-white'}"
                        onclick={() => playWordClip(w)}
                        ondblclick={() => playWordClip(w, idx)}
                        title="{w.word} ({w.start.toFixed(2)}s - {w.end.toFixed(2)}s) — {$t('review.wordChipHint')}"
                        role="button"
                        tabindex="0"
                        aria-keyshortcuts="Enter Space F2"
                        onkeydown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            playWordClip(w);
                            e.preventDefault();
                          } else if (e.key === 'F2') {
                            // Keyboard path into inline word-edit without playback.
                            editingWordIndex = idx;
                            e.preventDefault();
                          }
                        }}
                      >
                        {#if editingWordIndex === idx}
                          <!-- stopPropagation: the input lives INSIDE the clickable chip — without it
                               a caret click replays the word and Space/Enter bubble into the chip's
                               keydown (Space could never be typed into a correction). -->
                          <input
                            type="text"
                            dir="rtl"
                            lang="ckb"
                            class="bg-cortex-800 text-white text-xs px-1 border border-cortex-500 rounded outline-none focus:ring-1 focus:ring-cortex-400 w-16 text-start"
                            value={w.word}
                            aria-label={$t('review.editWordAria')}
                            onclick={(e) => e.stopPropagation()}
                            ondblclick={(e) => e.stopPropagation()}
                            onblur={(e) =>
                              finishEditingWord(idx, (e.target as HTMLInputElement).value)}
                            onkeydown={(e) => {
                              e.stopPropagation();
                              if (e.key === 'Enter') {
                                finishEditingWord(idx, (e.target as HTMLInputElement).value);
                                refocusChip(idx);
                              }
                              if (e.key === 'Escape') {
                                editingWordIndex = null;
                                refocusChip(idx);
                              }
                            }}
                            use:focusOnMount
                          />
                        {:else}
                          <span class="select-text">{w.word}</span>
                        {/if}
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
                placeholder={$t('editTranscript')}
                disabled={$isProcessing}
                oninput={(e) => {
                  const seg = $selectedSegment;
                  if (seg) {
                    const annotatedTranscript = (e.target as HTMLTextAreaElement).value;
                    segments.update((arr) =>
                      arr.map((s) => (s.id === seg.id ? { ...s, annotatedTranscript } : s)),
                    );
                    scheduleAutoSave({ annotatedTranscript });
                  }
                }}
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
                  disabled={$isProcessing}
                  oninput={(e) => {
                    const seg = $selectedSegment;
                    if (seg) {
                      const speakerId = (e.target as HTMLInputElement).value;
                      segments.update((arr) =>
                        arr.map((s) => (s.id === seg.id ? { ...s, speakerId } : s)),
                      );
                      // Persist like the annotation field, so the speaker edit isn't left only in the
                      // store (lost on reload) or silently piggybacked onto an unrelated later save.
                      scheduleAutoSave({ speakerId });
                    }
                  }}
                />
              </div>
              <button class="btn btn-secondary !text-xs shrink-0" onclick={handleSaveSpeaker}
                >{$t('speaker.save')}</button
              >
            </div>

            <DiffView
              raw={$selectedSegment.normalizedTranscript ?? $selectedSegment.rawTranscript ?? ''}
              annotated={$selectedSegment.annotatedTranscript ?? ''}
            />

            {#if $wordTimestamps.length > 0}
              <div class="space-y-1">
                <span class="text-[11px] text-cortex-400">{$t('wordTimestamps')}</span>
                <div class="flex flex-wrap gap-1 max-h-20 overflow-y-auto" role="group" aria-label={$t('wordTimestamps')} dir="rtl" lang="ckb">
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
                      aria-label={$t('review.playWordAria').replace('{word}', w.word)}>{w.word}</button
                    >
                  {/each}
                </div>
              </div>
            {/if}

            <div class="flex gap-2 pt-1">
              <button class="btn btn-primary !text-xs relative" onclick={handleSaveAnnotation}>
                {#if saveState === 'saving'}
                  <span class="flex items-center gap-1">
                    <svg class="animate-spin w-3 h-3" fill="none" viewBox="0 0 24 24"
                      ><circle
                        class="opacity-25"
                        cx="12"
                        cy="12"
                        r="10"
                        stroke="currentColor"
                        stroke-width="4"
                      /><path
                        class="opacity-75"
                        fill="currentColor"
                        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
                      /></svg
                    >
                    {$t('saving')}
                  </span>
                {:else if saveState === 'saved'}
                  <span class="text-success">✓ {$t('saved')}</span>
                {:else}
                  {$t('save')}
                {/if}
                {#if showHotkeyOverlay}
                  <span
                    class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
                    >^S</span
                  >
                {/if}
              </button>

              <button
                data-testid="verify-btn"
                class="btn btn-secondary !text-xs relative"
                onclick={handleToggleVerify}
                disabled={verifyInFlight || $isProcessing}
              >
                {$selectedSegment.verified ? $t('unverify') : $t('verify')}
                {#if showHotkeyOverlay}
                  <span
                    class="absolute -top-1.5 -right-1.5 bg-cyan-400 text-black text-[8px] font-mono font-bold px-1 rounded shadow-md border border-cyan-500 select-none z-50 pointer-events-none"
                    >^D</span
                  >
                {/if}
              </button>
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
                  data-testid="empty-start-review">{$t('reviewCorrect.start')} →</button
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
    {#if $filteredSegments.length > 0}
      <PanelSplitter
        direction="horizontal"
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
              <StatsDashboard />
              <HistoryPanel bind:this={historyPanel} {showHotkeyOverlay} />
            </div>
          {/if}
        </aside>
      </ErrorBoundary>
    {/if}
  </div>

  <!-- Status Bar -->
  <footer
    data-testid="status-bar"
    class="flex items-center justify-between px-4 py-1 glass border-t border-line shrink-0"
  >
    <div class="flex items-center gap-3 text-[10px] text-cortex-500">
      <span>{$statusMessage}</span>
      {#if $isProcessing}
        <span class="flex items-center gap-1">
          <span class="w-1.5 h-1.5 rounded-full bg-amber-400 animate-pulse"></span>
          {$t('processing')}
        </span>
      {/if}
      {#if $pipelinePhase === 'importing'}
        <span data-testid="pipeline-import-status" class="flex items-center gap-2 text-amber-400">
          <svg class="animate-spin w-3 h-3 shrink-0" fill="none" viewBox="0 0 24 24"
            ><circle
              class="opacity-25"
              cx="12"
              cy="12"
              r="10"
              stroke="currentColor"
              stroke-width="4"
            /><path
              class="opacity-75"
              fill="currentColor"
              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
            /></svg
          >
          <span class="flex flex-col gap-0.5 min-w-0">
            <span
              >{$t('pipeline.importing')}
              {$t('pipeline.filesProcessed', {
                current: String($filesProcessed),
                total: String($pipelineTotal || '?'),
              })}</span
            >
            {#if $pipelineCurrentFile}
              <span class="text-cortex-500 truncate max-w-xs" title={$pipelineCurrentFile}>
                {$t('pipeline.currentFile', {
                  file: $pipelineCurrentFile.split(/[/\\]/).pop() ?? $pipelineCurrentFile,
                })}
              </span>
            {/if}
            {#if $pipelineStatus}
              <span class="text-cortex-600">{$t('pipeline.phase', { phase: $pipelineStatus })}</span
              >
            {/if}
          </span>
        </span>
        <button
          class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded shrink-0"
          onclick={cancelOperation}>{$t('pipeline.cancel')}</button
        >
      {:else if $pipelinePhase === 'reference_transcribing'}
        <span
          data-testid="pipeline-reference-status"
          class="flex items-center gap-2 text-amber-400"
        >
          <svg class="animate-spin w-3 h-3 shrink-0" fill="none" viewBox="0 0 24 24"
            ><circle
              class="opacity-25"
              cx="12"
              cy="12"
              r="10"
              stroke="currentColor"
              stroke-width="4"
            /><path
              class="opacity-75"
              fill="currentColor"
              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
            /></svg
          >
          <span class="flex flex-col gap-0.5 min-w-0">
            <span>{$t('pipeline.referenceTranscribing')}</span>
            {#if $pipelineCurrentFile}
              <span class="text-cortex-500 truncate max-w-xs" title={$pipelineCurrentFile}>
                {$t('pipeline.currentFile', {
                  file: $pipelineCurrentFile.split(/[/\\]/).pop() ?? $pipelineCurrentFile,
                })}
              </span>
            {/if}
            {#if $pipelineStatus}
              <span class="text-cortex-600">{$t('pipeline.phase', { phase: $pipelineStatus })}</span
              >
            {/if}
          </span>
        </span>
        <button
          class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded shrink-0"
          onclick={cancelOperation}>{$t('pipeline.cancel')}</button
        >
      {:else if $pipelinePhase === 'detecting'}
        <span class="flex items-center gap-1 text-amber-400">
          <svg class="animate-spin w-3 h-3" fill="none" viewBox="0 0 24 24"
            ><circle
              class="opacity-25"
              cx="12"
              cy="12"
              r="10"
              stroke="currentColor"
              stroke-width="4"
            /><path
              class="opacity-75"
              fill="currentColor"
              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
            /></svg
          >
          {$t('pipeline.detecting')}
        </span>
      {:else if $pipelinePhase === 'transcribing'}
        <span class="flex items-center gap-1 text-amber-400">
          <svg class="animate-spin w-3 h-3" fill="none" viewBox="0 0 24 24"
            ><circle
              class="opacity-25"
              cx="12"
              cy="12"
              r="10"
              stroke="currentColor"
              stroke-width="4"
            /><path
              class="opacity-75"
              fill="currentColor"
              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
            /></svg
          >
          {$pipelineStatus || $t('pipeline.transcribing')}
          {$filesProcessed || $batchProgress.completed}/{$pipelineTotal ||
            $batchProgress.total ||
            '?'}
        </span>
        <!-- True-10 audit: the app's LONGEST operation had no cancel affordance while nearly every
             button is disabled during it — a mistaken batch locked the UI with no visible way out,
             even though the backend cancel token exists. Same cancel as the import branches. -->
        <button
          class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded shrink-0"
          data-testid="cancel-batch-transcribe-btn"
          onclick={cancelOperation}>{$t('pipeline.cancel')}</button
        >
      {:else if $pipelinePhase === 'adjudicating'}
        <span class="flex items-center gap-1 text-amber-400">
          <svg class="animate-spin w-3 h-3" fill="none" viewBox="0 0 24 24"
            ><circle
              class="opacity-25"
              cx="12"
              cy="12"
              r="10"
              stroke="currentColor"
              stroke-width="4"
            /><path
              class="opacity-75"
              fill="currentColor"
              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
            /></svg
          >
          {$t('pipeline.adjudicating')}
        </span>
      {/if}
      {#if $agentPipelineStages.length}
        <div
          class="hidden xl:flex items-center gap-1 max-w-[48rem] overflow-hidden"
          data-testid="agent-pipeline-timeline"
        >
          {#each $agentPipelineStages.slice(-5) as stage}
            <span
              class={`px-1.5 py-0.5 rounded border font-mono truncate max-w-[11rem] ${agentStageTone(stage.status)}`}
              title={`${compactStageLabel(stage.stage)}: ${stage.detail}`}
            >
              {compactStageLabel(stage.stage)}:{stage.status}
            </span>
          {/each}
        </div>
      {/if}
      {#if $batchProgress.status === 'running' && $pipelinePhase === 'idle'}
        <div class="flex items-center gap-2">
          <div class="w-20 h-1 bg-cortex-700 rounded-full overflow-hidden">
            <div
              class="h-full bg-cortex-400 rounded-full transition-all"
              style="width: {$batchProgress.percent}%"
            ></div>
          </div>
          <span
            >{$t('batchVerify.status', {
              current: String($batchProgress.completed),
              total: String($batchProgress.total),
            })}</span
          >
          <button
            class="text-red-400 hover:text-red-300 px-1.5 py-0.5 border border-red-500/30 rounded"
            onclick={cancelOperation}>{$t('pipeline.cancel')}</button
          >
        </div>
      {/if}
    </div>
    <div class="flex items-center gap-3 text-[10px] text-cortex-500">
      <span>{$segmentStats.total} {$t('segments')}</span>
      <span>{fmtDuration($segmentStats.totalDurationMs)} {$t('total')}</span>
      <span>{$segmentStats.verified}/{$segmentStats.total} {$t('verifiedCount')}</span>
      <span class="text-[10px] text-cortex-500">{$t('pressForShortcuts')}</span>
      <button
        class="hover:text-cortex-400 transition-colors"
        onclick={() => showKeyboardHelp.set(true)}
        title="{modKey}+/"
        aria-label={$t('keyboardShortcuts')}
      >
        <kbd class="text-[9px]">{modKey}+/</kbd>
        {$t('shortcuts')}
      </button>
    </div>
  </footer>
</div>

<!-- Modals -->
{#if $showSettings}
  <ErrorBoundary>
    <SettingsPanel />
  </ErrorBoundary>
{/if}

<KeyboardShortcuts />

<!-- reviewActive filters the palette to allowInReview commands: without it, Ctrl+K re-opened the
     exact hidden-selection hole the keyboard manager's review suppression closes (running e.g.
     "Toggle verified" from the palette verifies the INVISIBLE curate selection — unreviewed gold). -->
<CommandPalette
  open={showCommandPalette}
  reviewActive={viewMode === 'review' || $showReviewInbox}
  onClose={() => (showCommandPalette = false)}
/>

<ConfirmDialog />

{#if $showValidationPanel}
  <ErrorBoundary>
    <ValidationPanel />
  </ErrorBoundary>
{/if}

{#if $showReviewInbox}
  <!-- use:focusTrap (true-10 audit): the inbox was the ONLY overlay without one — Tab walked into
       the invisible background app and Enter activated top-bar buttons sight-unseen. -->
  <div class="fixed inset-0 z-[100] flex items-stretch justify-center p-6 glass" use:focusTrap>
    <ErrorBoundary>
      <!-- Reload on close: inbox decisions (accept/edit/reject/flag) write human_decision to the DB
           but only mutate the inbox's LOCAL copy — without this, header stats and the Review &
           Correct queue keep pre-decision data, and a reviewer could unknowingly overwrite a
           just-recorded reject with an accept. loadSegments routes through the loadSeq-guarded
           segments.load(), so the refresh is race-safe. -->
      <ReviewInbox onClose={() => { showReviewInbox.set(false); void loadSegments(); }} />
    </ErrorBoundary>
  </div>
{/if}

{#if $showSpeakerPanel}
  <ErrorBoundary>
    <SpeakerPanel />
  </ErrorBoundary>
{/if}

{#if $showDatasetMerge}
  <ErrorBoundary>
    <DatasetMerge />
  </ErrorBoundary>
{/if}

{#if $showWslConsole}
  <ErrorBoundary>
    <WslConsolePanel />
  </ErrorBoundary>
{/if}

<Toast />
