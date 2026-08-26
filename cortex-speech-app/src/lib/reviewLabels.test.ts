import { get } from 'svelte/store';
import { describe, expect, it } from 'vitest';
import { locale, t } from './i18n';
import { confidenceBand } from './reviewLabels';

describe('confidenceBand', () => {
  it('lets poor audio override apparently strong model agreement', () => {
    locale.set('en');
    const translate = get(t);
    const result = confidenceBand(0.99, translate, true);
    expect(result.label).toContain('99%');
    expect(result.label).not.toBe(confidenceBand(0.99, translate, false).label);
    expect(result.color).toBe('var(--warning)');
  });

  it('preserves the established threshold and unknown-state colors', () => {
    locale.set('en');
    const translate = get(t);
    expect(confidenceBand(undefined, translate).color).toBe('var(--text-subtle)');
    expect(confidenceBand(0.9, translate).color).toBe('var(--success)');
    expect(confidenceBand(0.75, translate).color).toBe('var(--warning)');
    expect(confidenceBand(0.55, translate).color).toBe('rgb(var(--orange-400-rgb))');
    expect(confidenceBand(0.54, translate).color).toBe('var(--danger)');
  });
});
