<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { get } from 'svelte/store';
  import * as api from './lib/commands';
  import { createAutosaveController, flushAutosaveForIds } from './lib/autosave';
  import { createSegmentMetadataCoordinator } from './lib/segmentMetadataCoordinator';
  import type { SpeechSegment } from './lib/types';
  import {
    segments,
    selectedSegmentId,
    selectedSegment,
    filteredSegments,
  } from './lib/stores/segmentStore';
  import { openSettings } from './lib/stores/settingsStore';
  import { isProcessing, showReviewInbox } from './lib/stores/uiStore';
  import { notifications } from './lib/stores/notificationStore';
  import type { HistoryRecorder } from './lib/historyAction';
  import { initKeyboardManager, modKeyLabel } from './lib/keyboard';
  const modKey = modKeyLabel();
  import { parseActionableError } from './lib/errors';
  import { createWorkstationBatchActions } from './lib/workstationBatchActions';
  import { createWorkstationDataController } from './lib/workstationDataController.svelte';
  import { createWorkstationExportActions } from './lib/workstationExportActions';
  import { createWorkstationHistoryActions } from './lib/workstationHistoryActions';
  import { createWorkstationPlaybackController } from './lib/workstationPlaybackController.svelte';
  import { createWorkstationRecoveryController } from './lib/workstationRecoveryController.svelte';
  import { createWorkstationRuntimeController } from './lib/workstationRuntimeController';
  import { createWorkstationSegmentActions } from './lib/workstationSegmentActions';
  import { createWorkstationSessionController } from './lib/workstationSessionController.svelte';
  import { registerWorkstationShortcuts } from './lib/workstationShortcuts';
  import { createWorkstationViewController } from './lib/workstationViewController.svelte';
  import { t } from './lib/i18n';
  import ActivityRail from './lib/ActivityRail.svelte';
  import LazyComponent from './lib/LazyComponent.svelte';
  import StatusBar from './lib/StatusBar.svelte';
  import WorkstationOverlays from './lib/WorkstationOverlays.svelte';
  import WorkstationHeader from './lib/WorkstationHeader.svelte';
  import WorkstationCenter from './lib/WorkstationCenter.svelte';
  import WorkstationLibraryPanel from './lib/WorkstationLibraryPanel.svelte';
  import WorkstationProgress from './lib/WorkstationProgress.svelte';
  import WorkstationStatsPanel from './lib/WorkstationStatsPanel.svelte';

  // Secondary workspaces are isolated chunks. These stable loader functions are intentionally
  // declared outside reactive work so a parent update cannot restart an in-flight import.
  const loadSettingsPanel = () => import('./lib/SettingsPanel.svelte');
  const loadStatsDashboard = () => import('./lib/StatsDashboard.svelte');
  const loadAgentReportPanel = () => import('./lib/AgentReportPanel.svelte');
  const loadRefineryPanel = () => import('./lib/RefineryPanel.svelte');
  const loadReviewMode = () => import('./lib/ReviewMode.svelte');
  const loadKeyboardShortcuts = () => import('./lib/KeyboardShortcuts.svelte');
  const loadValidationPanel = () => import('./lib/ValidationPanel.svelte');
  const loadReviewInbox = () => import('./lib/ReviewInbox.svelte');
  const loadSpeakerPanel = () => import('./lib/SpeakerPanel.svelte');
  const loadDatasetMerge = () => import('./lib/DatasetMerge.svelte');
  const loadWslConsolePanel = () => import('./lib/WslConsolePanel.svelte');
  const loadCommandPalette = () => import('./lib/CommandPalette.svelte');
  const loadRecoveryNotices = () => import('./lib/WorkstationRecoveryNotices.svelte');
  const lazyLabels = $derived({
    loadingLabel: $t('loading'),
    failedLabel: $t('workspace.loadFailed'),
    retryLabel: $t('retry'),
    closeLabel: $t('close'),
  });

  let historyPanel = $state<HistoryRecorder | null>(null);

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
    // The playback-selection effect synchronously calls the metadata coordinator. Keep this
    // notification's read-modify-write outside that effect's dependency graph; otherwise the
    // notification re-runs selection, which forgets/remembers the baseline and notifies forever.
    onReadinessChanged: () => untrack(() => (metadataReadinessEpoch += 1)),
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
  const data = createWorkstationDataController(() => tauriAvailable);
  const view = createWorkstationViewController(requireDesktopRuntime);
  const session = createWorkstationSessionController(() => tauriAvailable);
  const playback = createWorkstationPlaybackController({
    isTauriAvailable: () => tauriAvailable,
    forgetMetadata: metadataCoordinator.forget,
    rememberMetadata: metadataCoordinator.remember,
    pruneMetadata: metadataCoordinator.pruneExcept,
    retainedAutosaveIds: autosave.retainedIds,
    flushAutosave: autosave.flush,
  });
  const recovery = createWorkstationRecoveryController({
    requireDesktopRuntime,
    loadSegments: data.loadSegments,
    loadLatestAgentHistory: data.loadLatestAgentHistory,
    clearAgentEvidence: data.clearAgentEvidence,
    setSegmentsLoading: (loading) => (data.segmentsLoading = loading),
  });
  const { batchCoordinator, importCoordinator, importRecovery } = recovery;

  function requireDesktopRuntime(): boolean {
    if (tauriAvailable) return true;
    notifications.info($t('desktopRuntimeRequired'));
    return false;
  }

  function scheduleAutoSave(edits: api.SegmentMetadataFields) {
    autosave.schedule(edits);
  }

  function navigateSegment(direction: 'up' | 'down') {
    const list = $filteredSegments;
    if (list.length === 0) return;
    const currentId = $selectedSegmentId;
    const currentIndex = list.findIndex((s) => s.id === currentId);
    const startIdx = currentIndex < 0 ? (direction === 'down' ? -1 : list.length) : currentIndex;
    const targetIndex =
      direction === 'down' ? Math.min(list.length - 1, startIdx + 1) : Math.max(0, startIdx - 1);
    playback.selectSegment(list[targetIndex]);
  }

  function notifyActionableError(error: unknown, fallbackMessage: string) {
    const parsed = parseActionableError(error, fallbackMessage);
    notifications.error(parsed.message, {
      cause: error,
      publicDetail: parsed.detail,
      action: parsed.action,
    });
  }

  const handleOpenFile = () => importCoordinator.openFile();
  const handleImport = () => importCoordinator.importDirectory();

  const exportActions = createWorkstationExportActions({
    requireDesktopRuntime,
    getPromotionStage: () => data.datasetPromotionStage,
    isTrainingExportBlocked: () => data.trainingExportBlocked,
    trainingExportBlockDetail: data.trainingExportBlockDetail,
  });
  const batchActions = createWorkstationBatchActions({
    requireDesktopRuntime,
    batchCoordinator,
    getBatchStarting: () => recovery.batchStarting,
    setBatchStarting: (starting) => (recovery.batchStarting = starting),
    getBatchSpeakerId: () => view.batchSpeakerId,
    loadSegments: data.loadSegments,
    flushAutosave: (ids) => flushAutosaveForIds(autosave, ids),
  });
  const historyActions = createWorkstationHistoryActions({
    requireDesktopRuntime,
    getViewMode: () => view.viewMode,
    getHistoryPanel: () => historyPanel,
    loadSegments: data.loadSegments,
  });
  const segmentActions = createWorkstationSegmentActions({
    requireDesktopRuntime,
    loadSegments: data.loadSegments,
    notifyActionableError,
    pendingAutosaveId: autosave.pendingId,
    flushAutosave: autosave.flushAsync,
    flushAutosaveIds: (ids) => flushAutosaveForIds(autosave, ids),
    saveMetadata: metadataCoordinator.saveFields,
    getHistoryPanel: () => historyPanel,
  });

  const handleTranscribe = segmentActions.transcribe;
  const handleUndo = historyActions.undo;
  const handleRedo = historyActions.redo;
  const handleDeleteWithConfirm = segmentActions.deleteWithConfirm;
  const handleSaveSpeaker = segmentActions.saveSpeaker;
  const handleExport = exportActions.exportDataset;
  const handleExportTranscript = exportActions.exportTranscript;
  const handleExportHuggingface = exportActions.exportHuggingface;
  const handleExportAudio = exportActions.exportAudio;
  const handleBatchTranscribe = batchActions.transcribe;
  const handleBatchAssignSpeaker = batchActions.assignSpeaker;
  const handleBatchNormalize = batchActions.normalize;
  const handleRediarize = batchActions.rediarize;
  const handleDeleteFilteredWithConfirm = batchActions.deleteFilteredWithConfirm;
  const handleAlign = segmentActions.align;

  function registerShortcuts(keyboard: ReturnType<typeof initKeyboardManager>): void {
    registerWorkstationShortcuts(keyboard, {
      openFile: handleOpenFile,
      importDirectory: handleImport,
      transcribe: handleTranscribe,
      enterReview: view.enterReviewMode,
      undo: handleUndo,
      redo: handleRedo,
      deleteSegment: handleDeleteWithConfirm,
      validate: view.openValidationPanel,
      openReviewInbox: view.openReviewInbox,
      toggleSidebar: () => (view.sidebarOpen = !view.sidebarOpen),
      toggleStats: () => (view.statsOpen = !view.statsOpen),
      navigate: navigateSegment,
      togglePlayback: () => (playback.isAudioPlaying = !playback.isAudioPlaying),
      rewind: () => {
        playback.clearWordOverride();
        playback.currentTime = Math.max(0, playback.currentTime - 5);
      },
      forward: () => {
        playback.clearWordOverride();
        playback.currentTime = Math.min(playback.playerDuration, playback.currentTime + 5);
      },
      openCommandPalette: () => (view.showCommandPalette = true),
    });
  }

  const runtime = createWorkstationRuntimeController({
    importCoordinator,
    batchCoordinator,
    registerShortcuts,
    getViewMode: () => view.viewMode,
    setTauriAvailable: (available) => (tauriAvailable = available),
    setSegmentsLoading: (loading) => (data.segmentsLoading = loading),
    setQuarantineNotice: (notice) => {
      recovery.quarantineNotice = notice;
    },
    loadSegments: data.loadSegments,
    loadLatestAgentHistory: data.loadLatestAgentHistory,
    loadSettings: data.loadSettings,
    restoreAndApplySession: session.restoreAndApply,
    reconcileImportRecovery: importRecovery.reconcile,
    flushAutosave: autosave.flush,
    flushAutosaveAsync: autosave.flushAsync,
    clearSessionTimer: session.clearTimer,
  });
  onMount(() => void runtime.mount());
  onDestroy(runtime.destroy);
