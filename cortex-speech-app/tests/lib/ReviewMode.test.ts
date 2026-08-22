import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ReviewMode from '../../src/lib/ReviewMode.svelte';
import { searchQuery, segments, selectedSegmentId } from '../../src/lib/stores/segmentStore';
import { defaultSettings, settings } from '../../src/lib/stores/settingsStore';
import type { SpeechSegment } from '../../src/lib/types';
import { ckb } from '../../src/lib/i18n/ckb';

const mocks = vi.hoisted(() => ({
  getSegmentsPage: vi.fn(),
  getSegment: vi.fn(),
  getSegmentConsensus: vi.fn(),
  getDatasetStats: vi.fn(),
  getWaveform: vi.fn(),
  alignSegment: vi.fn(),
  recordPlaybackReceipt: vi.fn(),
  recordHumanDecision: vi.fn(),
  undoHumanDecision: vi.fn(),
  updateSegmentFields: vi.fn(),
  registerMediaAsset: vi.fn(),
  getMediaAssetUrl: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => ({
  ...mocks,
  is7bUnavailableError: vi.fn(() => false),
  engineLabel: vi.fn((id: string) => id),
}));

function segment(): SpeechSegment {
  return {
    id: 'review-1',
    createdAt: '2026-08-15T00:00:00Z',
    audioPath: 'C:\\audio\\review.wav',
    rawTranscript: 'دەقی تاقیکردنەوە',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: JSON.stringify({
      words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }],
    }),
    alignmentQuality: 'ctc_forced',
    durationMs: 1000,
    speakerId: null,
    verified: false,
    evidenceJson: null,
  };
}

function decisionCommit(
  source: SpeechSegment,
  action: 'accept' | 'edit' | 'reject',
  text?: string | null,
) {
  return {
    effectEventId: 101,
    segmentId: source.id,
    effectiveAction: action,
    priorRevision: 0,
    decidedRevision: 1,
    segment: {
      ...source,
      humanDecision: action,
      verdict: `human_${action}`,
      verdictTranscript: text ?? source.verdictTranscript ?? source.rawTranscript,
      annotatedTranscript: text ?? source.annotatedTranscript,
      verified: true,
    },
  };
}

