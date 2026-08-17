import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  getEscalationQueue: vi.fn(),
  getSettings: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => api);

import ReviewInbox from '../../src/lib/ReviewInbox.svelte';
import { locale } from '../../src/lib/i18n';

describe('ReviewInbox queue loading', () => {
  beforeEach(() => {
    api.getEscalationQueue.mockReset();
    api.getSettings.mockReset();
    api.getSettings.mockResolvedValue(null);
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
