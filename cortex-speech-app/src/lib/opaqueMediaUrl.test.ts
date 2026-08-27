import { describe, expect, it } from 'vitest';
import { validatedOpaqueMediaUrl } from './opaqueMediaUrl';

const ID = '2f2d9b66-8566-4d1c-8c14-e18d006b776f';

describe('opaque media URL boundary', () => {
  it('accepts only exact Windows and native custom-protocol grant URLs', () => {
    expect(validatedOpaqueMediaUrl(`http://cortex-media.localhost/${ID}`, false)).toBe(
      `http://cortex-media.localhost/${ID}`,
    );
    expect(validatedOpaqueMediaUrl(`cortex-media://localhost/${ID}`, false)).toBe(
      `cortex-media://localhost/${ID}`,
    );
  });

  it.each([
    `file:///Z:/fixture/private.wav`,
    `http://example.com/${ID}`,
    `http://cortex-media.localhost/C:/private.wav`,
    `http://cortex-media.localhost/${ID}?path=C:/private.wav`,
    `http://cortex-media.localhost/${ID}#fragment`,
    `http://user@cortex-media.localhost/${ID}`,
    `http://cortex-media.localhost:80/${ID}`,
    `http://cortex-media.localhost/not-a-uuid`,
    `data:audio/wav;base64,AAAA`,
  ])('rejects non-opaque production media URL %s', (url) => {
    expect(validatedOpaqueMediaUrl(url, false)).toBeNull();
  });

  it('allows the fixed WAV data fixture only in explicit development mode', () => {
    const fixture = 'data:audio/wav;base64,UklGRg==';
    expect(validatedOpaqueMediaUrl(fixture, true)).toBe(fixture);
    expect(validatedOpaqueMediaUrl(fixture, false)).toBeNull();
    expect(validatedOpaqueMediaUrl('data:text/html;base64,AAAA', true)).toBeNull();
  });
});