</script>

<div
  class="h-screen flex flex-col bg-app text-default"
  data-testid="app-root"
  inert={$showReviewInbox}
  aria-hidden={$showReviewInbox ? 'true' : undefined}
>
  {#if recovery.quarantineNotice || recovery.interruptedImport || recovery.importRecoveryAuthority !== 'known'}
    <div
      class="max-h-[min(42dvh,14rem)] shrink-0 overflow-y-auto overscroll-contain"
      data-testid="recovery-notice-region"
    >
      <LazyComponent
        load={loadRecoveryNotices}
        componentProps={{
          quarantineNotice: recovery.quarantineNotice,
          interruptedImport: recovery.interruptedImport,
          importRecoveryBusy: recovery.importRecoveryBusy,
          importRecoveryAuthority: recovery.importRecoveryAuthority,
          workspaceOperationBusy: $isProcessing || recovery.importStarting,
          onAcknowledgeQuarantine: () => void recovery.acknowledgeQuarantine(),
          onDismissQuarantine: () => (recovery.quarantineNotice = null),
          onResumeImport: () => void importRecovery.resume(),
          onDismissImport: () => void importRecovery.discard(),
          onRetryRecoveryCheck: () => void importRecovery.reconcile(),
        }}
        {...lazyLabels}
      />
    </div>
  {/if}
  <WorkstationHeader
    {tauriAvailable}
    bind:sidebarOpen={view.sidebarOpen}
    bind:statsOpen={view.statsOpen}
    showHotkeyOverlay={view.showHotkeyOverlay}
    trainingExportBlocked={data.trainingExportBlocked}
    trainingExportTitle={data.trainingExportTitle}
    {modKey}
    onSelectWorkspace={view.selectWorkspace}
    onOpenCommandPalette={() => (view.showCommandPalette = true)}
    onOpenFile={() => void handleOpenFile()}
    onImport={() => void handleImport()}
    onExport={() => void handleExport()}
    onExportTranscript={() => void handleExportTranscript()}
    onExportHuggingface={() => void handleExportHuggingface()}
    onExportAudio={() => void handleExportAudio()}
    onOpenWsl={view.openWslConsole}
    onEnterReview={view.enterReviewMode}
    onValidate={view.openValidationPanel}
    onOpenInbox={view.openReviewInbox}
    onOpenSettings={() => openSettings()}
  />

  <WorkstationProgress />

  <div class="flex flex-1 overflow-hidden">
    <ActivityRail view={view.viewMode} onSelect={view.selectWorkspace} />
    <WorkstationLibraryPanel
      bind:sidebarOpen={view.sidebarOpen}
      bind:sidebarWidth={view.sidebarWidth}
      bind:batchSpeakerId={view.batchSpeakerId}
      {tauriAvailable}
      segmentsLoading={data.segmentsLoading}
      showHotkeyOverlay={view.showHotkeyOverlay}
      onBatchTranscribe={handleBatchTranscribe}
      onBatchAssignSpeaker={handleBatchAssignSpeaker}
      onBatchNormalize={handleBatchNormalize}
      onRediarize={handleRediarize}
      onOpenSpeaker={view.openSpeakerPanel}
      onOpenDatasetMerge={view.openDatasetMerge}
      onDeleteFiltered={handleDeleteFilteredWithConfirm}
      onSelectSegment={playback.selectSegment}
      onLoadSegments={data.loadSegments}
      onImport={handleImport}
      onOpenFile={handleOpenFile}
    />

    <!-- Center: Transcription Work Area -->
    <WorkstationCenter
      viewMode={view.viewMode}
      bind:reviewNudgeDismissed={view.reviewNudgeDismissed}
      bind:editorTab={view.editorTab}
      bind:currentTime={playback.currentTime}
      bind:playerDuration={playback.playerDuration}
      bind:isAudioPlaying={playback.isAudioPlaying}
      waveformData={playback.waveformData}
      waveformError={playback.waveformError}
      chunkClipPosition={playback.chunkClipPosition}
      chunkClipLength={playback.chunkClipLength}
      chunkStartTime={playback.chunkStartTime}
      chunkEndTime={playback.chunkEndTime}
      chunkLabel={playback.chunkLabel}
      wordStartOverride={playback.wordStartOverride}
      wordEndOverride={playback.wordEndOverride}
      {selectedMetadataReady}
      showHotkeyOverlay={view.showHotkeyOverlay}
      {modKey}
      {lazyLabels}
      {loadStatsDashboard}
      {loadRefineryPanel}
      {loadReviewMode}
      onEnterReview={view.enterReviewMode}
      onLeaveReview={view.leaveReviewMode}
      onExport={handleExport}
      onRetryWaveform={() =>
        $selectedSegment &&
        playback.loadWaveform($selectedSegment.audioPath, $selectedSegment.alignmentJson)}
      onSeek={playback.seek}
      onTranscribe={handleTranscribe}
      onPlayWord={playback.playWordClip}
      onScheduleAutoSave={scheduleAutoSave}
      onSaveSpeaker={handleSaveSpeaker}
      onAlign={handleAlign}
      onDelete={handleDeleteWithConfirm}
      onOpenReviewInbox={view.openReviewInbox}
    />

    {#if $filteredSegments.length > 0 && view.viewMode !== 'insights'}
      <WorkstationStatsPanel
        statsOpen={view.statsOpen}
        bind:statsWidth={view.statsWidth}
        showHotkeyOverlay={view.showHotkeyOverlay}
        latestAgentReport={data.latestAgentReport}
        latestAgentStageEvents={data.latestAgentStageEvents}
        bind:historyPanel
        {lazyLabels}
        {loadAgentReportPanel}
        {loadStatsDashboard}
      />
    {/if}
  </div>

  <StatusBar />
</div>

<WorkstationOverlays
  bind:showCommandPalette={view.showCommandPalette}
  reviewActive={view.viewMode === 'review' || $showReviewInbox}
  {...lazyLabels}
  loadSegments={data.loadSegments}
  {loadSettingsPanel}
  {loadKeyboardShortcuts}
  {loadCommandPalette}
  {loadValidationPanel}
  {loadReviewInbox}
  {loadSpeakerPanel}
  {loadDatasetMerge}
  {loadWslConsolePanel}
/>
