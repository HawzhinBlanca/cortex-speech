import { createRequire } from 'node:module';
import { describe, expect, it } from 'vitest';

const require = createRequire(import.meta.url);
const uri = require('fast-uri') as { resolve(base: string, reference: string): string };
const qs = require('qs') as {
  parse(input: string, options: Record<string, unknown>): Record<string, unknown>;
  stringify(input: Record<string, unknown>): string;
};

// These exercise build-tool transitive dependencies, not the Rust reviewer HTTP server.
// Maintainer advisories: GHSA-5jgf-p345-68v8, GHSA-x5fp-wj9c-mxmx, GHSA-4mjr-xmp4-gh2g.
describe('locked tooling dependencies retain their security fixes', () => {
  it('canonicalizes an international hostname after resolving a scheme-relative URL', () => {
    const base = 'https://example.test/base';
    const reference = '//b\u00fccher.example/audio';
    expect(uri.resolve(base, reference)).toBe(new URL(reference, base).href);
    expect(uri.resolve(base, '../safe')).toBe('https://example.test/safe');
  });

  it('enforces comma array limits for bracket keys as well as plain keys', () => {
    const options = { comma: true, arrayLimit: 3, throwOnLimitExceeded: true };
    expect(() => qs.parse('clip[]=1,2,3,4', options)).toThrow(RangeError);
    expect(() => qs.parse('clip=1,2,3,4', options)).toThrow(RangeError);
    expect(qs.parse('clip=1,2,3', options)).toEqual({ clip: ['1', '2', '3'] });
  });

  it('safely serializes parsed data with an untrusted constructor.isBuffer property', () => {
    const input = qs.parse('clip[constructor][isBuffer]=not-a-function', { plainObjects: true });
    expect(() => qs.stringify(input)).not.toThrow();
    expect(qs.stringify({ clip: 'safe' })).toBe('clip=safe');
  });
});
