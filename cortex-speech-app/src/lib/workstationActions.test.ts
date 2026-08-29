import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { activeOperations } from './invoke';
import { locale } from './i18n';
import { createWorkstationBatchActions } from './workstationBatchActions';
import { createWorkstationExportActions } from './workstationExportActions';
import { createWorkstationHistoryActions } from './workstationHistoryActions';
import { createWorkstationSegmentActions } from './workstationSegmentActions';
import { historyStore } from './stores/historyStore';
import { notifications } from './stores/notificationStore';
import {
  filterVerified,
  searchQuery,
  segmentStats,
  segments,
  selectedSegmentId,
  wordTimestamps,
} from './stores/segmentStore';
import { defaultSettings, settings } from './stores/settingsStore';
import {
  batchProgress,
  isProcessing,
  pipelinePhase,
  showConfirmDialog,
  showReviewInbox,
  statusMessage,
} from './stores/uiStore';
import type { SpeechSegment, WordTimestamp } from './types';

const commandMocks = vi.hoisted(() => ({
  alignSegment: vi.fn(),
  assignSpeakersV1: vi.fn(),
  deleteSegment: vi.fn(),
  deleteSegmentsBatch: vi.fn(),
  exportAudio: vi.fn(),
  exportDataset: vi.fn(),
  exportHuggingfaceDataset: vi.fn(),
  exportTranscript: vi.fn(),
  getSegmentIdsForView: vi.fn(),
  is7bUnavailableError: vi.fn(),
  rediarizeSegments: vi.fn(),
  transcribeSegment: vi.fn(),
  getHistoryStatusV1: vi.fn(),
  redo: vi.fn(),
  undo: vi.fn(),
}));

const dialogMocks = vi.hoisted(() => ({
  chooseDirectory: vi.fn(),
  saveFile: vi.fn(),
}));

vi.mock('./commands', () => ({
  AudioExportFormat: { Wav: 'Wav' },
  alignSegment: commandMocks.alignSegment,
  assignSpeakersV1: commandMocks.assignSpeakersV1,
  deleteSegment: commandMocks.deleteSegment,
  deleteSegmentsBatch: commandMocks.deleteSegmentsBatch,
  exportAudio: commandMocks.exportAudio,
  exportDataset: commandMocks.exportDataset,
  exportHuggingfaceDataset: commandMocks.exportHuggingfaceDataset,
  exportTranscript: commandMocks.exportTranscript,
  getHistoryStatusV1: commandMocks.getHistoryStatusV1,
  getSegmentIdsForView: commandMocks.getSegmentIdsForView,
  is7bUnavailableError: commandMocks.is7bUnavailableError,
  rediarizeSegments: commandMocks.rediarizeSegments,
  redo: commandMocks.redo,
  transcribeSegment: commandMocks.transcribeSegment,
  undo: commandMocks.undo,
}));

vi.mock('./fileDialogs', () => ({
  chooseDirectory: dialogMocks.chooseDirectory,
  saveFile: dialogMocks.saveFile,
}));

