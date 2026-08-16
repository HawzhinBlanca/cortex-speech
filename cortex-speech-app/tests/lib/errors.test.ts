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

  it('passes a real Error message through', () => {
    const out = parseActionableError(new Error('disk I/O error'));
    expect(out.message).toContain('disk I/O error');
  });

  it('passes a plain string through', () => {
    const out = parseActionableError('something specific failed');
    expect(out.message).toBe('something specific failed');
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
    expect(parseActionableError(hostile).message).toBe('Unknown error');
  });
});
