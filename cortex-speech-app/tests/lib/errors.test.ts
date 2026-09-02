import { describe, it, expect, beforeEach } from 'vitest';
import { parseActionableError } from '../../src/lib/errors';
import { locale } from '../../src/lib/i18n';

describe('parseActionableError', () => {
  beforeEach(() => locale.set('en'));

  it('never surfaces the literal string "undefined" / "null" for a nullish error', () => {
    // Regression: a resource 'error' event carries no message, so the boundary used to call
    // String(undefined) === "undefined" and render that to the user.
    for (const bad of [undefined, null, '']) {
      const out = parseActionableError(bad);
      expect(out.message).toBeTruthy();
      expect(out.message).not.toBe('undefined');
      expect(out.message).not.toBe('null');
    }
  });

  it('does not expose a real Error message', () => {
    const out = parseActionableError(new Error('disk I/O error'));
    expect(out.message).toContain('unexpected error');
    expect(out.message).not.toContain('disk I/O');
    expect(out.detail).toBeUndefined();
  });

  it('does not expose a plain backend string', () => {
    const out = parseActionableError('something specific failed');
    expect(out.message).toContain('unexpected error');
    expect(out.message).not.toContain('something specific');
  });

  it('stays actionable when coercion hooks throw', () => {
    const hostile = new Proxy(
      {},
      {
        getPrototypeOf: () => {
          throw new Error('hostile prototype');
        },
        get: () => {
          throw new Error('hostile property');
        },
      },
    );

    expect(() => parseActionableError(hostile)).not.toThrow();
    expect(parseActionableError(hostile).message).toContain('unexpected error');
  });

  it('retains typed code, operation ID and suggested action without the backend message', () => {
    const out = parseActionableError({
      schema: 1,
      code: 'MODEL_UNAVAILABLE',
      message: 'model missing at C:\\private\\models',
      retryable: true,
      suggestedAction: 'openModels',
      operationId: '018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10',
    });

    expect(out.code).toBe('MODEL_UNAVAILABLE');
    expect(out.operationId).toBe('018f6e4a-2d71-4c66-8e4b-9d3c4b7e5a10');
    expect(out.suggestedAction).toBe('openModels');
    expect(out.action?.handler).toBeTypeOf('function');
    expect(out.detail).not.toContain('private');
  });
});
