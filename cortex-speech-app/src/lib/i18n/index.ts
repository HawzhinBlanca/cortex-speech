import { writable, derived } from 'svelte/store';
import { en } from './en';
import { ckb } from './ckb';

export type Locale = 'en' | 'ckb';

function readStoredLocale(): Locale {
  if (typeof localStorage === 'undefined') return 'ckb';
  const saved = localStorage.getItem('cortex-locale');
  return saved === 'en' || saved === 'ckb' ? saved : 'ckb';
}

export const locale = writable<Locale>(readStoredLocale());

locale.subscribe((value) => {
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem('cortex-locale', value);
  }
});

const translations = { en, ckb };

export const t = derived(locale, ($locale) => {
  const dict = translations[$locale];
  return (key: string, params?: Record<string, string>) => {
    // Fall back to English before showing the raw key, so a label that only exists in `en`
    // (e.g. an advanced/dev feature not yet translated) degrades to English rather than a key string.
    let text = dict[key] || en[key] || key;
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        text = text.replace(`{${k}}`, v);
      }
    }
    return text;
  };
});
