import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  getReviewPageV1: vi.fn(),
  getReviewDraftV1: vi.fn(),
  saveReviewDraftV1: vi.fn(),
  getSettings: vi.fn(),
  getSegmentIdsForView: vi.fn(),
  runJuryPipeline: vi.fn(),
  recordPlaybackReceipt: vi.fn(),
  commitReviewV1: vi.fn(),
  markSegmentUnusableV1: vi.fn(),
  reviewEffectId: vi.fn(),
  registerMediaAsset: vi.fn(),
  registerReviewMediaAsset: vi.fn(),
  getMediaAssetUrl: vi.fn(),
  beginDesktopPlaybackSessionV1: vi.fn(),
  cancelDesktopPlaybackSessionV1: vi.fn(),
  isCommandErrorV1: vi.fn(
    (error: unknown, code?: string) =>
      !!error &&
      typeof error === 'object' &&
      (error as { schema?: number }).schema === 1 &&
      (code === undefined || (error as { code?: string }).code === code),
  ),
  reviewErrorMessage: vi.fn((_error: unknown, fallback: string) => fallback),
}));

vi.mock('../../src/lib/commands', () => api);

import ReviewInbox from '../../src/lib/ReviewInbox.svelte';
import { locale } from '../../src/lib/i18n';
import { ckb } from '../../src/lib/i18n/ckb';
import { en } from '../../src/lib/i18n/en';
import type { SpeechSegment } from '../../src/lib/types';

const MEDIA_GRANT_ID = '2f2d9b66-8566-4d1c-8c14-e18d006b776f';
const NEXT_MEDIA_GRANT_ID = '52a492d4-14d8-4e24-9f5d-bc44221b48c1';

