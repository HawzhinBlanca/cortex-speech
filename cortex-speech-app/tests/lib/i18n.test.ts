import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
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

describe('signal-anomaly screen is honestly labeled (audit P1 #7 / honesty law)', () => {
  // Backend signal_anomaly.rs is a ZCR/energy-variance HEURISTIC — the fabricated WavLM "OOD" path was removed for
  // overclaiming. The UI must not present it as a trained out-of-distribution DETECTOR.
  it('English labels call it a heuristic anomaly screen, not an OOD detector', () => {
    expect(en['validation.signalAnomaly.title'].toLowerCase()).toContain('heuristic');
    expect(en['validation.signalAnomaly.title'].toLowerCase()).not.toContain('detector');
    expect(en['validation.tab.signalAnomaly'].toLowerCase()).toContain('anomaly');
    expect(en['validation.signalAnomaly.description'].toLowerCase()).toContain('not a trained');
    // The per-segment verdict/score no longer assert "out of distribution".
    expect(en['validation.signalAnomaly.isSignalAnomaly'].toLowerCase()).not.toContain('distribution');
    expect(en['validation.signalAnomaly.score'].toLowerCase()).not.toContain('ood');
  });

  it('the ckb dictionary carries every ood label (no silent English fallback for this surface)', () => {
    const signalAnomalyKeys = Object.keys(en).filter((k) => k.startsWith('validation.signalAnomaly.') || k === 'validation.tab.signalAnomaly');
    expect(signalAnomalyKeys.length).toBeGreaterThan(4);
    for (const key of signalAnomalyKeys) {
      expect(ckb[key], `ckb missing ${key}`).toBeTruthy();
      // ckb must not fall back to advertising a learned "OOD" detector either.
      expect(ckb[key]).not.toContain('OOD');
    }
  });

  it('ValidationPanel renders no HARDCODED user-facing "OOD" label — it must route through $t', () => {
    // The dictionary checks above cannot see hardcoded component text. Adversarial review found two live
    // un-localized "OOD" strings the relabel first missed (`<span>OOD: {score}</span>` and the "OOD
    // flagged" checkbox), which stayed English even under the Sorani build. Pin them out at the source.
    // vitest runs from the app root; resolve the component relative to it (import.meta.url is not a
    // file: scheme under vite).
    const panel = readFileSync(resolve(process.cwd(), 'src/lib/ValidationPanel.svelte'), 'utf-8');
    expect(panel, 'the "(not just OOD flagged)" checkbox must be localized').not.toContain('OOD flagged');
    expect(panel, 'the per-segment "OOD: {score}" label must be localized').not.toMatch(/>\s*OOD:/);
  });
});
