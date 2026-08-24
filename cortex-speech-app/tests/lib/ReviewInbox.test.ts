import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  getEscalationQueue: vi.fn(),
  getSettings: vi.fn(),
  getSegmentIdsForView: vi.fn(),
  runJuryPipeline: vi.fn(),
  recordPlaybackReceipt: vi.fn(),
  recordHumanDecision: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => api);

import ReviewInbox from '../../src/lib/ReviewInbox.svelte';
import { locale } from '../../src/lib/i18n';
import type { SpeechSegment } from '../../src/lib/types';

function inboxSegment(id: string): SpeechSegment {
  return {
    id,
    audioPath: '', // no player: this test is about the queue write-back, not playback evidence
    rawTranscript: 'دەق',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 1000,
    speakerId: null,
    verified: false,
  };
}

describe('ReviewInbox queue loading', () => {
  beforeEach(() => {
    api.getEscalationQueue.mockReset();
    api.getSettings.mockReset();
    api.getSegmentIdsForView.mockReset();
    api.runJuryPipeline.mockReset();
    api.recordPlaybackReceipt.mockReset();
    api.recordHumanDecision.mockReset();
    api.getSettings.mockResolvedValue(null);
    api.recordPlaybackReceipt.mockResolvedValue(undefined);
    locale.set('en');
  });

  afterEach(cleanup);

  it('shows a retryable error instead of claiming an unread queue is empty', async () => {
    api.getEscalationQueue.mockRejectedValueOnce(new Error('database unavailable'));
    api.getEscalationQueue.mockResolvedValueOnce([]);

    render(ReviewInbox);

    const alert = await screen.findByTestId('review-inbox-load-error');
    expect(alert).toHaveTextContent('Could not load the review queue');
    expect(alert).toHaveTextContent('database unavailable');
    expect(screen.queryByText('Inbox zero!')).not.toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: 'Review Inbox' })).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() =>
      expect(screen.queryByTestId('review-inbox-load-error')).not.toBeInTheDocument(),
    );
    expect(await screen.findByText('Inbox zero!')).toBeInTheDocument();
  });

  it('writes a committed decision back by id, never onto whatever now sits at the old index', async () => {
    // The queue can be REPLACED (a jury run reloads it) while a decision's IPC is in flight. Writing
    // the committed row back at a pre-await index then stamps it onto a DIFFERENT, undecided segment
    // — which the rail marks done and the reviewer never sees again — or, past the new end, punches
    // an `undefined` hole the rail's {#each} throws on. Fail-before: the rail shows 'aaaaaaaa' here.
    const first = inboxSegment('aaaaaaaa-1');
    const reloaded = inboxSegment('cccccccc-3');
    api.getEscalationQueue
      .mockResolvedValueOnce([first, inboxSegment('bbbbbbbb-2')])
      .mockResolvedValueOnce([reloaded]);
    let commitDecision!: (value: { effectEventId: number; segment: SpeechSegment }) => void;
    api.recordHumanDecision.mockReturnValue(
      new Promise((resolve) => {
        commitDecision = resolve;
      }),
    );
    api.getSegmentIdsForView.mockResolvedValue(['aaaaaaaa-1']);
    api.runJuryPipeline.mockResolvedValue({ t0AutoAccepted: 0, humanInbox: 1 });

    render(ReviewInbox);
    await screen.findByText('Queue (2)');

    await fireEvent.click(screen.getByRole('button', { name: /Accept/ }));
    await waitFor(() =>
      expect(api.recordHumanDecision).toHaveBeenCalledWith('aaaaaaaa-1', 'accept', null),
    );

    // The queue is reloaded (shorter, entirely different rows) while that decision is still in flight.
    await fireEvent.click(screen.getByRole('button', { name: /Run Jury/ }));
    await screen.findByText('Queue (1)');

    commitDecision({
      effectEventId: 7,
      segment: { ...first, humanDecision: 'accept', verified: true },
    });

    await waitFor(() => expect(screen.getByText('✅ Accepted')).toBeInTheDocument());
    expect(screen.getByText('Queue (1)')).toBeInTheDocument();
    expect(screen.getByText('cccccccc…')).toBeInTheDocument();
    expect(screen.queryByText('aaaaaaaa…')).not.toBeInTheDocument();
  });

  it('exposes the selected autonomy level as a pressed button', async () => {
    api.getEscalationQueue.mockResolvedValue([]);
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
