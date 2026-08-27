import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ReviewMode from '../../src/lib/ReviewMode.svelte';
import { searchQuery, segments, selectedSegmentId } from '../../src/lib/stores/segmentStore';
import { defaultSettings, settings } from '../../src/lib/stores/settingsStore';
import { showReviewInbox } from '../../src/lib/stores/uiStore';
import type { SpeechSegment } from '../../src/lib/types';
import { ckb } from '../../src/lib/i18n/ckb';
import { flushReviewDrafts } from '../../src/lib/reviewDraftFlush';

const MEDIA_GRANT_ID = '2f2d9b66-8566-4d1c-8c14-e18d006b776f';
const NEXT_MEDIA_GRANT_ID = '52a492d4-14d8-4e24-9f5d-bc44221b48c1';

const mocks = vi.hoisted(() => ({
  getSegmentsPage: vi.fn(),
  getReviewPageV1: vi.fn(),
  getSegment: vi.fn(),
  getSegmentConsensus: vi.fn(),
  getDatasetStats: vi.fn(),
  getWaveform: vi.fn(),
  alignSegment: vi.fn(),
  recordPlaybackReceipt: vi.fn(),
  recordHumanDecision: vi.fn(),
  commitReviewV1: vi.fn(),
  markSegmentUnusableV1: vi.fn(),
  getReviewDraftV1: vi.fn(),
  saveReviewDraftV1: vi.fn(),
  deleteReviewDraftV1: vi.fn(),
  undoHumanDecision: vi.fn(),
  updateSegmentMetadataV1: vi.fn(),
  registerMediaAsset: vi.fn(),
  registerReviewMediaAsset: vi.fn(),
  getMediaAssetUrl: vi.fn(),
  beginDesktopPlaybackSessionV1: vi.fn(),
  cancelDesktopPlaybackSessionV1: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => ({
  ...mocks,
  is7bUnavailableError: vi.fn(() => false),
  isCommandErrorV1: vi.fn(
    (error: unknown, code?: string) =>
      !!error &&
      typeof error === 'object' &&
      (error as { schema?: number }).schema === 1 &&
      (code === undefined || (error as { code?: string }).code === code),
  ),
  reviewEffectId: vi.fn((decisionId: string) => {
    const match = /^effect:([1-9][0-9]*)$/.exec(decisionId);
    return match ? Number(match[1]) : null;
  }),
  reviewErrorMessage: vi.fn((_error: unknown, fallback: string) => fallback),
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
      source_start_ms: 0,
      source_end_ms: 1_000,
      words: [{ word: 'دەقی', start: 0, end: 0.4, confidence: 0.9 }],
    }),
    alignmentQuality: 'ctc_forced',
    durationMs: 1000,
    speakerId: null,
    verified: false,
    evidenceJson: null,
  };
}

function installPlayableMediaStub() {
  Object.defineProperty(HTMLMediaElement.prototype, 'paused', {
    configurable: true,
    get(this: HTMLMediaElement & { __paused?: boolean }) {
      return this.__paused !== false;
    },
  });
  HTMLMediaElement.prototype.play = function (this: HTMLMediaElement & { __paused?: boolean }) {
    this.__paused = false;
    return Promise.resolve();
  };
  HTMLMediaElement.prototype.pause = function (this: HTMLMediaElement & { __paused?: boolean }) {
    this.__paused = true;
  };
  HTMLMediaElement.prototype.load = vi.fn();
}

let activePlaybackRevision = 0;

async function hearCurrentAudio() {
  const audio = document.querySelector('audio');
  expect(audio).not.toBeNull();
  await waitFor(() => expect(audio!.getAttribute('src')).toBeTruthy());
  Object.defineProperty(audio!, 'duration', { configurable: true, value: 1 });
  audio!.currentTime = 0;
  await fireEvent.loadedMetadata(audio!);
  const play = await waitFor(() => {
    const button = document.querySelector<HTMLButtonElement>(
      '[data-testid="audio-player-timeline"] button',
    );
    expect(button).not.toBeNull();
    return button!;
  });
  await fireEvent.click(play);
  await waitFor(() => expect(audio!.paused).toBe(false));
  audio!.currentTime = 0.9;
  await fireEvent.timeUpdate(audio!);
  await Promise.resolve();
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
    showReviewInbox.set(false);
    activePlaybackRevision = 0;
    mocks.cancelDesktopPlaybackSessionV1.mockResolvedValue(true);
    mocks.getSegmentConsensus.mockResolvedValue({ models: [], words: [] });
    mocks.getDatasetStats.mockResolvedValue({ totalSegments: 1, verifiedCount: 0 });
    mocks.getWaveform.mockResolvedValue([0.1, 0.4, 0.2]);
    mocks.recordPlaybackReceipt.mockImplementation(async () => ({
      playbackReceiptId: '11111111-1111-4111-8111-111111111111',
      segmentId: 'review-1',
      segmentRevision: activePlaybackRevision,
      uniquePlayedMs: 1000,
      clipDurationMs: 1000,
      coverageRatio: 1,
    }));
    mocks.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _mediaGrantId: string, expectedRevision: number) => {
        activePlaybackRevision = expectedRevision;
        return {
          playbackReceiptId: '11111111-1111-4111-8111-111111111111',
          segmentId,
          segmentRevision: expectedRevision,
          clipDurationMs: 1000,
          expiresAtMs: Date.now() + 60_000,
        };
      },
    );
    mocks.recordHumanDecision.mockImplementation(
      async (id: string, action: 'accept' | 'edit' | 'reject', text?: string | null) =>
        decisionCommit({ ...segment(), id }, action, text),
    );
    mocks.getReviewPageV1.mockImplementation(
      async (scope: { kind: string; query?: string }, cursor: string | null, limit: number) => {
        const page = await mocks.getSegmentsPage({
          verified: false,
          query: scope.kind === 'search' ? scope.query : null,
          sort: 'oldest',
          limit,
          cursor,
          focused: true,
        });
        return {
          items: page.items.map((item: SpeechSegment) => ({
            segment: item,
            baseRevision: page.revisions?.[item.id] ?? 0,
            eligible: true,
            disabledReason: null,
          })),
          total: page.total,
          nextCursor: page.nextCursor,
          scopeLabel: scope.kind,
          focusNarrowed: page.focusNarrowed === true,
        };
      },
    );
    mocks.commitReviewV1.mockImplementation(
      async (request: {
        segmentId: string;
        decision: 'accept' | 'edit' | 'reject';
        transcript: string | null;
      }) => {
        const legacy = await mocks.recordHumanDecision(
          request.segmentId,
          request.decision,
          request.transcript,
        );
        return {
          segmentId: legacy.segmentId,
          committedRevision: legacy.decidedRevision,
          authoritativeTranscript: legacy.segment.verdictTranscript ?? legacy.segment.rawTranscript,
          decisionId: `effect:${legacy.effectEventId}`,
        };
      },
    );
    mocks.markSegmentUnusableV1.mockImplementation(
      async (request: {
        segmentId: string;
        baseRevision: number;
        reason: 'decodeFailed' | 'missingFile' | 'permissionDenied' | 'corruptContainer';
      }) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        reason: request.reason,
        effectId: 'flag-effect:202',
      }),
    );
    mocks.getReviewDraftV1.mockResolvedValue(null);
    mocks.saveReviewDraftV1.mockImplementation(
      async (segmentId: string, baseRevision: number, text: string) => ({
        segmentId,
        baseRevision,
        text,
        updatedAt: '2026-08-25T12:00:00.000Z',
      }),
    );
    mocks.deleteReviewDraftV1.mockResolvedValue(true);
    mocks.undoHumanDecision.mockResolvedValue({
      status: 'applied',
      restoredRevision: 2,
      segment: segment(),
    });
    mocks.updateSegmentMetadataV1.mockResolvedValue(undefined);
    mocks.registerMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    mocks.registerReviewMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    mocks.getMediaAssetUrl.mockImplementation(
      async (id: string) => `http://cortex-media.localhost/${id}`,
    );
    installPlayableMediaStub();
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
    expect(errorState).toHaveTextContent(ckb['errors.unknown']);
    expect(errorState).not.toHaveTextContent('database unavailable');
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
    await hearCurrentAudio();

    await fireEvent.click(screen.getByRole('button', { name: ckb['review.markBad'] }));
    await waitFor(() =>
      expect(mocks.recordHumanDecision).toHaveBeenCalledWith('review-1', 'reject', null),
    );
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
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
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.markBad'] }));
    expect(await screen.findByTestId('review-terminal')).toBeInTheDocument();

    await fireEvent.keyDown(window, { key: 'Backspace', code: 'Backspace' });
    await waitFor(() =>
      expect(mocks.undoHumanDecision).toHaveBeenCalledWith(101, expect.any(String)),
    );
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue('authoritative restored text');
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
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

  it('never installs an old same-id hydration under a newer review revision', async () => {
    const stale = { ...segment(), rawTranscript: 'stale revision zero' };
    const fresh = { ...segment(), rawTranscript: 'fresh revision one' };
    let resolveStale!: (value: SpeechSegment) => void;
    let resolveFresh!: (value: SpeechSegment) => void;
    mocks.getSegmentsPage
      .mockResolvedValueOnce({
        items: [{ ...stale, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [stale.id]: 0 },
      })
      .mockResolvedValueOnce({
        items: [{ ...fresh, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [fresh.id]: 1 },
      });
    mocks.getSegment
      .mockReturnValueOnce(
        new Promise<SpeechSegment>((resolve) => {
          resolveStale = resolve;
        }),
      )
      .mockReturnValueOnce(
        new Promise<SpeechSegment>((resolve) => {
          resolveFresh = resolve;
        }),
      );

    render(ReviewMode);
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(1));
    searchQuery.set('fresh');
    await waitFor(() => expect(mocks.getSegment).toHaveBeenCalledTimes(2));

    resolveFresh(fresh);
    expect(await screen.findByRole('textbox')).toHaveValue('fresh revision one');
    resolveStale(stale);
    await Promise.resolve();
    expect(screen.getByRole('textbox')).toHaveValue('fresh revision one');
  });

  it('unmounts the background player and retires its playback authority while the inbox is open', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() => expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(1));
    expect(document.querySelector('audio')).not.toBeNull();

    showReviewInbox.set(true);
    await waitFor(() => expect(document.querySelector('audio')).toBeNull());
    await waitFor(() => expect(mocks.cancelDesktopPlaybackSessionV1).toHaveBeenCalledTimes(1));
  });

  it('restores a matching crash-safe draft without making it server truth', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1.mockResolvedValue({
      segmentId: full.id,
      baseRevision: 0,
      text: 'ڕەشنووسی نەخوازراو',
      updatedAt: '2026-08-25T12:00:00.000Z',
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(editor).toHaveValue('ڕەشنووسی نەخوازراو'));
    expect(screen.getByText(ckb['review.draftRecovered'])).toBeInTheDocument();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('shows stale local text beside server truth and never merges it automatically', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 3 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1.mockResolvedValue({
      segmentId: full.id,
      baseRevision: 2,
      text: 'ڕەشنووسی کۆن',
      updatedAt: '2026-08-25T12:00:00.000Z',
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    expect(await screen.findByText(ckb['review.draftConflictTitle'])).toBeInTheDocument();
    expect(screen.getByText('ڕەشنووسی کۆن')).toBeInTheDocument();
    expect(screen.getAllByText(full.rawTranscript).length).toBeGreaterThan(0);
    expect(editor).toHaveValue(full.rawTranscript);
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: ckb['review.acceptAsIs'] })).toBeDisabled();
    expect(screen.getByRole('button', { name: ckb['review.saveNext'] })).toBeDisabled();
    expect(screen.getByRole('button', { name: ckb['review.markBad'] })).toBeDisabled();
  });

  it('blocks every truth action and shortcut until draft recovery has succeeded', async () => {
    const full = segment();
    let resolveDraft!: (value: null) => void;
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1.mockReturnValue(
      new Promise<null>((resolve) => {
        resolveDraft = resolve;
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await hearCurrentAudio();

    const accept = screen.getByRole('button', { name: ckb['review.acceptAsIs'] });
    const save = screen.getByRole('button', { name: ckb['review.saveNext'] });
    const reject = screen.getByRole('button', { name: ckb['review.markBad'] });
    expect(accept).toBeDisabled();
    expect(save).toBeDisabled();
    expect(reject).toBeDisabled();
    expect(accept).toHaveAttribute('aria-describedby', 'review-draft-disabled-reason');
    expect(reject).toHaveAttribute('aria-describedby', 'review-draft-disabled-reason');

    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    await fireEvent.keyDown(window, { key: 'x', code: 'KeyX' });
    await fireEvent.keyDown(window, { key: 'Enter', code: 'Enter', ctrlKey: true });
    expect(mocks.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
    expect(mocks.markSegmentUnusableV1).not.toHaveBeenCalled();

    resolveDraft(null);
    await waitFor(() => expect(accept).not.toBeDisabled());
    expect(save).not.toBeDisabled();
    expect(reject).not.toBeDisabled();
  });

  it('refuses a wrong-segment draft response and recovers only after an exact retry', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.getReviewDraftV1
      .mockResolvedValueOnce({
        segmentId: 'another-segment',
        baseRevision: 0,
        text: 'دەقی کلیپێکی تر',
        updatedAt: '2026-08-25T12:00:00.000Z',
      })
      .mockResolvedValueOnce(null);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    expect(await screen.findByText(ckb['review.draftLoadFailedHint'])).toBeInTheDocument();
    expect(editor).toHaveValue(full.rawTranscript);
    expect(editor).not.toHaveValue('دەقی کلیپێکی تر');
    const accept = screen.getByRole('button', { name: ckb['review.acceptAsIs'] });
    expect(accept).toBeDisabled();

    await fireEvent.click(screen.getByRole('button', { name: ckb.retry }));
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(accept).not.toBeDisabled());
    expect(editor).toHaveValue(full.rawTranscript);
  });

  it('never deletes an unseen draft when recovery fails during close or a scope switch', async () => {
    const first = { ...segment(), rawTranscript: 'first server transcript' };
    const second = { ...segment(), id: 'review-2', rawTranscript: 'second server transcript' };
    mocks.getSegmentsPage
      .mockResolvedValueOnce({
        items: [{ ...first, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [first.id]: 4 },
      })
      .mockResolvedValueOnce({
        items: [{ ...second, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [second.id]: 9 },
      });
    mocks.getSegment.mockImplementation(async (id: string) => (id === first.id ? first : second));
    mocks.getReviewDraftV1
      .mockRejectedValueOnce(new Error('draft database unavailable'))
      .mockResolvedValueOnce(null);

    render(ReviewMode);
    expect(await screen.findByText(ckb['review.draftLoadFailedHint'])).toBeInTheDocument();
    await expect(flushReviewDrafts()).rejects.toThrow();
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();

    searchQuery.set('second');
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(second.rawTranscript));
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(second.id));
    expect(mocks.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();
  });

  it('serializes a fast navigation round-trip behind the outgoing draft write', async () => {
    const first = { ...segment(), rawTranscript: 'first transcript' };
    const second = { ...segment(), id: 'review-2', rawTranscript: 'second transcript' };
    let releaseSave!: () => void;
    const saveBarrier = new Promise<void>((resolve) => {
      releaseSave = resolve;
    });
    let durableFirstDraft: string | null = null;
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
      revisions: { [first.id]: 4, [second.id]: 9 },
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === first.id ? first : second));
    mocks.getReviewDraftV1.mockImplementation(async (id: string) =>
      id === first.id && durableFirstDraft !== null
        ? {
            segmentId: first.id,
            baseRevision: 4,
            text: durableFirstDraft,
            updatedAt: '2026-08-25T12:00:00.000Z',
          }
        : null,
    );
    mocks.saveReviewDraftV1.mockImplementation(async (segmentId, baseRevision, text) => {
      if (segmentId === first.id) {
        await saveBarrier;
        durableFirstDraft = text;
      }
      return { segmentId, baseRevision, text, updatedAt: '2026-08-25T12:00:00.000Z' };
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(first.id));
    await fireEvent.input(editor, { target: { value: 'latest unsaved human text' } });
    await fireEvent.blur(editor);
    await fireEvent.keyDown(window, { key: 'n', code: 'KeyN' });
    await waitFor(() => expect(editor).toHaveValue(second.rawTranscript));
    await waitFor(() =>
      expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(
        first.id,
        4,
        'latest unsaved human text',
      ),
    );

    await fireEvent.keyDown(window, { key: 'p', code: 'KeyP' });
    await waitFor(() => expect(editor).toHaveValue('latest unsaved human text'));
    expect(mocks.getReviewDraftV1.mock.calls.filter(([id]) => id === first.id)).toHaveLength(1);

    releaseSave();
    await waitFor(() =>
      expect(mocks.getReviewDraftV1.mock.calls.filter(([id]) => id === first.id)).toHaveLength(2),
    );
    expect(editor).toHaveValue('latest unsaved human text');
  });

  it('retries a settled off-screen draft failure at close after A to B navigation', async () => {
    const first = { ...segment(), id: 'review-a', rawTranscript: 'first transcript' };
    const second = { ...segment(), id: 'review-b', rawTranscript: 'second transcript' };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [
        { ...first, alignmentJson: null, evidenceJson: null },
        { ...second, alignmentJson: null, evidenceJson: null },
      ],
      total: 2,
      nextCursor: null,
      revisions: { [first.id]: 4, [second.id]: 9 },
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === first.id ? first : second));
    mocks.saveReviewDraftV1
      .mockRejectedValueOnce(new Error('first write failed'))
      .mockImplementation(async (segmentId, baseRevision, text) => ({
        segmentId,
        baseRevision,
        text,
        updatedAt: '2026-08-25T12:00:00.000Z',
      }));

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(first.id));
    await fireEvent.input(editor, { target: { value: 'exact off-screen A' } });
    await fireEvent.keyDown(window, { key: 'n', code: 'KeyN' });
    await waitFor(() => expect(editor).toHaveValue(second.rawTranscript));
    await waitFor(() =>
      expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(first.id, 4, 'exact off-screen A'),
    );
    expect(mocks.saveReviewDraftV1).toHaveBeenCalledTimes(1);

    // The first Promise has already rejected and disappeared from the old in-flight map shape. The
    // desired-state coordinator must still retain A and retry it through the global close barrier.
    await Promise.resolve();
    await flushReviewDrafts();

    expect(mocks.saveReviewDraftV1).toHaveBeenCalledTimes(2);
    expect(mocks.saveReviewDraftV1).toHaveBeenNthCalledWith(2, first.id, 4, 'exact off-screen A');
  });

  it('debounces an edited transcript into the revision-bound draft command', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 7 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });
    await waitFor(
      () => expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(full.id, 7, 'دەقی ڕاستکراوە'),
      { timeout: 1_500 },
    );
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('flushes an immediate pre-close edit without waiting for the debounce timer', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 11 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await fireEvent.input(editor, { target: { value: 'ڕەشنووسی پێش داخستن' } });
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();

    await flushReviewDrafts();
    expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(full.id, 11, 'ڕەشنووسی پێش داخستن');
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('durably flushes the exact visible correction before playback finalization and commit', async () => {
    const full = segment();
    const order: string[] = [];
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.saveReviewDraftV1.mockImplementation(async (segmentId, baseRevision, text) => {
      order.push('draft');
      return { segmentId, baseRevision, text, updatedAt: '2026-08-25T12:00:00.000Z' };
    });
    mocks.recordPlaybackReceipt.mockImplementation(async (request) => {
      order.push('receipt');
      return {
        playbackReceiptId: request.playbackReceiptId,
        segmentId: full.id,
        segmentRevision: 0,
        uniquePlayedMs: 900,
        clipDurationMs: 1000,
        coverageRatio: 0.9,
      };
    });
    mocks.commitReviewV1.mockImplementation(async (request) => {
      order.push('commit');
      return {
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        authoritativeTranscript: request.transcript ?? full.rawTranscript,
        decisionId: 'effect:303',
      };
    });

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await waitFor(() => expect(mocks.getReviewDraftV1).toHaveBeenCalledWith(full.id));
    await hearCurrentAudio();
    await fireEvent.input(editor, { target: { value: 'دەقی دەستکاری‌کراوی نوێ' } });
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.saveNext'] }));

    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(order).toEqual(['draft', 'receipt', 'commit']);
    expect(mocks.saveReviewDraftV1).toHaveBeenCalledWith(full.id, 0, 'دەقی دەستکاری‌کراوی نوێ');
  });

  it('cannot discard a typed correction through Accept or the A shortcut', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 7 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    const editor = await screen.findByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });
    const accept = screen.getByRole('button', {
      name: new RegExp(ckb['review.acceptAsIs']),
    });
    expect(accept).toBeDisabled();
    expect(accept).toHaveAttribute('aria-describedby', 'review-accept-disabled-reason');
    const reject = screen.getByRole('button', { name: ckb['review.markBad'] });
    expect(reject).toBeDisabled();
    expect(reject).toHaveAttribute('aria-describedby', 'review-reject-disabled-reason');

    await fireEvent.blur(editor);
    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    await fireEvent.keyDown(window, { key: 'x', code: 'KeyX' });
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
    expect(editor).toHaveValue('دەقی ڕاستکراوە');
  });

  it('preserves server review eligibility through hydration and blocks every truth action', async () => {
    const full = segment();
    mocks.getReviewPageV1.mockResolvedValueOnce({
      items: [
        {
          segment: { ...full, alignmentJson: null, evidenceJson: null },
          baseRevision: 7,
          eligible: false,
          disabledReason: 'TRANSCRIPT_NOT_READY',
        },
      ],
      total: 1,
      nextCursor: null,
      scopeLabel: 'pending',
      focusNarrowed: false,
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    const reason = document.getElementById('review-eligibility-disabled-reason');
    expect(reason).toHaveTextContent(ckb['review.transcriptNotReady']);

    const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
    const save = screen.getByRole('button', { name: ckb['review.saveNext'] });
    const reject = screen.getByRole('button', { name: ckb['review.markBad'] });
    for (const action of [accept, save, reject]) {
      expect(action).toBeDisabled();
      expect(action).toHaveAttribute('aria-describedby', 'review-eligibility-disabled-reason');
    }

    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    await fireEvent.keyDown(window, { key: 'x', code: 'KeyX' });
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
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
    await hearCurrentAudio();
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

  it('accrues the final between-timeupdate delta before freezing an exact-threshold receipt', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    const audio = document.querySelector('audio')!;
    await waitFor(() => expect(audio.getAttribute('src')).toBeTruthy());
    Object.defineProperty(audio, 'duration', { configurable: true, value: 1 });
    audio.currentTime = 0;
    await fireEvent.loadedMetadata(audio);
    const play = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>(
        '[data-testid="audio-player-timeline"] button',
      );
      expect(button).not.toBeNull();
      return button!;
    });
    await fireEvent.click(play);
    await waitFor(() => expect(audio.paused).toBe(false));
    audio.currentTime = 0.84;
    await fireEvent.timeUpdate(audio);
    // No timeupdate arrives for the final 10ms before the reviewer clicks. The child-owned pause
    // snapshot must accrue it synchronously; parent bindings alone still contain only 840ms here.
    audio.currentTime = 0.85;

    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
    expect(mocks.recordPlaybackReceipt).toHaveBeenCalledWith({
      playbackReceiptId: '11111111-1111-4111-8111-111111111111',
      mediaGrantId: MEDIA_GRANT_ID,
      intervals: [{ startMs: 0, endMs: 850 }],
    });
    expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1);
  });

  it('uses the server session duration and refuses a local-span threshold one millisecond short', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _grant: string, expectedRevision: number) => ({
        playbackReceiptId: '11111111-1111-4111-8111-111111111111',
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1_001,
        expiresAtMs: Date.now() + 60_000,
      }),
    );

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    const audio = document.querySelector('audio')!;
    await waitFor(() => expect(audio.getAttribute('src')).toBeTruthy());
    Object.defineProperty(audio, 'duration', { configurable: true, value: 1 });
    audio.currentTime = 0;
    await fireEvent.loadedMetadata(audio);
    const play = await waitFor(() => {
      const button = document.querySelector<HTMLButtonElement>(
        '[data-testid="audio-player-timeline"] button',
      );
      expect(button).not.toBeNull();
      return button!;
    });
    await fireEvent.click(play);
    await waitFor(() => expect(audio.paused).toBe(false));
    audio.currentTime = 0.85;

    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(mocks.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();
  });

  it('reissues authority for a stale row revision and never carries the old receipt forward', async () => {
    const full = segment();
    mocks.getSegmentsPage
      .mockResolvedValueOnce({
        items: [{ ...full, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [full.id]: 7 },
      })
      .mockResolvedValueOnce({
        items: [{ ...full, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [full.id]: 8 },
      });
    mocks.getSegment.mockResolvedValue(full);
    mocks.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _grant: string, expectedRevision: number) => ({
        playbackReceiptId: `receipt-revision-${expectedRevision}`,
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1_000,
        expiresAtMs: Date.now() + 60_000,
      }),
    );
    mocks.recordPlaybackReceipt.mockImplementation(async ({ playbackReceiptId }) => ({
      playbackReceiptId,
      segmentId: full.id,
      segmentRevision: Number(playbackReceiptId.replace('receipt-revision-', '')),
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    }));
    mocks.commitReviewV1
      .mockRejectedValueOnce({
        schema: 1,
        code: 'STALE_REVISION',
        message: 'row changed',
        retryable: false,
      })
      .mockImplementationOnce(async (request) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        authoritativeTranscript: request.transcript ?? full.rawTranscript,
        decisionId: 'effect:808',
      }));

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await waitFor(() =>
      expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        full.id,
        MEDIA_GRANT_ID,
        7,
        expect.any(String),
      ),
    );
    await hearCurrentAudio();
    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() =>
      expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        full.id,
        MEDIA_GRANT_ID,
        8,
        expect.any(String),
      ),
    );
    const issuanceCalls = mocks.beginDesktopPlaybackSessionV1.mock.calls;
    expect(issuanceCalls[1][3]).not.toBe(issuanceCalls[0][3]);
    await hearCurrentAudio();
    await fireEvent.click(
      screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) }),
    );

    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(2));
    expect(mocks.recordPlaybackReceipt.mock.calls[1][0]).toMatchObject({
      playbackReceiptId: 'receipt-revision-8',
    });
    expect(mocks.commitReviewV1.mock.calls[1][0]).toMatchObject({
      baseRevision: 8,
      playbackReceiptId: 'receipt-revision-8',
    });
  });

  it('retries every ambiguous typed finalization error with the first immutable interval union', async () => {
    const full = segment();
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...full, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
      revisions: { [full.id]: 0 },
    });
    mocks.getSegment.mockResolvedValue(full);
    mocks.recordPlaybackReceipt
      .mockRejectedValueOnce({
        schema: 1,
        code: 'EVIDENCE_WRITE_FAILED',
        message: 'response stage failed',
        retryable: true,
      })
      .mockImplementationOnce(async (request) => ({
        playbackReceiptId: request.playbackReceiptId,
        segmentId: full.id,
        segmentRevision: 0,
        uniquePlayedMs: 900,
        clipDurationMs: 1_000,
        coverageRatio: 0.9,
      }));

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    await hearCurrentAudio();
    const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
    await fireEvent.click(accept);
    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();

    const audio = document.querySelector('audio')!;
    const play = document.querySelector<HTMLButtonElement>(
      '[data-testid="audio-player-timeline"] button',
    )!;
    await fireEvent.click(play);
    await waitFor(() => expect(audio.paused).toBe(false));
    audio.currentTime = 1;
    await fireEvent.timeUpdate(audio);
    await fireEvent.click(accept);

    await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(2));
    expect(mocks.recordPlaybackReceipt.mock.calls[1][0]).toEqual(
      mocks.recordPlaybackReceipt.mock.calls[0][0],
    );
    expect(mocks.recordPlaybackReceipt.mock.calls[1][0].intervals).toEqual([
      { startMs: 0, endMs: 900 },
    ]);
    await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
  });

  it.each([
    'PLAYBACK_SESSION_EXPIRED',
    'PLAYBACK_EVIDENCE_CHANGED',
    'PLAYBACK_MEDIA_GRANT_UNAVAILABLE',
  ])(
    'retires a proven non-commit %s and succeeds only after a fresh grant/session listen',
    async (code) => {
      const full = segment();
      mocks.getSegmentsPage.mockResolvedValue({
        items: [{ ...full, alignmentJson: null, evidenceJson: null }],
        total: 1,
        nextCursor: null,
        revisions: { [full.id]: 0 },
      });
      mocks.getSegment.mockResolvedValue(full);
      let issuance = 0;
      mocks.beginDesktopPlaybackSessionV1.mockImplementation(
        async (segmentId: string, _grant: string, expectedRevision: number) => ({
          playbackReceiptId: `receipt-attempt-${++issuance}`,
          segmentId,
          segmentRevision: expectedRevision,
          clipDurationMs: 1_000,
          expiresAtMs: Date.now() + 60_000,
        }),
      );
      mocks.recordPlaybackReceipt
        .mockRejectedValueOnce({
          schema: 1,
          code,
          message:
            code === 'PLAYBACK_SESSION_EXPIRED'
              ? 'the 30-minute session expired'
              : 'the source or grant changed',
          retryable: true,
          suggestedAction: 'reloadClip',
        })
        .mockImplementationOnce(async (request) => ({
          playbackReceiptId: request.playbackReceiptId,
          segmentId: full.id,
          segmentRevision: 0,
          uniquePlayedMs: 900,
          clipDurationMs: 1_000,
          coverageRatio: 0.9,
        }));

      render(ReviewMode);
      expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
      await hearCurrentAudio();
      const accept = screen.getByRole('button', { name: new RegExp(ckb['review.acceptAsIs']) });
      await fireEvent.click(accept);
      await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
      expect(mocks.commitReviewV1).not.toHaveBeenCalled();
      await waitFor(() => expect(mocks.beginDesktopPlaybackSessionV1).toHaveBeenCalledTimes(2));

      await hearCurrentAudio();
      await fireEvent.click(accept);
      await waitFor(() => expect(mocks.recordPlaybackReceipt).toHaveBeenCalledTimes(2));
      expect(mocks.recordPlaybackReceipt.mock.calls[0][0]).toMatchObject({
        playbackReceiptId: 'receipt-attempt-1',
      });
      expect(mocks.recordPlaybackReceipt.mock.calls[1][0]).toMatchObject({
        playbackReceiptId: 'receipt-attempt-2',
      });
      await waitFor(() => expect(mocks.commitReviewV1).toHaveBeenCalledTimes(1));
    },
  );

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
    await hearCurrentAudio();

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
    await waitFor(() => expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled());
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
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
    expect(mocks.recordHumanDecision).not.toHaveBeenCalled();

    await fireEvent.keyDown(window, { key: 'ArrowLeft', code: 'ArrowLeft' });
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue('دەقی دەستکاریکراو'));
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();

    view.unmount();
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
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
  it('falls through a BLANK annotated column to the champion raw draft', async () => {
    // VERBATIM LAW precedence is human ▸ annotated ▸ raw, and a blank annotated column is ABSENT.
    // `??` only falls through on null, so a whitespace-only annotated row masked the champion draft
    // and opened an EMPTY editor — the reviewer then retypes (or accepts) text nobody drafted.
    // Fail-before: the editor holds '   ' here.
    const blank = { ...segment(), annotatedTranscript: '   ' };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...blank, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(blank);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue(blank.rawTranscript);
  });

  it('still prefers a real annotated (human) transcript over the raw draft', async () => {
    const annotated = { ...segment(), annotatedTranscript: 'دەقی مرۆیی' };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [{ ...annotated, alignmentJson: null, evidenceJson: null }],
      total: 1,
      nextCursor: null,
    });
    mocks.getSegment.mockResolvedValue(annotated);

    render(ReviewMode);
    expect(await screen.findByTestId('review-action-bar')).toBeInTheDocument();
    expect(screen.getByRole('textbox')).toHaveValue('دەقی مرۆیی');
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
    expect(mocks.updateSegmentMetadataV1).not.toHaveBeenCalled();
  });

  it('requires an explicit technical reason, retains the clip on failure, and advances only after a verified unusable commit', async () => {
    const failed = {
      ...segment(),
      id: 'unplayable-1',
      audioPath: 'C:\\audio\\gone.wav',
      rawTranscript: 'ڕەشنووسی پارێزراو',
    };
    const next = {
      ...segment(),
      id: 'playable-2',
      audioPath: 'C:\\audio\\next.wav',
      rawTranscript: 'پارچەی دواتر',
    };
    mocks.getSegmentsPage.mockResolvedValue({
      items: [failed, next],
      total: 2,
      nextCursor: null,
      revisions: { [failed.id]: 4, [next.id]: 9 },
    });
    mocks.getSegment.mockImplementation(async (id: string) => (id === failed.id ? failed : next));
    mocks.registerReviewMediaAsset.mockImplementation(async (path: string) => {
      if (path === failed.audioPath) throw new Error('file missing');
      return { id: NEXT_MEDIA_GRANT_ID };
    });
    mocks.markSegmentUnusableV1
      .mockRejectedValueOnce(new Error('transport response lost'))
      .mockResolvedValueOnce({
        segmentId: 'wrong-segment',
        committedRevision: 5,
        reason: 'missingFile',
        effectId: 'flag-effect:202',
      })
      .mockImplementationOnce(async (request) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        reason: request.reason,
        effectId: 'flag-effect:202',
      }));

    render(ReviewMode);
    expect(await screen.findByTestId('review-technical-unusable')).toBeInTheDocument();
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'ڕەشنووسی نەپاشەکەوتکراو' } });
    const reason = screen.getByLabelText(ckb['review.unusable.reasonLabel']);
    const mark = screen.getByRole('button', { name: ckb['review.unusable.mark'] });
    expect(mark).toBeDisabled();
    reason.focus();
    await fireEvent.keyDown(reason, { key: 'ArrowDown', code: 'ArrowDown' });
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('gone.wav');
    await fireEvent.change(reason, { target: { value: 'missingFile' } });
    // A technical disposition clears the revision-bound draft atomically. It must not silently erase
    // a visible correction: the reviewer explicitly resets it before the action becomes available.
    expect(mark).toBeDisabled();
    expect(mark).toHaveAttribute(
      'aria-describedby',
      'review-unusable-help review-reject-disabled-reason',
    );
    await fireEvent.click(screen.getByRole('button', { name: ckb['review.reset'] }));
    expect(mark).toBeEnabled();

    mark.focus();
    await fireEvent.click(mark);
    await waitFor(() => expect(mocks.markSegmentUnusableV1).toHaveBeenCalledTimes(1));
    expect(editor).toHaveValue(failed.rawTranscript);
    expect(document.activeElement).toBe(mark);
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('gone.wav');
    expect(mocks.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(mocks.commitReviewV1).not.toHaveBeenCalled();

    await fireEvent.click(mark);
    await waitFor(() => expect(mocks.markSegmentUnusableV1).toHaveBeenCalledTimes(2));
    expect(editor).toHaveValue(failed.rawTranscript);
    expect(document.activeElement).toBe(mark);
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('gone.wav');

    mocks.saveReviewDraftV1.mockClear();
    await fireEvent.click(mark);
    await waitFor(() => expect(mocks.markSegmentUnusableV1).toHaveBeenCalledTimes(3));
    const first = mocks.markSegmentUnusableV1.mock.calls[0][0];
    const replay = mocks.markSegmentUnusableV1.mock.calls[1][0];
    const verifiedRetry = mocks.markSegmentUnusableV1.mock.calls[2][0];
    expect(first).toEqual({
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
      segmentId: failed.id,
      baseRevision: 4,
      reason: 'missingFile',
    });
    expect(replay).toEqual(first);
    expect(verifiedRetry).toEqual(first);
    await waitFor(() => expect(screen.getByRole('textbox')).toHaveValue(next.rawTranscript));
    expect(screen.getByTestId('review-source-file')).toHaveTextContent('next.wav');
    await new Promise((resolve) => setTimeout(resolve, 550));
    expect(mocks.saveReviewDraftV1).not.toHaveBeenCalled();
  });
});
