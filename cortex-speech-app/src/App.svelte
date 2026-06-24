<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import * as api from './lib/commands';
  import type {
    AgenticReadiness,
    AgentImportReport,
    AgentOrchestrationStage,
    AgentStageEvent,
  } from './lib/commands';
  import type { SpeechSegment } from './lib/types';
  import {
    segments,
    selectedSegmentId,
    wordTimestamps,
    searchQuery,
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
  import { historyStore } from './lib/stores/historyStore';
  import { initKeyboardManager, globalKeyboardManager } from './lib/keyboard';
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
  import { PARQUET_EXPORT_SUPPORTED } from './lib/appFeatures';
  import { isTauriRuntime } from './lib/runtime';
  import AudioPlayer from './lib/AudioPlayer.svelte';
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
  let saveTimeout: ReturnType<typeof setTimeout> | null = null;
  let tauriAvailable = $state(false);
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

  function scheduleAutoSave() {
    saveState = 'saving';
    if (saveTimeout) clearTimeout(saveTimeout);
    // Capture only the target segment ID now (so selecting a different segment within 1s still saves
    // the RIGHT one), then re-read the FRESH segment from the store at fire time. Persisting a
    // whole-segment snapshot captured now would clobber any field — verified / speakerId /
    // normalizedTranscript — that a concurrent verify/normalize/speaker action, or a background
    // reload (WSL/import completion), changed during the 1s debounce, because update_segment writes
    // the entire row.
    const id = $selectedSegment?.id;
    if (!id) {
      saveState = 'idle';
      return;
    }
    saveTimeout = setTimeout(async () => {
      const fresh = $segments.find((s) => s.id === id);
      if (!fresh) {
        saveState = 'idle';
        return;
      }
      try {
        await api.updateSegment(fresh);
        saveState = 'saved';
        setTimeout(() => {
          if (saveState === 'saved') saveState = 'idle';
        }, 2000);
      } catch (e) {
        saveState = 'idle';
        notifications.error($t('notifications.saveFailed'), { detail: String(e) });
      }
    }, 1000);
  }

  function finishEditingWord(index: number, newValue: string) {
    if (!newValue.trim()) {
      editingWordIndex = null;
      return;
    }
    const updatedWords = [...$wordTimestamps];
    updatedWords[index] = { ...updatedWords[index], word: newValue.trim() };
    wordTimestamps.set(updatedWords);

    const seg = $selectedSegment;
    if (seg) {
      const alignmentJson = mergeWordTimestamps(seg.alignmentJson, updatedWords);
      const annotatedTranscript = updatedWords.map((w) => w.word).join(' ');
      // Update store so auto-save picks it up
      segments.update((arr) =>
        arr.map((s) =>
          s.id === seg.id
            ? {
                ...s,
                alignmentJson,
                annotatedTranscript,
              }
            : s,
        ),
      );
      scheduleAutoSave();
    }
    editingWordIndex = null;
  }

  let chunkStartTime = $derived.by(() => {
    const meta = parseSourceMeta($selectedSegment?.alignmentJson);
    return chunkPlaybackRange(meta).startTime;
  });

  let chunkEndTime = $derived.by(() => {
    const meta = parseSourceMeta($selectedSegment?.alignmentJson);
    return chunkPlaybackRange(meta).endTime;
  });

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

  onMount(async () => {
    tauriAvailable = isTauriRuntime();
    const km = initKeyboardManager();
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
    } else {
      segments.set([]);
      segmentsLoading = false;
      statusMessage.set($t('ready'));
    }
  });

  onDestroy(() => {
    stopEventListeners();
    globalKeyboardManager?.destroy();
    if (saveTimeout) clearTimeout(saveTimeout);
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
        key: 's',
        ctrl: true,
        description: 'Save annotation',
        action: handleSaveAnnotation,
        category: 'edit',
      },
      { key: 'z', ctrl: true, description: 'Undo', action: () => handleUndo(), category: 'edit' },
      {
        key: 'z',
        ctrl: true,
        shift: true,
        description: 'Redo',
        action: () => handleRedo(),
        category: 'edit',
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
      },
      {
        key: ',',
        ctrl: true,
        description: 'Open settings',
        action: () => openSettings(),
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
        action: () => (currentTime = Math.max(0, currentTime - 5)),
        category: 'playback',
      },
      {
        key: 'ArrowRight',
        description: 'Forward 5s',
        action: () => (currentTime = Math.min(playerDuration, currentTime + 5)),
        category: 'playback',
      },
      {
        key: 'k',
        ctrl: true,
        description: 'Command palette',
        action: () => (showCommandPalette = true),
        category: 'general',
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
      if (readiness.status === 'blocked') {
        notifications.warning($t('agenticReadiness.blocked'), {
          detail: agenticReadinessDetail(readiness),
        });
      } else if (readiness.status === 'degraded') {
        notifications.warning($t('agenticReadiness.degraded'), {
          detail: agenticReadinessDetail(readiness),
        });
      }
    } catch (e) {
      notifications.warning($t('agenticReadiness.checkFailed'), { detail: String(e) });
    }
  }

  async function handleOpenFile() {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
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
    }
  }

  async function handleImport() {
    if ($isProcessing) return;
    if (!requireDesktopRuntime()) return;
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
      notifyActionableError(e, $t('importFailed'));
      statusMessage.set($t('importFailed'));
      isProcessing.set(false);
      pipelinePhase.set('idle');
      pipelineCurrentFile.set('');
      pipelineStatus.set('');
      pipelineTotal.set(0);
      filesProcessed.set(0);
      endOperation('import');
    }
  }

  async function handleTranscribe() {
    const seg = $selectedSegment;
    if (!seg || $isProcessing) return;
    if (!requireDesktopRuntime()) return;
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
        normalizedTranscript = await api.normalizeText(result.text);
      }
      let alignmentJson = seg.alignmentJson;
      if ($settings.autoAlign) {
        const alignText = normalizedTranscript ?? result.text;
        if (alignText?.trim()) {
          const ts = await api.alignSegment(seg.audioPath, alignText, seg.alignmentJson);
          wordTimestamps.set(ts);
          alignmentJson = mergeWordTimestamps(seg.alignmentJson, ts);
        }
      }
      const updatedSeg = {
        ...seg,
        rawTranscript,
        annotatedTranscript,
        normalizedTranscript,
        alignmentJson,
      };
      await api.updateSegment(updatedSeg);
      await loadSegments();
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

  async function handleNormalize() {
    const seg = $selectedSegment;
    if (!seg?.rawTranscript) return;
    if (!requireDesktopRuntime()) return;
    try {
      const normalizedTranscript = await api.normalizeText(seg.rawTranscript);
      const updatedSeg = { ...seg, normalizedTranscript };
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
      await api.updateSegment(seg);
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
    try {
      const description = await historyStore.undo();
      notifications.info(`Undo: ${description ?? 'Last action reverted'}`);
      await loadSegments();
      if (historyPanel) {
        historyPanel.recordAction(`Reverted: ${description ?? 'action'}`, 'edit');
      }
    } catch (e) {
      notifications.error(`Undo failed: ${e}`);
    }
  }

  async function handleRedo() {
    if (!requireDesktopRuntime()) return;
    try {
      const description = await historyStore.redo();
      notifications.info(`Redo: ${description ?? 'Last action reapplied'}`);
      await loadSegments();
      if (historyPanel) {
        historyPanel.recordAction(`Redone: ${description ?? 'action'}`, 'edit');
      }
    } catch (e) {
      notifications.error(`Redo failed: ${e}`);
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
      const updatedSeg = { ...seg, verified: nextVerified };
      await api.updateSegment(updatedSeg);
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
      await api.updateSegment(seg);
      notifications.success($t('speaker.saved'));
    } catch (e) {
      notifications.error($t('notifications.saveFailed'), { detail: String(e) });
    }
  }

  async function handleExport() {
    if (!requireDesktopRuntime()) return;
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      let format = $settings.exportFormat;
      if (format === 'parquet' && !PARQUET_EXPORT_SUPPORTED) {
        format = 'json';
      }
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
      if (PARQUET_EXPORT_SUPPORTED) {
        filters.push({ name: 'Parquet', extensions: ['parquet'] });
      }
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
    const verifiedIds = $segments.filter((s) => s.verified).map((s) => s.id);
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
        ? $segments.filter((s) => !s.verified).map((s) => s.id)
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
      const ts = await api.alignSegment(seg.audioPath, text, seg.alignmentJson);
      const alignmentJson = mergeWordTimestamps(seg.alignmentJson, ts);
      const updatedSeg = { ...seg, alignmentJson };
      await api.updateSegment(updatedSeg);
      // Update UI/store only AFTER the persist succeeds. Mutating first left an unsaved alignment in
      // the UI on a failed save, which a later auto-save would then silently persist.
      wordTimestamps.set(ts);
      segments.update((arr) => arr.map((s) => (s.id === seg.id ? { ...s, alignmentJson } : s)));
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
    selectedSegmentId.set(seg.id);
    wordTimestamps.set(parseWordTimestamps(seg.alignmentJson));
    currentTime = chunkPlaybackRange(parseSourceMeta(seg.alignmentJson)).startTime;
    loadWaveform(seg.audioPath, seg.alignmentJson);
  }

  function onSeek(time: number) {
    currentTime = time;
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
        title="⌘,"
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
        {$locale === 'ckb' ? 'EN' : 'ckb'}
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
        {#if viewMode === 'insights'}
          <StatsDashboard />
          <RefineryPanel />
        {:else if viewMode === 'review'}
          <ReviewMode />
        {:else if $selectedSegment}
          <div class="card overflow-hidden">
            <Waveform
              waveform={waveformData}
              {currentTime}
              duration={playerDuration}
              wordTimestamps={$wordTimestamps}
              {onSeek}
            />
          </div>

          <ErrorBoundary>
            <AudioPlayer
              audioPath={$selectedSegment.audioPath}
              startTime={chunkStartTime}
              endTime={chunkEndTime}
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
                  class="input h-28 resize-none font-mono text-xs text-right"
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
                  class="input h-28 resize-none font-mono text-xs text-right"
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
                  <div class="flex flex-wrap gap-x-1.5 gap-y-2" dir="rtl" lang="ckb">
                    {#each $wordTimestamps as w, idx}
                      {@const isActive = currentTime >= w.start && currentTime <= w.end}
                      <span
                        class="relative inline-block px-1.5 py-0.5 rounded cursor-pointer transition-all duration-150 group
                        {isActive
                          ? 'bg-cortex-700 text-default font-bold border-b border-yellow-400'
                          : 'text-cortex-200 hover:bg-cortex-800 hover:text-white'}"
                        onclick={() => (currentTime = w.start)}
                        ondblclick={() => (editingWordIndex = idx)}
                        title="{w.word} ({w.start.toFixed(2)}s - {w.end.toFixed(2)}s)"
                        role="button"
                        tabindex="0"
                        onkeydown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            currentTime = w.start;
                            e.preventDefault();
                          } else if (e.key === 'F2') {
                            // Keyboard path into inline word-edit (double-click is mouse-only).
                            editingWordIndex = idx;
                            e.preventDefault();
                          }
                        }}
                      >
                        {#if editingWordIndex === idx}
                          <input
                            type="text"
                            dir="rtl"
                            lang="ckb"
                            class="bg-cortex-800 text-white text-xs px-1 border border-cortex-500 rounded outline-none focus:ring-1 focus:ring-cortex-400 w-16 text-right"
                            value={w.word}
                            onblur={(e) =>
                              finishEditingWord(idx, (e.target as HTMLInputElement).value)}
                            onkeydown={(e) => {
                              if (e.key === 'Enter')
                                finishEditingWord(idx, (e.target as HTMLInputElement).value);
                              if (e.key === 'Escape') editingWordIndex = null;
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
                    No word timestamps available. Click 'Align' to align the text with audio.
                  </p>
                {/if}
              </div>
            {:else}
              <textarea
                dir="rtl"
                lang="ckb"
                class="input h-32 resize-none font-mono text-sm text-right"
                value={$selectedSegment.annotatedTranscript ?? ''}
                placeholder={$t('editTranscript')}
                oninput={(e) => {
                  const seg = $selectedSegment;
                  if (seg) {
                    segments.update((arr) =>
                      arr.map((s) =>
                        s.id === seg.id
                          ? { ...s, annotatedTranscript: (e.target as HTMLTextAreaElement).value }
                          : s,
                      ),
                    );
                    scheduleAutoSave();
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
                  oninput={(e) => {
                    const seg = $selectedSegment;
                    if (seg) {
                      const speakerId = (e.target as HTMLInputElement).value;
                      segments.update((arr) =>
                        arr.map((s) => (s.id === seg.id ? { ...s, speakerId } : s)),
                      );
                      // Persist like the annotation field, so the speaker edit isn't left only in the
                      // store (lost on reload) or silently piggybacked onto an unrelated later save.
                      scheduleAutoSave();
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
                      onclick={() => (currentTime = w.start)}
                      onkeydown={(e) => {
                        if (e.key === 'Enter' || e.key === ' ') {
                          currentTime = w.start;
                          e.preventDefault();
                        }
                      }}
                      aria-label="Jump to {w.word} at {w.start.toFixed(2)}s">{w.word}</button
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
            <div class="flex flex-wrap justify-center gap-x-3 gap-y-1 text-xs text-subtle">
              <span><kbd>⌘O</kbd> {$t('openFile')}</span>
              <span><kbd>⌘I</kbd> {$t('import')}</span>
              <span><kbd>⌘T</kbd> {$t('transcribe')}</span>
              <span><kbd>⌘K</kbd> {$t('shortcuts')}</span>
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
        title="⌘/"
        aria-label={$t('keyboardShortcuts')}
      >
        <kbd class="text-[9px]">⌘/</kbd>
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

<CommandPalette open={showCommandPalette} onClose={() => (showCommandPalette = false)} />

<ConfirmDialog />

{#if $showValidationPanel}
  <ErrorBoundary>
    <ValidationPanel />
  </ErrorBoundary>
{/if}

{#if $showReviewInbox}
  <div class="fixed inset-0 z-[100] flex items-stretch justify-center p-6 glass">
    <ErrorBoundary>
      <ReviewInbox onClose={() => showReviewInbox.set(false)} />
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
