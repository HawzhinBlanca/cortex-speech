import { describe, expect, it } from 'vitest';
import { formatPublicErrorReference, publicErrorReference } from '../../src/lib/errorText';

const operationId = '018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10';

describe('public error boundary', () => {
  it('preserves only typed recovery metadata and excludes backend prose', () => {
    const failure = {
      schema: 1,
      code: 'PAY_POLICY_REQUIRED',
      message: 'SQL failed at C:\\private\\secret.db\nstack: internal()',
      retryable: false,
      suggestedAction: 'openModels',
      operationId,
      details: { query: 'select * from decisions' },
    };

    expect(publicErrorReference(failure)).toEqual({
      code: 'PAY_POLICY_REQUIRED',
      operationId,
      retryable: false,
      suggestedAction: 'openModels',
    });
    const rendered = formatPublicErrorReference(failure);
    expect(rendered).toBe(`PAY_POLICY_REQUIRED · ${operationId}`);
    expect(rendered).not.toMatch(/SQL|Users|secret|stack|select/i);
  });

  it('accepts a serialized typed error without exposing its message', () => {
    const value = JSON.stringify({
      schema: 1,
      code: 'STALE_REVISION',
      message: 'row at C:\\private\\db.sqlite is stale',
      retryable: true,
      suggestedAction: 'reloadClip',
      operationId,
    });

    expect(publicErrorReference(value)).toMatchObject({
      code: 'STALE_REVISION',
      operationId,
      suggestedAction: 'reloadClip',
    });
    expect(formatPublicErrorReference(value)).not.toContain('private');
  });

  it('fails closed for invalid schema, identifiers, actions and operation IDs', () => {
    expect(
      publicErrorReference({
        schema: 2,
        code: 'VALID_CODE',
        retryable: true,
        suggestedAction: 'retry',
        operationId,
      }),
    ).toEqual({});
    expect(
      publicErrorReference({
        schema: 1,
        code: '../../secret',
        retryable: 'yes',
        suggestedAction: 'runSql',
        operationId: 'C:\\private',
      }),
    ).toEqual({});
  });

  it('retains only a legacy E_* token and drops surrounding prose', () => {
    const rendered = formatPublicErrorReference(
      'write failed E_DATABASE_LOCKED: C:\\private\\library.db; SQL stack follows',
    );
    expect(rendered).toBe('E_DATABASE_LOCKED');
    expect(publicErrorReference(new Error('E_FILE_PICKER_CANCELLED: private detail'))).toEqual({
      code: 'E_FILE_PICKER_CANCELLED',
    });
  });

  it('is total for hostile values and bounded for every output', () => {
    const hostile = new Proxy(
      {},
      {
        get: () => {
          throw new Error('blocked');
        },
      },
    );

    expect(() => publicErrorReference(hostile)).not.toThrow();
    expect(publicErrorReference(hostile)).toEqual({});
    expect(formatPublicErrorReference({ schema: 1, code: 'A'.repeat(64) }, 12)).toHaveLength(12);
  });
});
