import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { restoreSession, saveSession } from './commands';

const invokeMock = vi.mocked(invoke);

describe('session IPC contract', () => {
  it('uses generated persistence arguments and returns the generated snapshot', async () => {
    invokeMock.mockReset();
    const snapshot = {
      selected_segment_id: 'segment-a',
      filter_verified: false,
      search_query: 'query',
      sort_order: 'newest',
      segment_count: 12,
      verified_count: 3,
    };
    invokeMock.mockResolvedValueOnce(undefined).mockResolvedValueOnce(snapshot);

    await saveSession('query', 'newest', false);
    await expect(restoreSession()).resolves.toEqual(snapshot);

    expect(invokeMock.mock.calls).toEqual([
      ['save_session', { searchQuery: 'query', sortOrder: 'newest', filterVerified: false }],
      ['restore_session'],
    ]);
  });

  it('propagates a structured persistence refusal', async () => {
    invokeMock.mockReset();
    const refusal = {
      schema: 1,
      code: 'SESSION_SAVE_FAILED',
      message: 'The workspace view could not be saved. Open Health for recovery options.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(saveSession('', 'newest', null)).rejects.toEqual(refusal);
  });
});
