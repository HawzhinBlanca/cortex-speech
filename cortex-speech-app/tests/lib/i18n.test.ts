import { describe, it, expect } from 'vitest';
import { get } from 'svelte/store';
import { t, locale } from '../../src/lib/i18n';
import { en } from '../../src/lib/i18n/en';
import { ckb } from '../../src/lib/i18n/ckb';

describe('i18n English fallback', () => {
  it('renders the English string for keys missing in the ckb dictionary', () => {
    locale.set('ckb');
    const translate = get(t);
    const enOnly = Object.keys(en).filter((k) => !(k in ckb));
    for (const key of enOnly) {
      expect(translate(key)).toBe(en[key]);
    }
  });

  it('never shows a raw key string under the ckb locale for any known en key', () => {
    locale.set('ckb');
    const translate = get(t);
    for (const key of Object.keys(en)) {
      expect(translate(key)).not.toBe(key);
    }
  });

  it('still substitutes params on a fallen-back string', () => {
    locale.set('ckb');
    const translate = get(t);
    // openFile.multiChunk uses a {count} param and exists in en.
    expect(translate('openFile.multiChunk', { count: '3' })).toContain('3');
  });
});
