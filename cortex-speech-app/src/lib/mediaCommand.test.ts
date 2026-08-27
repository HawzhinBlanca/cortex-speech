import { invoke } from '@tauri-apps/api/core';
import { describe, expect, expectTypeOf, it, vi } from 'vitest';
import { getMediaAssetUrl, registerMediaAsset, registerReviewMediaAsset } from './commands';
import type { MediaGrant } from './generated/ipc';

const invokeMock = vi.mocked(invoke);
const ORDINARY_GRANT_ID = '2f2d9b66-8566-4d1c-8c14-e18d006b776f';
const REVIEW_GRANT_ID = '52a492d4-14d8-4e24-9f5d-bc44221b48c1';

describe('opaque media IPC contract', () => {
  it('returns no filesystem path and preserves the exact generated arguments', async () => {
    invokeMock.mockReset();
    const ordinary: MediaGrant = { id: ORDINARY_GRANT_ID, expiresAt: '2026-08-27T00:00:00Z' };
    const review: MediaGrant = { id: REVIEW_GRANT_ID, expiresAt: '2026-08-27T00:01:00Z' };
    const url = `http://cortex-media.localhost/${REVIEW_GRANT_ID}`;
    invokeMock
      .mockResolvedValueOnce(ordinary)
      .mockResolvedValueOnce(review)
      .mockResolvedValueOnce(url);

    const ordinaryResult = registerMediaAsset('D:/private/source.wav');
    const reviewResult = registerReviewMediaAsset('D:/private/review.wav');
    expectTypeOf(ordinaryResult).toEqualTypeOf<Promise<MediaGrant>>();
    await expect(ordinaryResult).resolves.toEqual(ordinary);
    await expect(reviewResult).resolves.toEqual(review);
    await expect(getMediaAssetUrl(review.id)).resolves.toBe(url);

    expect(ordinary).not.toHaveProperty('path');
    expect(review).not.toHaveProperty('path');
    expect(invokeMock.mock.calls).toEqual([
      ['register_media_asset', { audioPath: 'D:/private/source.wav' }],
      ['register_review_media_asset', { audioPath: 'D:/private/review.wav' }],
      ['get_media_asset_url', { id: REVIEW_GRANT_ID }],
    ]);
  });

  it('propagates the structured reload refusal without stringifying it', async () => {
    invokeMock.mockReset();
    const refusal = {
      schema: 1,
      code: 'MEDIA_ASSET_UNAVAILABLE',
      message: 'This audio clip is unavailable. Reload the clip and retry.',
      retryable: false,
      suggestedAction: 'reloadClip',
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(registerMediaAsset('D:/private/missing.wav')).rejects.toEqual(refusal);
  });
});
