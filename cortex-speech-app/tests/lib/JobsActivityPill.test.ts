import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { invoke } from '@tauri-apps/api/core';
import JobsActivityPill from '../../src/lib/JobsActivityPill.svelte';
import { locale } from '../../src/lib/i18n';

const invokeMock = vi.mocked(invoke);

function job(overrides: Record<string, unknown> = {}) {
  return {
    id: 'j1',
    kind: 'export_dataset',
    state: 'succeeded',
    progress: 1,
    completed: 0,
    total: null,
    errorCode: null,
    ...overrides,
  };
}

describe('JobsActivityPill', () => {
  beforeEach(() => {
    locale.set('en');
    invokeMock.mockReset();
  });
  afterEach(() => {
    locale.set('ckb');
    vi.restoreAllMocks();
  });

  it('renders nothing when nothing is running and nothing recently failed', async () => {
    invokeMock.mockResolvedValue([job({ state: 'succeeded' })]);
    render(JobsActivityPill);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('get_jobs'));
    // A succeeded-only history is not worth a pill — the header stays quiet.
    expect(screen.queryByTestId('jobs-activity')).not.toBeInTheDocument();
  });

  it('shows a running count while work is in progress', async () => {
    invokeMock.mockResolvedValue([job({ id: 'a', state: 'running' }), job({ id: 'b', state: 'running' })]);
    render(JobsActivityPill);
    await waitFor(() => expect(screen.getByTestId('jobs-activity')).toBeInTheDocument());
    expect(screen.getByText('2 running')).toBeInTheDocument();
    expect(screen.getByTestId('jobs-activity')).toHaveAttribute('data-mode', 'running');
  });

  it('flags a recent failure', async () => {
    invokeMock.mockResolvedValue([job({ state: 'failed', errorCode: 'EXPORT_FAILED' })]);
    render(JobsActivityPill);
    await waitFor(() => expect(screen.getByText('A task failed')).toBeInTheDocument());
    expect(screen.getByTestId('jobs-activity')).toHaveAttribute('data-mode', 'failed');
  });

  it('distinguishes a crash-interrupted task from an ordinary failure', async () => {
    invokeMock.mockResolvedValue([job({ state: 'failed', errorCode: 'INTERRUPTED' })]);
    render(JobsActivityPill);
    await waitFor(() => expect(screen.getByText('A task was interrupted')).toBeInTheDocument());
    expect(screen.getByTestId('jobs-activity')).toHaveAttribute('data-mode', 'interrupted');
  });

  it('prioritizes running work over a past failure', async () => {
    // get_jobs is newest-first; a fresh running job outranks an older failed one.
    invokeMock.mockResolvedValue([
      job({ id: 'new', state: 'running' }),
      job({ id: 'old', state: 'failed', errorCode: 'EXPORT_FAILED' }),
    ]);
    render(JobsActivityPill);
    await waitFor(() => expect(screen.getByTestId('jobs-activity')).toHaveAttribute('data-mode', 'running'));
    expect(screen.getByText('1 running')).toBeInTheDocument();
  });

  it('does not nag about an old failure once a newer job succeeded', async () => {
    // Newest-first: a succeeded job is the most recent, an older one failed. The failure is stale news,
    // so the pill self-clears rather than standing as a permanent alarm.
    invokeMock.mockResolvedValue([
      job({ id: 'new', state: 'succeeded' }),
      job({ id: 'old', state: 'failed', errorCode: 'EXPORT_FAILED' }),
    ]);
    render(JobsActivityPill);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('get_jobs'));
    expect(screen.queryByTestId('jobs-activity')).not.toBeInTheDocument();
  });

  it('flags a failure that is the newest terminal outcome even under a not-yet-started queued job', async () => {
    // Newest is queued (not running), the newest TERMINAL outcome is a failure — surface it.
    invokeMock.mockResolvedValue([
      job({ id: 'q', state: 'queued' }),
      job({ id: 'f', state: 'failed', errorCode: 'EXPORT_FAILED' }),
    ]);
    render(JobsActivityPill);
    await waitFor(() => expect(screen.getByTestId('jobs-activity')).toHaveAttribute('data-mode', 'failed'));
  });

  it('never throws to the UI when the jobs read fails', async () => {
    invokeMock.mockRejectedValue(new Error('db locked'));
    render(JobsActivityPill);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('get_jobs'));
    // No crash, and no pill (empty state).
    expect(screen.queryByTestId('jobs-activity')).not.toBeInTheDocument();
  });

  it('tolerates a null/non-array response without throwing (trust boundary)', async () => {
    // A generic app-runtime mock may resolve every invoke to null; the derived .filter must not blow up.
    invokeMock.mockResolvedValue(null);
    render(JobsActivityPill);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith('get_jobs'));
    expect(screen.queryByTestId('jobs-activity')).not.toBeInTheDocument();
  });
});
