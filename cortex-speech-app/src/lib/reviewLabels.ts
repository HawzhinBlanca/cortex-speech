import type { Translate } from './i18n';

/** Localized agreement label; poor audio always overrides apparently strong model agreement. */
export function confidenceBand(
  confidence: number | null | undefined,
  translate: Translate,
  poorAudio = false,
): { label: string; color: string } {
  const percent = (value: number) => ({ pct: String(Math.round(value * 100)) });
  if (confidence == null) {
    return { label: translate('inbox.band.unknown'), color: 'var(--text-subtle)' };
  }
  // Agreement is not trustworthiness: every recognizer can agree on the same garbage. The jury uses
  // this same poor-audio veto, so the UI must never turn a strongly-agreed but unusable clip green.
  if (poorAudio) {
    return {
      label: translate('inbox.band.poorAudio', percent(confidence)),
      color: 'var(--warning)',
    };
  }
  if (confidence >= 0.9) {
    return {
      label: translate('inbox.band.veryConfident', percent(confidence)),
      color: 'var(--success)',
    };
  }
  if (confidence >= 0.75) {
    return {
      label: translate('inbox.band.fairlySure', percent(confidence)),
      color: 'var(--warning)',
    };
  }
  if (confidence >= 0.55) {
    return {
      label: translate('inbox.band.unsure', percent(confidence)),
      color: 'rgb(var(--orange-400-rgb))',
    };
  }
  return {
    label: translate('inbox.band.low', percent(confidence)),
    color: 'var(--danger)',
  };
}