function inboxSegment(id: string, withAudio = false): SpeechSegment {
  return {
    id,
    audioPath: withAudio ? `C:\\audio\\${id}.wav` : '',
    rawTranscript: 'دەق',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: JSON.stringify({ source_start_ms: 0, source_end_ms: 1_000 }),
    durationMs: 1000,
    speakerId: null,
    verified: false,
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

function reviewPage(
  segments: SpeechSegment[],
  options: {
    baseRevision?: number;
    eligible?: boolean;
    disabledReason?: string | null;
    total?: number;
    nextCursor?: string | null;
  } = {},
) {
  const baseRevision = options.baseRevision ?? 7;
  const eligible = options.eligible ?? true;
  return {
    items: segments.map((segment) => ({
      segment,
      baseRevision,
      eligible,
      disabledReason: options.disabledReason ?? null,
    })),
    total: options.total ?? segments.length,
    nextCursor: options.nextCursor ?? null,
    scopeLabel: 'escalation',
    focusNarrowed: false,
  };
}

describe('ReviewInbox queue loading', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    installPlayableMediaStub();
    api.cancelDesktopPlaybackSessionV1.mockResolvedValue(true);
    api.getReviewDraftV1.mockResolvedValue(null);
    api.saveReviewDraftV1.mockImplementation(
      async (segmentId: string, baseRevision: number, text: string) => ({
        segmentId,
        baseRevision,
        text,
        updatedAt: '2026-08-26T00:00:00Z',
      }),
    );
    api.getSettings.mockResolvedValue(null);
    api.registerMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    api.registerReviewMediaAsset.mockResolvedValue({ id: MEDIA_GRANT_ID });
    api.getMediaAssetUrl.mockImplementation(
      async (id: string) => `http://cortex-media.localhost/${id}`,
    );
    api.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _mediaGrantId: string, expectedRevision: number) => ({
        playbackReceiptId: `receipt-${segmentId}`,
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1000,
        expiresAtMs: Date.now() + 60_000,
      }),
    );
    api.recordPlaybackReceipt.mockImplementation(
      async ({ playbackReceiptId }: { playbackReceiptId: string }) => ({
        playbackReceiptId,
        segmentId: playbackReceiptId.replace(/^receipt-/, ''),
        segmentRevision: 7,
        uniquePlayedMs: 1000,
        clipDurationMs: 1000,
        coverageRatio: 1,
      }),
    );
    api.commitReviewV1.mockImplementation(
      async (request: { segmentId: string; baseRevision: number; transcript: string | null }) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        authoritativeTranscript: request.transcript ?? 'دەق',
        decisionId: 'effect:101',
      }),
    );
    api.markSegmentUnusableV1.mockImplementation(
      async (request: {
        segmentId: string;
        baseRevision: number;
        reason: 'decodeFailed' | 'missingFile' | 'permissionDenied' | 'corruptContainer';
      }) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        reason: request.reason,
        effectId: 'flag-effect:303',
      }),
    );
    api.reviewEffectId.mockImplementation((decisionId: string) => {
      const match = /^effect:([1-9][0-9]*)$/.exec(decisionId);
      return match ? Number(match[1]) : null;
    });
    Element.prototype.scrollIntoView = vi.fn();
    locale.set('en');
  });

  afterEach(cleanup);

  it('shows a retryable error instead of claiming an unread queue is empty', async () => {
    api.getReviewPageV1.mockRejectedValueOnce(new Error('database unavailable'));
    api.getReviewPageV1.mockResolvedValueOnce(reviewPage([]));

    render(ReviewInbox);

    const alert = await screen.findByTestId('review-inbox-load-error');
    expect(alert).toHaveTextContent('Could not load the review queue');
    expect(alert).toHaveTextContent('An unknown review error occurred');
    expect(alert).not.toHaveTextContent('database unavailable');
    expect(screen.queryByText('Inbox zero!')).not.toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: 'Review Inbox' })).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() =>
      expect(screen.queryByTestId('review-inbox-load-error')).not.toBeInTheDocument(),
    );
    expect(await screen.findByText('Inbox zero!')).toBeInTheDocument();
  });

  it('continues the escalation cursor explicitly and caps residency at three pages', async () => {
    const rows = (start: number, count: number) =>
      Array.from({ length: count }, (_, offset) =>
        inboxSegment(`row-${String(start + offset).padStart(4, '0')}`),
      );
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage(rows(1, 200), { total: 800, nextCursor: 'cursor-1' }))
      .mockResolvedValueOnce(reviewPage(rows(201, 200), { total: 800, nextCursor: 'cursor-2' }))
      .mockResolvedValueOnce(reviewPage(rows(401, 200), { total: 800, nextCursor: 'cursor-3' }))
      .mockResolvedValueOnce(reviewPage(rows(601, 200), { total: 800, nextCursor: null }));

    render(ReviewInbox);
    expect(await screen.findByText('200 of 800 escalation rows loaded')).toBeInTheDocument();

    for (const [index, expectedLoaded] of [400, 600, 600].entries()) {
      await fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
      await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(index + 2));
      if (index < 2) {
        await waitFor(() =>
          expect(
            screen.getByText(`${expectedLoaded} of 800 escalation rows loaded`),
          ).toBeInTheDocument(),
        );
      } else {
        await waitFor(() => expect(screen.queryByRole('button', { name: 'Load more' })).toBeNull());
      }
    }

    expect(api.getReviewPageV1.mock.calls.slice(1).map((call) => call[1])).toEqual([
      'cursor-1',
      'cursor-2',
      'cursor-3',
    ]);
    expect(within(screen.getByRole('listbox')).getAllByRole('option')).toHaveLength(600);
    expect(screen.getByText('row-0001…')).toBeInTheDocument();
    expect(screen.queryByText('row-0002…')).not.toBeInTheDocument();
    expect(screen.getByText('row-0800…')).toBeInTheDocument();
    expect(screen.getByTestId('inbox-eviction-notice')).toHaveTextContent(
      '200 earlier rows were released',
    );
    expect(screen.getByRole('button', { name: 'Reload from start' })).toBeInTheDocument();
  });

  it('never lets a duplicate later page replace the selected revision or active correction', async () => {
    const selected = { ...inboxSegment('selected', true), rawTranscript: 'server text revision 7' };
    const duplicate = {
      ...selected,
      rawTranscript: 'concurrently changed server text revision 8',
    };
    api.getReviewPageV1
      .mockResolvedValueOnce(
        reviewPage([selected, inboxSegment('row-2')], {
          baseRevision: 7,
          total: 3,
          nextCursor: 'cursor-1',
        }),
      )
      .mockResolvedValueOnce(
        reviewPage([duplicate, inboxSegment('row-3')], {
          baseRevision: 8,
          total: 3,
          nextCursor: null,
        }),
      );

    render(ReviewInbox);
    expect(await screen.findByText('server text revision 7')).toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByLabelText(/Edit transcript/);
    expect(editor).toHaveValue('server text revision 7');
    await fireEvent.input(editor, { target: { value: 'new human correction' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
    await waitFor(() => expect(api.getReviewPageV1).toHaveBeenCalledTimes(2));
    expect(editor).toHaveValue('new human correction');
    expect(screen.queryByText('concurrently changed server text revision 8')).toBeNull();

    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).toHaveBeenCalledWith(
      expect.objectContaining({
        segmentId: selected.id,
        baseRevision: 7,
        transcript: 'new human correction',
      }),
    );
  });

  it('drops a late draft hydration result after navigation to another segment', async () => {
    const first = inboxSegment('aaaaaaaa-1');
    const second = inboxSegment('bbbbbbbb-2');
    let resolveFirstDraft!: (value: {
      segmentId: string;
      baseRevision: number;
      text: string;
      updatedAt: string;
    }) => void;
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.getReviewDraftV1
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveFirstDraft = resolve;
        }),
      )
      .mockResolvedValueOnce(null);

    render(ReviewInbox);
    const listbox = await screen.findByRole('listbox', { name: 'Queue (2)' });
    const options = within(listbox).getAllByRole('option');
    await fireEvent.click(options[1]);
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));

    resolveFirstDraft({
      segmentId: first.id,
      baseRevision: 7,
      text: 'draft for the first row',
      updatedAt: '2026-08-26T00:00:00Z',
    });
    await Promise.resolve();

    expect(options[1]).toHaveAttribute('aria-selected', 'true');
    expect(screen.queryByDisplayValue('draft for the first row')).not.toBeInTheDocument();
  });

  it('recovers a matching durable draft into the editor without merging revisions', async () => {
    const row = inboxSegment('aaaaaaaa-1');
    api.getReviewPageV1.mockResolvedValue(reviewPage([row], { baseRevision: 7 }));
    api.getReviewDraftV1.mockResolvedValue({
      segmentId: row.id,
      baseRevision: 7,
      text: 'دەقی گەڕێندراوە',
      updatedAt: '2026-08-26T00:00:00Z',
    });

    render(ReviewInbox);

    expect(await screen.findByRole('textbox')).toHaveValue('دەقی گەڕێندراوە');
    expect(
      screen.getByText('Recovered your unsaved draft from this workstation.'),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toBeDisabled();
  });

  it('waits for the durable draft barrier before closing the inbox', async () => {
    const onClose = vi.fn();
    let resolveSave!: (value: {
      segmentId: string;
      baseRevision: number;
      text: string;
      updatedAt: string;
    }) => void;
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    api.saveReviewDraftV1.mockReturnValue(
      new Promise((resolve) => {
        resolveSave = resolve;
      }),
    );

    render(ReviewInbox, { onClose });
    await screen.findByText('Queue (1)');
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: 'durable correction' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Close inbox' }));

    await waitFor(() => expect(api.saveReviewDraftV1).toHaveBeenCalledTimes(1));
    expect(onClose).not.toHaveBeenCalled();
    resolveSave({
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      text: 'durable correction',
      updatedAt: '2026-08-26T00:00:00Z',
    });
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it('keeps the clip, editor and draft when an authoritative commit response is ambiguous', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1', true)]));
    api.commitReviewV1.mockRejectedValue(new Error('response lost'));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی پارێزراو' } });
    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.saveReviewDraftV1).toHaveBeenCalledWith('aaaaaaaa-1', 7, 'دەقی پارێزراو');
    expect(api.saveReviewDraftV1.mock.invocationCallOrder[0]).toBeLessThan(
      api.commitReviewV1.mock.invocationCallOrder[0],
    );
    expect(editor).toBeInTheDocument();
    expect(editor).toHaveValue('دەقی پارێزراو');
    expect(
      screen.getByText('Failed to save edit: An unknown review error occurred.'),
    ).toBeInTheDocument();
    expect(screen.queryByText(/response lost/)).not.toBeInTheDocument();
  });

  it('writes a committed decision back by id, never onto whatever now sits at the old index', async () => {
    // The queue can be REPLACED (a jury run reloads it) while a decision's IPC is in flight. Writing
    // the committed row back at a pre-await index then stamps it onto a DIFFERENT, undecided segment
    // — which the rail marks done and the reviewer never sees again — or, past the new end, punches
    // an `undefined` hole the rail's {#each} throws on. Fail-before: the rail shows 'aaaaaaaa' here.
    const first = inboxSegment('aaaaaaaa-1', true);
    const reloaded = inboxSegment('cccccccc-3');
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([first, inboxSegment('bbbbbbbb-2')]))
      .mockResolvedValueOnce(reviewPage([reloaded]));
    let commitDecision!: (value: {
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }) => void;
    api.commitReviewV1.mockReturnValue(
      new Promise((resolve) => {
        commitDecision = resolve;
      }),
    );
    api.getSegmentIdsForView.mockResolvedValue(['aaaaaaaa-1']);
    api.runJuryPipeline.mockResolvedValue({ t0AutoAccepted: 0, humanInbox: 1 });

    render(ReviewInbox);
    await screen.findByText('Queue (2)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();

    await fireEvent.click(screen.getByRole('button', { name: /Accept/ }));
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalled());
    expect(api.recordPlaybackReceipt).toHaveBeenCalledWith({
      playbackReceiptId: 'receipt-aaaaaaaa-1',
      mediaGrantId: MEDIA_GRANT_ID,
      intervals: [{ startMs: 0, endMs: 900 }],
    });
    expect(api.commitReviewV1).toHaveBeenCalledWith({
      operationId: expect.any(String),
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      decision: 'accept',
      transcript: 'دەق',
      reasonCode: null,
      playbackReceiptId: 'receipt-aaaaaaaa-1',
    });

    // The queue is reloaded (shorter, entirely different rows) while that decision is still in flight.
    await fireEvent.click(screen.getByRole('button', { name: /Run Jury/ }));
    await screen.findByText('Queue (1)');

    commitDecision({
      segmentId: first.id,
      committedRevision: 8,
      authoritativeTranscript: first.rawTranscript,
      decisionId: 'effect:7',
    });

    await waitFor(() => expect(screen.getByText('Accepted')).toBeInTheDocument());
    expect(screen.getByText('Queue (1)')).toBeInTheDocument();
    expect(screen.getByText('cccccccc…')).toBeInTheDocument();
    expect(screen.queryByText('aaaaaaaa…')).not.toBeInTheDocument();
  });

  it('aborts a correction commit when receipt finalization fails and preserves the typed draft', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1', true)]));
    api.recordPlaybackReceipt.mockRejectedValue(new Error('receipt store unavailable'));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });

    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));

    await waitFor(() => expect(api.recordPlaybackReceipt).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).not.toHaveBeenCalled();
    expect(editor).toHaveValue('دەقی ڕاستکراوە');
    expect(editor).toBeInTheDocument();
    expect(
      screen.getByText('Failed to save edit: An unknown review error occurred.'),
    ).toBeInTheDocument();
    expect(screen.queryByText(/receipt store unavailable/)).not.toBeInTheDocument();
  });

  it('reissues same-segment authority after a stale revision and never reuses the old receipt', async () => {
    const row = inboxSegment('aaaaaaaa-1', true);
    api.getReviewPageV1
      .mockResolvedValueOnce(reviewPage([row], { baseRevision: 7 }))
      .mockResolvedValueOnce(reviewPage([row], { baseRevision: 8 }));
    api.beginDesktopPlaybackSessionV1.mockImplementation(
      async (segmentId: string, _grant: string, expectedRevision: number) => ({
        playbackReceiptId: `receipt-${segmentId}-revision-${expectedRevision}`,
        segmentId,
        segmentRevision: expectedRevision,
        clipDurationMs: 1_000,
        expiresAtMs: Date.now() + 60_000,
      }),
    );
    api.recordPlaybackReceipt.mockImplementation(async ({ playbackReceiptId }) => ({
      playbackReceiptId,
      segmentId: row.id,
      segmentRevision: Number(playbackReceiptId.replace(/^.*-revision-/, '')),
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    }));
    api.commitReviewV1
      .mockRejectedValueOnce({
        schema: 1,
        code: 'STALE_REVISION',
        message: 'row changed',
        retryable: false,
      })
      .mockImplementationOnce(async (request) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        authoritativeTranscript: request.transcript ?? row.rawTranscript,
        decisionId: 'effect:808',
      }));

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() =>
      expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        row.id,
        MEDIA_GRANT_ID,
        7,
        expect.any(String),
      ),
    );
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

    await waitFor(() =>
      expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalledWith(
        row.id,
        MEDIA_GRANT_ID,
        8,
        expect.any(String),
      ),
    );
    const issuanceCalls = api.beginDesktopPlaybackSessionV1.mock.calls;
    expect(issuanceCalls[1][3]).not.toBe(issuanceCalls[0][3]);
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(2));
    expect(api.recordPlaybackReceipt.mock.calls[1][0]).toMatchObject({
      playbackReceiptId: 'receipt-aaaaaaaa-1-revision-8',
    });
    expect(api.commitReviewV1.mock.calls[1][0]).toMatchObject({
      baseRevision: 8,
      playbackReceiptId: 'receipt-aaaaaaaa-1-revision-8',
    });
  });

  it('uses the typed revision-bound commit for a correction', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1', true)], { baseRevision: 7 }),
    );

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    await fireEvent.input(screen.getByRole('textbox'), {
      target: { value: 'دەقی ڕاستکراوە' },
    });
    await fireEvent.click(screen.getByRole('button', { name: /Save edit/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).toHaveBeenCalledWith({
      operationId: expect.any(String),
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      decision: 'edit',
      transcript: 'دەقی ڕاستکراوە',
      reasonCode: null,
      playbackReceiptId: 'receipt-aaaaaaaa-1',
    });
  });

  it('uses the typed revision-bound commit for rejection', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1', true)], { baseRevision: 7 }),
    );

    render(ReviewInbox);
    await screen.findByText('Queue (1)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();
    await fireEvent.click(screen.getByRole('button', { name: /^X Reject$/ }));

    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalledTimes(1));
    expect(api.commitReviewV1).toHaveBeenCalledWith({
      operationId: expect.any(String),
      segmentId: 'aaaaaaaa-1',
      baseRevision: 7,
      decision: 'reject',
      transcript: null,
      reasonCode: null,
      playbackReceiptId: 'receipt-aaaaaaaa-1',
    });
  });

  it('fails closed when the typed page says the segment is not eligible', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1', true)], {
        eligible: false,
        disabledReason: 'TRANSCRIPT_NOT_READY',
      }),
    );
    render(ReviewInbox);
    await screen.findByText('Queue (1)');

    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    expect(accept).toBeDisabled();
    expect(accept).toHaveAccessibleDescription(
      'This segment is not eligible for a review decision. Reload the queue after its transcript is ready.',
    );
    expect(api.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('keeps a typed correction authoritative and flushes it before rail navigation', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([inboxSegment('aaaaaaaa-1'), inboxSegment('bbbbbbbb-2')]),
    );
    render(ReviewInbox);
    await screen.findByText('Queue (2)');

    await fireEvent.click(screen.getByRole('button', { name: /^E Edit$/ }));
    const editor = screen.getByRole('textbox');
    await fireEvent.input(editor, { target: { value: 'دەقی ڕاستکراوە' } });

    const accept = screen.getByRole('button', { name: /^A Accept$/ });
    expect(accept).toBeDisabled();
    expect(accept).toHaveAttribute('aria-describedby', 'inbox-accept-disabled-reason');
    expect(accept).toHaveAccessibleDescription(
      'Save the correction or explicitly discard it before accepting.',
    );
    for (const name of [/^E Edit$/, /^X Reject$/, /^F Flag$/, /Undo$/]) {
      const action = screen.getByRole('button', { name });
      expect(action).toBeDisabled();
      expect(action).toHaveAccessibleDescription(
        'Save or cancel the current correction before choosing another action.',
      );
    }
    await fireEvent.keyDown(window, { key: 'a', code: 'KeyA' });
    expect(api.commitReviewV1).not.toHaveBeenCalled();
    expect(editor).toHaveValue('دەقی ڕاستکراوە');

    const listbox = screen.getByRole('listbox', { name: 'Queue (2)' });
    const options = within(listbox).getAllByRole('option');
    await fireEvent.click(options[1]);
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    expect(api.saveReviewDraftV1).toHaveBeenCalledWith('aaaaaaaa-1', 7, 'دەقی ڕاستکراوە');
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('does not skip the row the reviewer navigated to while another commit was pending', async () => {
    const first = inboxSegment('aaaaaaaa-1', true);
    const second = inboxSegment('bbbbbbbb-2');
    let resolveDecision!: (value: {
      segmentId: string;
      committedRevision: number;
      authoritativeTranscript: string;
      decisionId: string;
    }) => void;
    api.getReviewPageV1.mockResolvedValue(reviewPage([first, second]));
    api.commitReviewV1.mockReturnValue(
      new Promise((resolve) => {
        resolveDecision = resolve;
      }),
    );
    render(ReviewInbox);
    await screen.findByText('Queue (2)');
    await waitFor(() => expect(api.beginDesktopPlaybackSessionV1).toHaveBeenCalled());
    await hearCurrentAudio();

    await fireEvent.click(screen.getByRole('button', { name: /^A Accept$/ }));
    await waitFor(() => expect(api.commitReviewV1).toHaveBeenCalled());
    expect(screen.getByRole('button', { name: /^A Accept$/ })).toHaveAccessibleDescription(
      'A review change is still being saved. Wait for it to finish.',
    );
    await fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    await waitFor(() => expect(screen.getByText('bbbbbbbb…')).toBeInTheDocument());

    resolveDecision({
      segmentId: first.id,
      committedRevision: 8,
      authoritativeTranscript: first.rawTranscript,
      decisionId: 'effect:9',
    });
    await waitFor(() => expect(screen.getByText('Accepted')).toBeInTheDocument());
    expect(screen.getByText('bbbbbbbb…').closest('[role="option"]')).toHaveAttribute(
      'aria-selected',
      'true',
    );
  });

  it('uses one composite listbox tab stop with an active descendant and deterministic focus', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([
        inboxSegment('aaaaaaaa-1'),
        inboxSegment('bbbbbbbb-2'),
        inboxSegment('cccccccc-3'),
      ]),
    );
    render(ReviewInbox);

    const listbox = await screen.findByRole('listbox', { name: 'Queue (3)' });
    const options = within(listbox).getAllByRole('option');
    expect(listbox).toHaveAttribute('tabindex', '0');
    expect(options).toHaveLength(3);
    expect(options.every((option) => option.getAttribute('tabindex') === '-1')).toBe(true);
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
    expect(listbox).toHaveAttribute('aria-activedescendant', options[0].id);

    listbox.focus();
    await fireEvent.keyDown(listbox, { key: 'ArrowDown', code: 'ArrowDown' });
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    expect(document.activeElement).toBe(listbox);
    expect(listbox).toHaveAttribute('aria-activedescendant', options[1].id);
    expect(screen.getByTestId('inbox-active-announcement')).toHaveTextContent(
      'Active segment 2 of 3',
    );

    await fireEvent.keyDown(listbox, { key: 'End', code: 'End' });
    expect(options[2]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.keyDown(listbox, { key: 'Home', code: 'Home' });
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
  });

  it('keeps the unified navigation and replay shortcuts while announcing the active item', async () => {
    api.getReviewPageV1.mockResolvedValue(
      reviewPage([
        inboxSegment('aaaaaaaa-1'),
        inboxSegment('bbbbbbbb-2'),
        inboxSegment('cccccccc-3'),
      ]),
    );
    render(ReviewInbox);

    const listbox = await screen.findByRole('listbox', { name: 'Queue (3)' });
    const options = within(listbox).getAllByRole('option');
    await fireEvent.keyDown(window, { key: 'n', code: 'KeyN' });
    await waitFor(() => expect(options[1]).toHaveAttribute('aria-selected', 'true'));
    expect(document.activeElement).toBe(listbox);

    await fireEvent.keyDown(window, { key: 'p', code: 'KeyP' });
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.keyDown(window, { key: 'ArrowDown', code: 'ArrowDown' });
    expect(options[1]).toHaveAttribute('aria-selected', 'true');
    await fireEvent.keyDown(window, { key: 'ArrowUp', code: 'ArrowUp' });
    expect(options[0]).toHaveAttribute('aria-selected', 'true');

    const replay = new KeyboardEvent('keydown', {
      key: 'r',
      code: 'KeyR',
      bubbles: true,
      cancelable: true,
    });
    listbox.dispatchEvent(replay);
    expect(replay.defaultPrevented).toBe(true);
    expect(options[0]).toHaveAttribute('aria-selected', 'true');
  });

  it('does not hijack native Space or Enter on a focused button', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    render(ReviewInbox);
    await screen.findByText('Queue (1)');

    const edit = screen.getByRole('button', { name: /^E Edit$/ });
    edit.focus();
    const space = new KeyboardEvent('keydown', {
      key: ' ',
      code: 'Space',
      bubbles: true,
      cancelable: true,
    });
    edit.dispatchEvent(space);
    expect(space.defaultPrevented).toBe(false);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();

    const enter = new KeyboardEvent('keydown', {
      key: 'Enter',
      code: 'Enter',
      bubbles: true,
      cancelable: true,
    });
    edit.dispatchEvent(enter);
    expect(enter.defaultPrevented).toBe(false);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(api.commitReviewV1).not.toHaveBeenCalled();
  });

  it('describes why unavailable actions are disabled', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    render(ReviewInbox);
    await screen.findByText('Queue (1)');

    const undo = screen.getByRole('button', { name: /Undo$/ });
    expect(undo).toBeDisabled();
    expect(undo).toHaveAttribute('aria-describedby', 'inbox-undo-disabled-reason');
    expect(undo).toHaveAccessibleDescription('There is no committed action to undo.');
  });

  it('marks failed media only with an explicit reason, reuses uncertain operation identity, and advances on typed success', async () => {
    const failed = inboxSegment('unusable-1', true);
    const next = inboxSegment('nextclip-2', true);
    api.getReviewPageV1.mockResolvedValue(reviewPage([failed, next], { baseRevision: 7 }));
    api.registerReviewMediaAsset.mockImplementation(async (path: string) => {
      if (path.includes('unusable-1')) throw new Error('file missing');
      return { id: NEXT_MEDIA_GRANT_ID };
    });
    api.markSegmentUnusableV1
      .mockRejectedValueOnce(new Error('transport response lost'))
      .mockResolvedValueOnce({
        segmentId: failed.id,
        committedRevision: 8,
        reason: 'corruptContainer',
        effectId: 'not-a-flag-effect',
      })
      .mockImplementationOnce(async (request) => ({
        segmentId: request.segmentId,
        committedRevision: request.baseRevision + 1,
        reason: request.reason,
        effectId: 'flag-effect:303',
      }));

    render(ReviewInbox);
    expect(await screen.findByTestId('inbox-technical-unusable')).toBeInTheDocument();
    expect(screen.getByText('unusable-1.wav')).toBeInTheDocument();
    const reason = screen.getByLabelText('Technical reason');
    const mark = screen.getByRole('button', { name: 'Mark technically unusable' });
    expect(mark).toBeDisabled();
    reason.focus();
    await fireEvent.keyDown(reason, { key: 'ArrowDown', code: 'ArrowDown' });
    expect(screen.getByText('unusable-1.wav')).toBeInTheDocument();
    expect(screen.queryByText('nextclip-2.wav')).not.toBeInTheDocument();
    await fireEvent.change(reason, { target: { value: 'corruptContainer' } });
    expect(mark).toBeEnabled();

    mark.focus();
    await fireEvent.click(mark);
    await waitFor(() => expect(api.markSegmentUnusableV1).toHaveBeenCalledTimes(1));
    expect(screen.getByText('unusable-1.wav')).toBeInTheDocument();
    expect(reason).toHaveValue('corruptContainer');
    expect(document.activeElement).toBe(mark);
    expect(api.recordPlaybackReceipt).not.toHaveBeenCalled();
    expect(api.commitReviewV1).not.toHaveBeenCalled();

    await fireEvent.click(mark);
    await waitFor(() => expect(api.markSegmentUnusableV1).toHaveBeenCalledTimes(2));
    expect(screen.getByText('unusable-1.wav')).toBeInTheDocument();
    expect(document.activeElement).toBe(mark);

    await fireEvent.click(mark);
    await waitFor(() => expect(api.markSegmentUnusableV1).toHaveBeenCalledTimes(3));
    const first = api.markSegmentUnusableV1.mock.calls[0][0];
    const replay = api.markSegmentUnusableV1.mock.calls[1][0];
    const verifiedRetry = api.markSegmentUnusableV1.mock.calls[2][0];
    expect(first).toEqual({
      operationId: expect.stringMatching(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      ),
      segmentId: failed.id,
      baseRevision: 7,
      reason: 'corruptContainer',
    });
    expect(replay).toEqual(first);
    expect(verifiedRetry).toEqual(first);
    await waitFor(() => expect(screen.getByText('nextclip-2.wav')).toBeInTheDocument());
    expect(screen.queryByText('unusable-1.wav')).not.toBeInTheDocument();
  });

  it('localizes queue, audio, duration, and active-item accessibility text in Sorani', async () => {
    locale.set('ckb');
    api.getReviewPageV1.mockResolvedValue(reviewPage([inboxSegment('aaaaaaaa-1')]));
    render(ReviewInbox);

    const listbox = await screen.findByRole('listbox', { name: 'ڕیز (1)' });
    expect(within(listbox).getByRole('option')).toHaveAccessibleName('پارچەی 1 لە 1: aaaaaaaa-1');
    expect(screen.getByLabelText('لێدانی دەنگ')).toBeInTheDocument();
    expect(screen.getByText('دەنگ بەردەست نییە')).toBeInTheDocument();
    expect(screen.getByText('1 چرکە')).toBeInTheDocument();
    expect(screen.queryByLabelText('Audio playback')).not.toBeInTheDocument();
  });

  it('exposes the selected autonomy level as a pressed button', async () => {
    api.getReviewPageV1.mockResolvedValue(reviewPage([]));
    render(ReviewInbox);
    await screen.findByText('Inbox zero!');

    const group = screen.getByRole('group', { name: 'Autonomy level' });
    const propose = within(group).getByRole('button', { name: /Propose/ });
    const observe = within(group).getByRole('button', { name: /Observe/ });
    expect(propose).toHaveAttribute('aria-pressed', 'true');
    expect(observe).toHaveAttribute('aria-pressed', 'false');

    await fireEvent.click(observe);
    expect(observe).toHaveAttribute('aria-pressed', 'true');
    expect(propose).toHaveAttribute('aria-pressed', 'false');
  });
});

describe('ReviewInbox locale contract', () => {
  it('keeps every inbox translation key in exact English/Sorani parity', () => {
    const inboxKeys = (messages: Record<string, string>) =>
      Object.keys(messages)
        .filter((key) => key.startsWith('inbox.'))
        .sort();
    expect(inboxKeys(ckb)).toEqual(inboxKeys(en));
  });
});