function segment(overrides: Partial<SpeechSegment> = {}): SpeechSegment {
  return {
    id: 'segment-1',
    audioPath: 'C:\\audio\\sample.wav',
    rawTranscript: 'real transcript',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 1_000,
    speakerId: null,
    verified: false,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function resetWorkstationStores(): void {
  segments.set([]);
  selectedSegmentId.set(null);
  wordTimestamps.set([]);
  filterVerified.set(null);
  searchQuery.set('');
  segmentStats.set({ total: 0, verified: 0, pending: 0, withAnnotations: 0, totalDurationMs: 0 });
  isProcessing.set(false);
  pipelinePhase.set('idle');
  batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
  statusMessage.set('Ready');
  showConfirmDialog.set(null);
  showReviewInbox.set(false);
  activeOperations.set(new Set());
  settings.set({ ...defaultSettings });
}

beforeEach(() => {
  locale.set('en');
  resetWorkstationStores();
  for (const mock of Object.values(commandMocks)) mock.mockReset();
  dialogMocks.chooseDirectory.mockReset();
  dialogMocks.saveFile.mockReset();
});

afterEach(() => {
  vi.restoreAllMocks();
  resetWorkstationStores();
  locale.set('ckb');
});

describe('workstation batch write authority', () => {
  type BatchDependencies = Parameters<typeof createWorkstationBatchActions>[0];

  function harness(overrides: Partial<BatchDependencies> = {}) {
    let starting = false;
    const startingChanges: boolean[] = [];
    const batchCoordinator = {
      startTranscription: vi.fn(),
      startNormalization: vi.fn(),
    } as unknown as BatchDependencies['batchCoordinator'];
    const loadSegments = vi.fn(async () => {});
    const flushAutosave = vi.fn(async () => true);
    const actions = createWorkstationBatchActions({
      requireDesktopRuntime: () => true,
      batchCoordinator,
      getBatchStarting: () => starting,
      setBatchStarting: (value) => {
        starting = value;
        startingChanges.push(value);
      },
      getBatchSpeakerId: () => 'speaker-a',
      loadSegments,
      flushAutosave,
      ...overrides,
    });
    return { actions, batchCoordinator, flushAutosave, loadSegments, startingChanges };
  }

  it('keeps selection intact until a durable batch deletion succeeds', async () => {
    segments.set([segment({ id: 'one' }), segment({ id: 'two' })]);
    selectedSegmentId.set('one');
    const timestamps: WordTimestamp[] = [{ word: 'one', start: 0, end: 1, confidence: 1 }];
    wordTimestamps.set(timestamps);
    commandMocks.getSegmentIdsForView.mockResolvedValue(['one', 'two']);
    const deletion = deferred<void>();
    commandMocks.deleteSegmentsBatch.mockReturnValue(deletion.promise);
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const { actions, loadSegments } = harness();

    await actions.deleteFilteredWithConfirm();
    const confirmation = get(showConfirmDialog);
    expect(confirmation?.message).toContain('2');

    const commit = confirmation?.onConfirm();
    await vi.waitFor(() =>
      expect(commandMocks.deleteSegmentsBatch).toHaveBeenCalledWith(['one', 'two']),
    );
    expect(get(selectedSegmentId)).toBe('one');
    expect(get(wordTimestamps)).toEqual(timestamps);
    expect(get(isProcessing)).toBe(true);
    expect(get(activeOperations)).toContain('batch-delete');

    deletion.resolve();
    await commit;

    expect(get(selectedSegmentId)).toBeNull();
    expect(get(wordTimestamps)).toEqual([]);
    expect(loadSegments).toHaveBeenCalledOnce();
    expect(success).toHaveBeenCalledOnce();
    expect(get(isProcessing)).toBe(false);
    expect(get(activeOperations)).toEqual(new Set());
  });

  it('refuses destructive batch deletion when pending human edits cannot flush', async () => {
    segments.set([segment({ id: 'one' })]);
    selectedSegmentId.set('one');
    const timestamps: WordTimestamp[] = [{ word: 'one', start: 0, end: 1, confidence: 1 }];
    wordTimestamps.set(timestamps);
    commandMocks.getSegmentIdsForView.mockResolvedValue(['one']);
    const { actions, flushAutosave, loadSegments } = harness();
    flushAutosave.mockResolvedValue(false);

    await actions.deleteFilteredWithConfirm();
    await get(showConfirmDialog)?.onConfirm();

    expect(flushAutosave).toHaveBeenCalledWith(['one']);
    expect(commandMocks.deleteSegmentsBatch).not.toHaveBeenCalled();
    expect(get(selectedSegmentId)).toBe('one');
    expect(get(wordTimestamps)).toEqual(timestamps);
    expect(loadSegments).not.toHaveBeenCalled();
  });

  it('rechecks shared busy authority after an asynchronous view-ID read', async () => {
    const ids = deferred<string[]>();
    commandMocks.getSegmentIdsForView.mockReturnValue(ids.promise);
    const { actions, loadSegments } = harness();

    const assignment = actions.assignSpeaker();
    isProcessing.set(true);
    ids.resolve(['one']);
    await assignment;

    expect(commandMocks.assignSpeakersV1).not.toHaveBeenCalled();
    expect(loadSegments).not.toHaveBeenCalled();
  });

  it('uses the selected durable ID and always releases the batch-start lease after failure', async () => {
    selectedSegmentId.set('selected-id');
    const failure = new Error('worker refused');
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    const { actions, batchCoordinator, startingChanges } = harness();
    vi.mocked(batchCoordinator.startTranscription).mockRejectedValue(failure);

    await actions.transcribe('selected');

    expect(batchCoordinator.startTranscription).toHaveBeenCalledWith(['selected-id']);
    expect(startingChanges).toEqual([true, false]);
    expect(error).toHaveBeenCalledOnce();
  });

  it('resolves normalization against the exact filtered server view', async () => {
    filterVerified.set(true);
    searchQuery.set('  target voice  ');
    commandMocks.getSegmentIdsForView.mockResolvedValue(['one', 'two']);
    const { actions, batchCoordinator, startingChanges } = harness();

    await actions.normalize();

    expect(commandMocks.getSegmentIdsForView).toHaveBeenCalledWith({
      verified: true,
      query: 'target voice',
      transcriptState: 'real',
    });
    expect(batchCoordinator.startNormalization).toHaveBeenCalledWith(['one', 'two']);
    expect(startingChanges).toEqual([true, false]);
  });

  it('covers every transcribe scope and reports empty, missing-selection, and view-read outcomes', async () => {
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    const setup = harness();

    await setup.actions.transcribe('selected');
    expect(warning).toHaveBeenCalledWith('Select a segment first');

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce([]);
    await setup.actions.transcribe('empty');
    expect(commandMocks.getSegmentIdsForView).toHaveBeenLastCalledWith({
      verified: null,
      query: null,
      transcriptState: 'missing',
    });
    expect(info).toHaveBeenCalledWith('No segments need transcription');

    commandMocks.getSegmentIdsForView.mockRejectedValueOnce(new Error('view unavailable'));
    await setup.actions.transcribe('filtered');
    expect(error).toHaveBeenCalledWith('Failed to load segments', {
      cause: expect.any(Error),
    });

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce(['empty-a']);
    await setup.actions.transcribe('empty');
    expect(setup.batchCoordinator.startTranscription).toHaveBeenCalledWith(['empty-a']);
  });

  it('keeps every batch start inert while busy, leased, or outside desktop authority', async () => {
    const busy = harness();
    isProcessing.set(true);
    await busy.actions.transcribe('filtered');
    await busy.actions.assignSpeaker();
    await busy.actions.normalize();
    await busy.actions.rediarize('filtered');
    await busy.actions.deleteFilteredWithConfirm();

    isProcessing.set(false);
    const leased = harness({ getBatchStarting: () => true });
    await leased.actions.transcribe('filtered');
    await leased.actions.assignSpeaker();
    await leased.actions.normalize();
    await leased.actions.rediarize('filtered');

    const browser = harness({ requireDesktopRuntime: () => false });
    await browser.actions.transcribe('filtered');
    await browser.actions.assignSpeaker();
    await browser.actions.normalize();
    await browser.actions.rediarize('filtered');
    await browser.actions.deleteFilteredWithConfirm();

    expect(commandMocks.getSegmentIdsForView).not.toHaveBeenCalled();
    expect(commandMocks.assignSpeakersV1).not.toHaveBeenCalled();
    expect(commandMocks.rediarizeSegments).not.toHaveBeenCalled();
  });

  it('validates speaker assignment and closes both successful and failed durable operations', async () => {
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');

    await harness({ getBatchSpeakerId: () => '   ' }).actions.assignSpeaker();
    expect(warning).toHaveBeenCalledWith('Enter a speaker ID first');

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce([]);
    await harness().actions.assignSpeaker();
    expect(info).toHaveBeenCalledWith('No segments in current filter');

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce(['one', 'two']);
    commandMocks.assignSpeakersV1.mockResolvedValueOnce({ changedCount: 2 });
    vi.spyOn(historyStore, 'refresh').mockResolvedValue(undefined);
    const assigned = harness({ getBatchSpeakerId: () => '  speaker-b  ' });
    await assigned.actions.assignSpeaker();
    expect(commandMocks.assignSpeakersV1).toHaveBeenCalledWith({
      ids: ['one', 'two'],
      targetSpeakerId: 'speaker-b',
    });
    expect(success).toHaveBeenCalledWith('Assigned speaker on 2 segment(s)');
    expect(assigned.loadSegments).toHaveBeenCalledOnce();
    expect(get(activeOperations)).toEqual(new Set());

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce(['three']);
    commandMocks.assignSpeakersV1.mockRejectedValueOnce(new Error('write refused'));
    await harness().actions.assignSpeaker();
    expect(error).toHaveBeenCalledWith('Batch speaker assign failed', {
      cause: expect.any(Error),
    });
    expect(get(isProcessing)).toBe(false);
  });

  it('reports normalization empty/failure paths and always releases its start lease', async () => {
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    commandMocks.getSegmentIdsForView.mockResolvedValueOnce([]);
    const empty = harness();
    await empty.actions.normalize();
    expect(info).toHaveBeenCalledWith('No segments with raw transcripts');
    expect(empty.startingChanges).toEqual([true, false]);

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce(['one']);
    const failed = harness();
    vi.mocked(failed.batchCoordinator.startNormalization).mockRejectedValueOnce(
      new Error('normalizer refused'),
    );
    await failed.actions.normalize();
    expect(error).toHaveBeenCalledWith('Batch normalize failed', { cause: expect.any(Error) });
    expect(failed.startingChanges).toEqual([true, false]);
  });

  it('rediarizes selected and filtered scopes with honest no-op, success, and failure states', async () => {
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');

    await harness().actions.rediarize('selected');
    expect(warning).toHaveBeenCalledWith('Select a segment first');

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce([]);
    await harness().actions.rediarize('filtered');
    expect(info).toHaveBeenCalledWith('No segments to rediarize');

    selectedSegmentId.set('selected');
    commandMocks.rediarizeSegments.mockResolvedValueOnce(1);
    const selected = harness();
    await selected.actions.rediarize('selected');
    expect(commandMocks.rediarizeSegments).toHaveBeenCalledWith(['selected']);
    expect(selected.loadSegments).toHaveBeenCalledOnce();
    expect(success).toHaveBeenCalledWith('Updated 1 segment speaker label(s)');

    commandMocks.getSegmentIdsForView.mockResolvedValueOnce(['filtered']);
    commandMocks.rediarizeSegments.mockRejectedValueOnce(new Error('diarizer refused'));
    await harness().actions.rediarize('filtered');
    expect(error).toHaveBeenCalledWith('Rediarization failed', { cause: expect.any(Error) });
    expect(get(isProcessing)).toBe(false);
    expect(get(activeOperations)).toEqual(new Set());
  });

  it('handles empty/read-failed deletion scopes and preserves unrelated selection on success', async () => {
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    commandMocks.getSegmentIdsForView.mockResolvedValueOnce([]);
    await harness().actions.deleteFilteredWithConfirm();
    expect(info).toHaveBeenCalledWith('No segments in current filter');
    expect(get(showConfirmDialog)).toBeNull();

    commandMocks.getSegmentIdsForView.mockRejectedValueOnce(new Error('scope failed'));
    await harness().actions.deleteFilteredWithConfirm();
    expect(error).toHaveBeenCalledWith('Failed to load segments', { cause: expect.any(Error) });

    segments.set([segment({ id: 'keep' }), segment({ id: 'delete' })]);
    selectedSegmentId.set('keep');
    wordTimestamps.set([{ word: 'keep', start: 0, end: 1, confidence: 1 }]);
    commandMocks.getSegmentIdsForView.mockResolvedValueOnce(['delete']);
    commandMocks.deleteSegmentsBatch.mockResolvedValueOnce(undefined);
    const success = harness();
    await success.actions.deleteFilteredWithConfirm();
    await get(showConfirmDialog)?.onConfirm();
    expect(get(selectedSegmentId)).toBe('keep');
    expect(get(wordTimestamps)).toHaveLength(1);
    expect(success.loadSegments).toHaveBeenCalledOnce();

    isProcessing.set(true);
    await get(showConfirmDialog)?.onConfirm();
    expect(commandMocks.deleteSegmentsBatch).toHaveBeenCalledOnce();
  });
});

describe('workstation export truth', () => {
  type ExportDependencies = Parameters<typeof createWorkstationExportActions>[0];

  function actions(overrides: Partial<ExportDependencies> = {}) {
    return createWorkstationExportActions({
      requireDesktopRuntime: () => true,
      getPromotionStage: () => null,
      isTrainingExportBlocked: () => false,
      trainingExportBlockDetail: () => undefined,
      ...overrides,
    });
  }

  it('blocks training export before opening a folder when promotion evidence is blocked', async () => {
    segmentStats.set({
      total: 10,
      verified: 8,
      pending: 2,
      withAnnotations: 0,
      totalDurationMs: 0,
    });
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    const promotionStage = { status: 'blocked' } as never;
    const exportActions = actions({
      getPromotionStage: () => promotionStage,
      isTrainingExportBlocked: () => true,
      trainingExportBlockDetail: () => '1 unresolved promotion blocker',
    });

    await exportActions.exportHuggingface();

    expect(warning).toHaveBeenCalledWith('HF export blocked by dataset promotion stage', {
      detail: '1 unresolved promotion blocker',
    });
    expect(dialogMocks.chooseDirectory).not.toHaveBeenCalled();
    expect(commandMocks.exportHuggingfaceDataset).not.toHaveBeenCalled();
  });

  it('warns on needs-review evidence but permits the explicitly non-blocked export', async () => {
    segmentStats.set({
      total: 10,
      verified: 8,
      pending: 2,
      withAnnotations: 0,
      totalDurationMs: 0,
    });
    dialogMocks.chooseDirectory.mockResolvedValue('D:\\hf');
    commandMocks.exportHuggingfaceDataset.mockResolvedValue(undefined);
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const promotionStage = { status: 'needs_review' } as never;
    const exportActions = actions({
      getPromotionStage: () => promotionStage,
      trainingExportBlockDetail: () => 'manual review remains',
    });

    await exportActions.exportHuggingface();

    expect(warning).toHaveBeenCalledWith('HF export has review-stage warnings', {
      detail: 'manual review remains',
    });
    expect(commandMocks.exportHuggingfaceDataset).toHaveBeenCalledWith('D:\\hf');
    expect(success).toHaveBeenCalledWith('HuggingFace dataset exported', { detail: 'D:\\hf' });
  });

  it('derives the dataset format from the owner-selected suffix', async () => {
    settings.set({ ...defaultSettings, exportFormat: 'json' });
    dialogMocks.saveFile.mockResolvedValue('D:\\exports\\dataset.CSV');
    commandMocks.exportDataset.mockResolvedValue(undefined);
    vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');

    await actions().exportDataset();

    expect(dialogMocks.saveFile).toHaveBeenCalledWith(
      expect.objectContaining({ defaultPath: 'cortex-dataset.json' }),
    );
    expect(commandMocks.exportDataset).toHaveBeenCalledWith('D:\\exports\\dataset.CSV', 'csv');
  });

  it('exports only verified-good audio and reports partial output without a success claim', async () => {
    segments.set([
      segment({ id: 'accepted', verified: true, humanDecision: 'accept' }),
      segment({ id: 'rejected', verified: true, humanDecision: 'reject' }),
      segment({ id: 'pending', verified: false }),
    ]);
    dialogMocks.chooseDirectory.mockResolvedValue('D:\\audio');
    commandMocks.exportAudio.mockResolvedValue({
      total: 1,
      succeeded: 0,
      failed: 1,
      output_dir: 'D:\\audio',
      files: [],
      errors: ['source missing'],
    });
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');

    await actions().exportAudio();

    expect(commandMocks.exportAudio).toHaveBeenCalledWith(['accepted'], {
      output_dir: 'D:\\audio',
      format: 'Wav',
      sample_rate: 16_000,
      include_metadata: true,
    });
    expect(warning).toHaveBeenCalledWith('Exported 0, 1 failed', { detail: 'D:\\audio' });
    expect(success).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(false);
    expect(get(batchProgress)).toEqual({ status: 'idle', completed: 0, total: 0, percent: 0 });
    expect(get(activeOperations)).toEqual(new Set());
  });

  it('does not open transcript export without corpus evidence', async () => {
    segmentStats.set({ total: 0, verified: 0, pending: 0, withAnnotations: 0, totalDurationMs: 0 });

    await actions().exportTranscript();

    expect(dialogMocks.saveFile).not.toHaveBeenCalled();
    expect(commandMocks.exportTranscript).not.toHaveBeenCalled();
  });

  it.each([
    ['D:\\exports\\dataset.parquet', 'parquet'],
    ['D:\\exports\\dataset.jsonl', 'jsonl'],
    ['D:\\exports\\dataset.unknown', 'json'],
  ] as const)('derives %s as the closed %s dataset format', async (path, format) => {
    dialogMocks.saveFile.mockResolvedValueOnce(path);
    commandMocks.exportDataset.mockResolvedValueOnce(undefined);
    await actions().exportDataset();
    expect(commandMocks.exportDataset).toHaveBeenCalledWith(path, format);
  });

  it('contains dataset cancellation/failure and never opens a picker outside desktop', async () => {
    await actions({ requireDesktopRuntime: () => false }).exportDataset();
    expect(dialogMocks.saveFile).not.toHaveBeenCalled();

    dialogMocks.saveFile.mockResolvedValueOnce(null);
    await actions().exportDataset();
    expect(commandMocks.exportDataset).not.toHaveBeenCalled();

    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    dialogMocks.saveFile.mockRejectedValueOnce(new Error('picker refused'));
    await actions().exportDataset();
    expect(error).toHaveBeenCalledWith('Export failed', { cause: expect.any(Error) });
  });

  it.each([
    ['D:\\exports\\transcript.vtt', 'vtt'],
    ['D:\\exports\\transcript.txt', 'txt'],
    ['D:\\exports\\transcript.srt', 'srt'],
  ] as const)('exports %s with exact %s transcript identity', async (path, format) => {
    segmentStats.set({ total: 1, verified: 1, pending: 0, withAnnotations: 0, totalDurationMs: 1 });
    dialogMocks.saveFile.mockResolvedValueOnce(path);
    commandMocks.exportTranscript.mockResolvedValueOnce(undefined);
    await actions().exportTranscript();
    expect(commandMocks.exportTranscript).toHaveBeenCalledWith(path, format);
  });

  it('guards transcript export while busy/outside desktop and contains cancel/failure paths', async () => {
    segmentStats.set({ total: 1, verified: 1, pending: 0, withAnnotations: 0, totalDurationMs: 1 });
    isProcessing.set(true);
    await actions().exportTranscript();
    isProcessing.set(false);
    await actions({ requireDesktopRuntime: () => false }).exportTranscript();
    expect(dialogMocks.saveFile).not.toHaveBeenCalled();

    dialogMocks.saveFile.mockResolvedValueOnce(null);
    await actions().exportTranscript();
    expect(commandMocks.exportTranscript).not.toHaveBeenCalled();

    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    dialogMocks.saveFile.mockRejectedValueOnce(new Error('transcript picker refused'));
    await actions().exportTranscript();
    expect(error).toHaveBeenCalledWith('Transcript export failed', { cause: expect.any(Error) });
  });

  it('contains HuggingFace guard, cancellation, and failure paths', async () => {
    segmentStats.set({ total: 1, verified: 1, pending: 0, withAnnotations: 0, totalDurationMs: 1 });
    isProcessing.set(true);
    await actions().exportHuggingface();
    isProcessing.set(false);
    await actions({ requireDesktopRuntime: () => false }).exportHuggingface();
    expect(dialogMocks.chooseDirectory).not.toHaveBeenCalled();

    dialogMocks.chooseDirectory.mockResolvedValueOnce(null);
    await actions().exportHuggingface();
    expect(commandMocks.exportHuggingfaceDataset).not.toHaveBeenCalled();

    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    dialogMocks.chooseDirectory.mockRejectedValueOnce(new Error('folder refused'));
    await actions().exportHuggingface();
    expect(error).toHaveBeenCalledWith('HuggingFace export failed', { cause: expect.any(Error) });
  });

  it('guards audio export and reports full success or durable failure without leaked busy state', async () => {
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    await actions({ requireDesktopRuntime: () => false }).exportAudio();
    await actions().exportAudio();
    expect(warning).toHaveBeenCalledWith('No verified segments to export');
    expect(dialogMocks.chooseDirectory).not.toHaveBeenCalled();

    segments.set([segment({ id: 'accepted', verified: true, humanDecision: 'accept' })]);
    dialogMocks.chooseDirectory.mockResolvedValueOnce(null);
    await actions().exportAudio();
    expect(commandMocks.exportAudio).not.toHaveBeenCalled();

    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    dialogMocks.chooseDirectory.mockResolvedValueOnce('D:\\audio');
    commandMocks.exportAudio.mockResolvedValueOnce({
      total: 1,
      succeeded: 1,
      failed: 0,
      output_dir: 'D:\\audio',
      files: ['D:\\audio\\accepted.wav'],
      errors: [],
    });
    await actions().exportAudio();
    expect(success).toHaveBeenCalledWith('Exported 1 audio file(s)', { detail: 'D:\\audio' });

    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    dialogMocks.chooseDirectory.mockResolvedValueOnce('D:\\audio');
    commandMocks.exportAudio.mockRejectedValueOnce(new Error('write refused'));
    await actions().exportAudio();
    expect(error).toHaveBeenCalledWith('Audio export failed', { cause: expect.any(Error) });
    expect(get(isProcessing)).toBe(false);
    expect(get(activeOperations)).toEqual(new Set());
  });
});

describe('workstation exact history actions', () => {
  it('reloads and records only a real undo mutation', async () => {
    const loadSegments = vi.fn(async () => {});
    const recordAction = vi.fn();
    vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    vi.spyOn(historyStore, 'undo').mockResolvedValue({
      action: 'updateSegment',
      status: { undoAction: null, redoAction: 'updateSegment' },
    });
    const historyActions = createWorkstationHistoryActions({
      requireDesktopRuntime: () => true,
      getViewMode: () => 'curate',
      getHistoryPanel: () => ({ recordAction }),
      loadSegments,
    });

    await historyActions.undo();

    expect(loadSegments).toHaveBeenCalledOnce();
    expect(recordAction).toHaveBeenCalledWith('Undo: Update segment', 'edit');
  });

  it('blocks global undo while review owns exact local undo semantics', async () => {
    const undo = vi.spyOn(historyStore, 'undo');
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const historyActions = createWorkstationHistoryActions({
      requireDesktopRuntime: () => true,
      getViewMode: () => 'review',
      getHistoryPanel: () => null,
      loadSegments: vi.fn(),
    });

    await historyActions.undo();

    expect(undo).not.toHaveBeenCalled();
    expect(info).toHaveBeenCalledWith('Press Backspace to undo the last review decision');
  });

  it('does not reload or record when the backend reports nothing to redo', async () => {
    const loadSegments = vi.fn(async () => {});
    const recordAction = vi.fn();
    vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    vi.spyOn(historyStore, 'redo').mockResolvedValue({
      action: null,
      status: { undoAction: null, redoAction: null },
    });
    const historyActions = createWorkstationHistoryActions({
      requireDesktopRuntime: () => true,
      getViewMode: () => 'curate',
      getHistoryPanel: () => ({ recordAction }),
      loadSegments,
    });

    await historyActions.redo();

    expect(loadSegments).not.toHaveBeenCalled();
    expect(recordAction).not.toHaveBeenCalled();
  });
});

describe('workstation single-segment write authority', () => {
  type SegmentDependencies = Parameters<typeof createWorkstationSegmentActions>[0];

  function actions(overrides: Partial<SegmentDependencies> = {}) {
    const loadSegments = vi.fn(async () => {});
    const flushAutosave = vi.fn(async () => {});
    const flushAutosaveIds = vi.fn(async () => true);
    const saveMetadata = vi.fn(async () => ({}));
    const recordAction = vi.fn();
    const segmentActions = createWorkstationSegmentActions({
      requireDesktopRuntime: () => true,
      loadSegments,
      notifyActionableError: vi.fn(),
      pendingAutosaveId: () => null,
      flushAutosave,
      flushAutosaveIds,
      saveMetadata,
      getHistoryPanel: () => ({ recordAction }),
      ...overrides,
    });
    return {
      segmentActions,
      loadSegments,
      flushAutosave,
      flushAutosaveIds,
      saveMetadata,
      recordAction,
    };
  }

  it('hard-stops a human-decided clip before retranscription can overwrite truth', async () => {
    segments.set([segment({ id: 'accepted', verified: true, humanDecision: 'accept' })]);
    selectedSegmentId.set('accepted');
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');

    await actions().segmentActions.transcribe();

    expect(commandMocks.transcribeSegment).not.toHaveBeenCalled();
    expect(info).toHaveBeenCalledWith(
      'This clip already has a human decision. Undo or reopen that review first; ASR will not overwrite reviewed text.',
    );
    expect(get(activeOperations)).toEqual(new Set());
  });

  it('turns champion unavailability into an explicit retry while restoring idle state', async () => {
    const current = segment({ id: 'pending' });
    segments.set([current]);
    selectedSegmentId.set('pending');
    const championFailure = new Error('champion unavailable');
    commandMocks.transcribeSegment.mockRejectedValueOnce(championFailure).mockResolvedValueOnce({});
    commandMocks.is7bUnavailableError.mockImplementation((error) => error === championFailure);
    const { segmentActions, loadSegments } = actions();

    await segmentActions.transcribe();

    expect(get(showConfirmDialog)).toEqual(
      expect.objectContaining({
        title: 'OmniASR-7B champion unavailable',
        confirmLabel: 'Try 7B again',
        danger: false,
      }),
    );
    expect(get(isProcessing)).toBe(false);
    expect(get(pipelinePhase)).toBe('idle');
    expect(get(activeOperations)).toEqual(new Set());
    expect(get(selectedSegmentId)).toBe('pending');

    await get(showConfirmDialog)?.onConfirm();

    expect(commandMocks.transcribeSegment).toHaveBeenCalledTimes(2);
    expect(loadSegments).toHaveBeenCalledOnce();
  });

  it('does not delete when the pending transcript cannot be durably flushed', async () => {
    const current = segment({ id: 'pending-edit' });
    segments.set([current]);
    selectedSegmentId.set('pending-edit');
    const timestamps: WordTimestamp[] = [{ word: 'pending', start: 0, end: 1, confidence: 1 }];
    wordTimestamps.set(timestamps);
    const { segmentActions, flushAutosaveIds } = actions();
    flushAutosaveIds.mockResolvedValue(false);

    segmentActions.deleteWithConfirm();
    await get(showConfirmDialog)?.onConfirm();

    expect(flushAutosaveIds).toHaveBeenCalledWith(['pending-edit']);
    expect(commandMocks.deleteSegment).not.toHaveBeenCalled();
    expect(get(segments)).toEqual([current]);
    expect(get(selectedSegmentId)).toBe('pending-edit');
    expect(get(wordTimestamps)).toEqual(timestamps);
  });

  it('rolls back the optimistic row and selection when durable deletion fails', async () => {
    const current = segment({ id: 'delete-me', audioPath: 'C:\\audio\\delete-me.wav' });
    segments.set([current]);
    selectedSegmentId.set('delete-me');
    const failure = new Error('database busy');
    commandMocks.deleteSegment.mockRejectedValueOnce(failure);
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    vi.spyOn(historyStore, 'refresh').mockResolvedValue(undefined);
    const { segmentActions } = actions();

    segmentActions.deleteWithConfirm();
    await get(showConfirmDialog)?.onConfirm();

    expect(get(segments)).toEqual([current]);
    expect(get(selectedSegmentId)).toBe('delete-me');
    expect(error).toHaveBeenCalledWith('Delete failed', { cause: failure });
  });

  it('flushes an existing autosave instead of issuing a stale duplicate speaker write', async () => {
    const current = segment({ id: 'speaker-edit', speakerId: 'speaker-new' });
    segments.set([current]);
    selectedSegmentId.set('speaker-edit');
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const { segmentActions, flushAutosave, saveMetadata } = actions({
      pendingAutosaveId: () => 'speaker-edit',
    });

    await segmentActions.saveSpeaker();

    expect(flushAutosave).toHaveBeenCalledOnce();
    expect(saveMetadata).not.toHaveBeenCalled();
    expect(success).toHaveBeenCalledWith('Speaker updated');
  });

  it('aligns the effective human transcript and restores processing state after failure', async () => {
    const current = segment({
      id: 'align-me',
      rawTranscript: 'stale raw',
      verdictTranscript: 'human correction',
      humanDecision: 'edit',
    });
    segments.set([current]);
    selectedSegmentId.set('align-me');
    const failure = new Error('aligner failed');
    commandMocks.alignSegment.mockRejectedValueOnce(failure);
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');

    await actions().segmentActions.align();

    expect(commandMocks.alignSegment).toHaveBeenCalledWith(
      current.audioPath,
      'human correction',
      null,
      'align-me',
    );
    expect(error).toHaveBeenCalledWith('Alignment failed', { cause: failure });
    expect(get(isProcessing)).toBe(false);
    expect(get(pipelinePhase)).toBe('idle');
    expect(get(statusMessage)).toBe('Ready');
    expect(get(activeOperations)).toEqual(new Set());
  });
});
