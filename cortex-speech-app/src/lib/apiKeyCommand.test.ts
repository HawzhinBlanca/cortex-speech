import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { getConfiguredProviders, setApiKey } from './commands';

const invokeMock = vi.mocked(invoke);

describe('API-key IPC contract', () => {
  it('uses generated provider-only status and closed secret mutation commands', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce(['gemini']).mockResolvedValueOnce(['gemini', 'openrouter']);

    await expect(getConfiguredProviders()).resolves.toEqual(['gemini']);
    await expect(setApiKey('openrouter', 'private-value')).resolves.toEqual([
      'gemini',
      'openrouter',
    ]);

    expect(invokeMock.mock.calls).toEqual([
      ['get_configured_providers'],
      ['set_api_key', { provider: 'openrouter', key: 'private-value' }],
    ]);
  });

  it('propagates only the backend structured refusal', async () => {
    invokeMock.mockReset();
    const refusal = {
      schema: 1,
      code: 'API_KEY_SAVE_FAILED',
      message: 'The API key could not be saved safely. Open Health for recovery options.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(setApiKey('gemini', 'never-echo-this')).rejects.toEqual(refusal);
    expect(JSON.stringify(refusal)).not.toContain('never-echo-this');
  });
});