describe('ReviewMode windowed queue', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    segments.set([]);
    selectedSegmentId.set(null);
    searchQuery.set('');
    settings.set({ ...defaultSettings });
    mocks.getSegmentConsensus.mockResolvedValue({ models: [], words: [] });
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 1, verifiedCount: 0 });
    mocks.getWaveform.mockResolvedValue([0.1, 0.4, 0.2]);
    mocks.recordPlaybackReceipt.mockResolvedValue(undefined);
    mocks.recordHumanDecision.mockImplementation(
      async (id: string, action: 'accept' | 'edit' | 'reject', text?: string | null) =>
        decisionCommit({ ...segment(), id }, action, text),
    );
    mocks.undoHumanDecision.mockResolvedValue({
      status: 'applied',
      restoredRevision: 2,
      segment: segment(),
    });
    mocks.updateSegmentFields.mockResolvedValue(undefined);
    mocks.registerMediaAsset.mockResolvedValue({ id: 'asset-key' });
    mocks.getMediaAssetUrl.mockResolvedValue('C:\\audio\\review.wav');
    Object.defineProperty(HTMLMediaElement.prototype, 'pause', {
      configurable: true,
      value: vi.fn(),
    });
    Object.defineProperty(HTMLMediaElement.prototype, 'load', {
      configurable: true,
      value: vi.fn(),
    });
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
      configurable: true,
      value: vi.fn(() => null),
    });
  });

  it('shows a truthful terminal only after the pending-page request returns empty', async () => {
    mocks.getSegmentsPage.mockResolvedValue({ items: [], total: 0, nextCursor: null });
    render(ReviewMode);
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
    expect(mocks.getSegmentsPage).toHaveBeenCalledWith(
      expect.objectContaining({ verified: false, limit: 100, cursor: null }),
    );
  });

  it('shows a retryable error instead of claiming the queue is complete when loading fails', async () => {
    mocks.getSegmentsPage
      .mockRejectedValueOnce(new Error('database unavailable'))
      .mockResolvedValueOnce({ items: [], total: 0, nextCursor: null });

    render(ReviewMode);
    const errorState = await screen.findByTestId('review-load-error');
    expect(errorState).toHaveTextContent('database unavailable');
    expect(screen.queryByTestId('review-terminal')).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: ckb.retry }));
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
    expect(mocks.getSegmentsPage).toHaveBeenCalledTimes(2);
  });

  it('hydrates only the current row and removes a persisted rejection from the queue', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(mocks.getSegment).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByRole('button', { name: ckb['review.markBad'] }));
    await waitFor(() =>
      expect(mocks.recordHumanDecision).toHaveBeenCalledWith('review-1', 'reject', null),
    );
    expect(mocks.updateSegmentFields).not.toHaveBeenCalled();
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();
  });

  it('undoes by immutable effect id and applies only the authoritative server row', async () => {
    const original = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...original, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(original);
    mocks.undoHumanDecision.mockResolvedValue({
      status: 'applied',
      restoredRevision: 2,
      segment: { ...original, rawTranscript: 'authoritative restored text' },
    });

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.markBad'] }));
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();

    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
    await waitFor(() =>
      expect(mocks.undoHumanDecision).toHaveBeenCalledWith(101, expect.any(String)),
    );
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue('authoritative restored text');
    expect(mocks.updateSegmentFields).not.toHaveBeenCalled();
  });

  it('never exposes a lightweight row before its chunk metadata is hydrated', async () => {
    const full = {
      ...segment(),
      alignmentJson: JSON.stringify({
        source_start_ms: 12_000,
        source_end_ms: 13_000,
        chunk_index: 2,
        chunk_count: 4,
        words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }],
      }),
    };
    let resolveHydration!: (value: SpeechSegment) => void;
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockReturnValue(
      new Promise<SpeechSegment>((resolve) => {
        resolveHydration = resolve;
      }),
    );

    render(ReviewMode);
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledWith('review-1'));
    expect(screen.queryByTestId('review-action-bar')).not.toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();

    resolveHydration(full);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();
  });

  it('keeps the next row non-actionable after a decision until that row is fully hydrated', async () => {
    const first = segment();
    const second: SpeechSegment = {
      ...segment(),
      id: 'review-2',
      audioPath: 'C:\\audio\\review-2.wav',
      alignmentJson: JSON.stringify({
        source_start_ms: 8_000,
        source_end_ms: 9_000,
        chunk_index: 1,
        chunk_count: 3,
        words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }],
      }),
    };
    let resolveSecond!: (value: SpeechSegment) => void;
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 2, verifiedCount: 0 });
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValueOnce(first).mockReturnValueOnce(
      new Promise<SpeechSegment>((resolve) => {
        resolveSecond = resolve;
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledWith('review-2'));
    expect(screen.queryByTestId('review-action-bar')).not.toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();

    resolveSecond(second);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(mocks.alignSegment).not.toHaveBeenCalled();
  });

  it('never copies a saved clip into another editor when navigation wins the decision race', async () => {
    const first = segment();
    const second: SpeechSegment = {
      ...segment(),
      id: 'review-2',
      rawTranscript: 'دەقی دووەم',
      audioPath: 'C:\\audio\\review-2.wav',
    };
    let resolveDecision!: (value: ReturnType<typeof decisionCommit>) => void;
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 2, verifiedCount: 0 });
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
    });
    mocks.getSegment.mockImplementation((id: string) =>
      Promise.resolve(id === first.id ? first : second),
    );
    mocks.recordHumanDecision.mockReturnValue(
      new Promise<ReturnType<typeof decisionCommit>>((resolve) => {
        resolveDecision = resolve;
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue(first.rawTranscript);

    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );
    await waitFor(() =>
      expect(mocks.recordHumanDecision).toHaveBeenCalledWith(
        first.id,
        'accept',
        first.rawTranscript,
      ),
    );

    // Keyboard navigation remains available while the slow decision call is in flight.
    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript));

    resolveDecision(decisionCommit(first, 'accept', first.rawTranscript));
    await waitFor(() => expect(mocks.updateSegmentFields).not.toHaveBeenCalled());
    expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript);
  });

  it('keeps navigation drafts in session memory and never persists them without a decision', async () => {
    const first = segment();
    const second: SpeechSegment = {
      ...segment(),
      id: 'review-2',
      rawTranscript: 'دەقی دووەم',
      audioPath: 'C:\\audio\\review-2.wav',
    };
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 2, verifiedCount: 0 });
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
    });
    mocks.getSegment.mockImplementation((id: string) =>
      Promise.resolve(id === first.id ? first : second),
    );

    const view = render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی دەستکاریکراو' } });

    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript));
    expect(mocks.updateSegmentFields).not.toHaveBeenCalled();
    expect(mocks.recordHumanDecision).not.toHaveBeenCalled();

    await fireEvent.keyDown(window, { key: 'ArrowLeft', code: 'ArrowLeft' });
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('دەقی دەستکاریکراو'));
    expect(mocks.updateSegmentFields).not.toHaveBeenCalled();

    view.unmount();
    expect(mocks.updateSegmentFields).not.toHaveBeenCalled();
    expect(mocks.recordHumanDecision).not.toHaveBeenCalled();
  });

  it('aligns only with hydrated chunk metadata and force-refreshes the persisted result', async () => {
    settings.set({ ...defaultSettings, autoAlign: true });
    const chunkJson = JSON.stringify({
      source_start_ms: 4_000,
      source_end_ms: 5_000,
      chunk_index: 1,
      chunk_count: 2,
      words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.7 }],
    });
    const heuristic = {
      ...segment(),
      alignmentJson: chunkJson,
      alignmentQuality: 'energy_heuristic',
    };
    const refreshed = { ...heuristic, alignmentQuality: 'ctc_forced' };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...heuristic, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValueOnce(heuristic).mockResolvedValueOnce(refreshed);
    mocks.alignSegment.mockResolvedValue([{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }]);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() =>
      expect(mocks.alignSegment).toHaveBeenCalledWith(
        heuristic.audioPath,
        heuristic.rawTranscript,
        chunkJson,
        heuristic.id,
      ),
    );
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(2));
  });
  it('refuses to record a verdict when the clip audio could not be played', async () => {
    // AUDIT FIND 2026-08-17: the player showed its error banner while Accept/Save stayed live, so a
    // clip whose audio failed (missing permission, corrupt container, decode failure) could be marked
    // human-verified by someone who never heard it. `speech_segments` cannot tell that apart from a
    // real listen, and this is a VERBATIM corpus — the queue already refuses clips whose FILE is gone
    // (2026-08-15); this covers every other failure mode. Fail-before: without the guard,
    // recordHumanDecision IS called here.
    const seg: SpeechSegment = {
      ...segment(),
      id: 'unplayable-1',
      audioPath: 'C:\\audio\\gone.wav',
    };
    mocks.getSegmentsPage.mockResolvedValue({ items: [seg], total: 1, nextCursor: null });
    mocks.getSegment.mockResolvedValue(seg);
    // The real failure path: resolving the playable URL throws, so AudioPlayer sets its error state.
    mocks.getMediaAssetUrl.mockRejectedValue(new Error('audio unavailable'));
    mocks.registerMediaAsset.mockRejectedValue(new Error('audio unavailable'));

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();

    const accept = await screen.findByText(new RegExp(ckb['review.acceptAsIs']));
    await fireEvent.click(accept);
    // The refusal is the assertion: nothing was written.
    await waitFor(() => expect(mocks.recordHumanDecision).not.toHaveBeenCalled());
    expect(mocks.updateSegmentFields).not.toHaveBeenCalled();
  });
});
