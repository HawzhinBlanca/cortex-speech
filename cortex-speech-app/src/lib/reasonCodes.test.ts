import { describe, expect, it } from 'vitest';
import {
  KNOWN_REASON_CODES,
  parseEscalationEvidence,
  reasonLabelKey,
  reasonTone,
} from './reasonCodes';
import { en } from './i18n/en';
import { ckb } from './i18n/ckb';

describe('parseEscalationEvidence', () => {
  it('returns null when there is no decision record, so the UI renders nothing', () => {
    // Each of these means "this row has no reason to show" — NOT "escalated for no reason". Rendering
    // an empty reasons row would assert something the data does not support.
    expect(parseEscalationEvidence(null)).toBeNull();
    expect(parseEscalationEvidence(undefined)).toBeNull();
    expect(parseEscalationEvidence('')).toBeNull();
    expect(parseEscalationEvidence('   ')).toBeNull();
    expect(parseEscalationEvidence('{}')).toBeNull(); // a jury row with other evidence, no codes
    expect(parseEscalationEvidence('{"reasonCodes":[]}')).toBeNull();
  });

  it('never breaks the review screen on malformed evidence', () => {
    // A corrupt blob must not throw into the render path — a reviewer losing the whole screen because
    // one row has bad JSON is far worse than that row showing no reasons.
    expect(parseEscalationEvidence('not json at all')).toBeNull();
    expect(parseEscalationEvidence('[1,2,3]')).toBeNull();
    expect(parseEscalationEvidence('null')).toBeNull();
    expect(parseEscalationEvidence('{"reasonCodes":"low_snr"}')).toBeNull();
    expect(parseEscalationEvidence('{"reasonCodes":[1,2]}')).toBeNull();
  });

  it('preserves order and carries the policy version', () => {
    const ev = parseEscalationEvidence(
      '{"reasonCodes":["low_snr","clipping"],"policyVersion":"t0-2026-08-07"}',
    );
    // Order is the backend's, not re-sorted: it records the order the vetoes fired.
    expect(ev?.reasonCodes).toEqual(['low_snr', 'clipping']);
    expect(ev?.policyVersion).toBe('t0-2026-08-07');
  });

  it('tolerates a missing policy version rather than dropping the codes', () => {
    const ev = parseEscalationEvidence('{"reasonCodes":["policy_hold"]}');
    expect(ev?.reasonCodes).toEqual(['policy_hold']);
    expect(ev?.policyVersion).toBeNull();
  });

  it('PRESERVES codes this build does not know, instead of silently dropping them', () => {
    // The failure this guards: the backend gains a tenth code, this file has not caught up, and the
    // reviewer sees a shorter list with no indication anything is missing. A raw string in the UI is a
    // prompt to add a translation; a dropped code is invisible and stays that way.
    const ev = parseEscalationEvidence('{"reasonCodes":["low_snr","brand_new_code"]}');
    expect(ev?.reasonCodes).toEqual(['low_snr', 'brand_new_code']);
    expect(reasonLabelKey('brand_new_code')).toBeNull(); // caller renders it verbatim
    expect(reasonLabelKey('low_snr')).toBe('reason.low_snr');
  });
});

describe('reason vocabulary', () => {
  it('every known code has a translation in BOTH languages', () => {
    // The reviewer reads Sorani. An untranslated code would surface English internals in the RTL UI,
    // which is precisely what having machine-stable codes plus human labels exists to avoid.
    for (const code of KNOWN_REASON_CODES) {
      const key = `reason.${code}`;
      expect(en[key], `en is missing ${key}`).toBeTruthy();
      expect(ckb[key], `ckb is missing ${key}`).toBeTruthy();
      expect(ckb[key], `${key} is untranslated (identical to en)`).not.toBe(en[key]);
    }
  });

  it('matches the Rust vocabulary exactly — no drift in either direction', () => {
    // Mirrors jury::reason. Extra codes here would render labels the backend never writes; missing ones
    // would render raw strings. Both are drift, so the list is pinned by value.
    expect([...KNOWN_REASON_CODES].sort()).toEqual(
      [
        'clipping',
        'low_snr',
        'missing_audio',
        'model_disagreement',
        'policy_hold',
        'single_recognizer',
        't1_unresolved',
        't2_no_majority',
        'uncalibrated_bucket',
      ].sort(),
    );
  });

  it('does not tint operator/evidence reasons as clip defects', () => {
    // policy_hold means the autonomy dial forbade a machine commit; uncalibrated_bucket means there was
    // no threshold to judge against. Nothing is wrong with the CLIP in either case, and colouring them
    // like bad audio would tell the reviewer to distrust a recording that is fine.
    expect(reasonTone('policy_hold')).toBe('neutral');
    expect(reasonTone('uncalibrated_bucket')).toBe('neutral');
    expect(reasonTone('t1_unresolved')).toBe('neutral');
    // Genuine clip/decision problems are not neutral.
    expect(reasonTone('missing_audio')).toBe('danger');
    expect(reasonTone('low_snr')).toBe('warning');
    expect(reasonTone('model_disagreement')).toBe('warning');
  });
});
