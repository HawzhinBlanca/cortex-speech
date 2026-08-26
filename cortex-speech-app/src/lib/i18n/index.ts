import { writable, derived } from 'svelte/store';
import { en, type TranslationKey } from './en';
import { ckb } from './ckb';

export type { TranslationKey } from './en';
export { autonomyLabelKey, autonomyValues, type AutonomyLevel } from './autonomy';

export type Translate = (key: TranslationKey, params?: Record<string, string>) => string;

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

/** Narrow untrusted/runtime strings before they are allowed into the typed translator. */
export function isTranslationKey(key: string): key is TranslationKey {
  return Object.prototype.hasOwnProperty.call(en, key);
}

export const t = derived(locale, ($locale) => {
  const dict = translations[$locale];
  return (key: TranslationKey, params?: Record<string, string>): string => {
    // English and Sorani have a compile-time exact key contract. The explicit fallback protects
    // against malformed runtime replacement data; it is not permission to ship a missing locale.
    let text: string = dict[key] || en[key];
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        // replaceAll, not replace: a string that repeats a placeholder (e.g. speaker.mergeConfirm uses
        // {target} twice) must substitute EVERY occurrence — replace() left the second one as literal
        // "{target}" in a destructive-merge confirmation dialog.
        text = text.replaceAll(`{${k}}`, v);
      }
    }
    return text;
  };
});
