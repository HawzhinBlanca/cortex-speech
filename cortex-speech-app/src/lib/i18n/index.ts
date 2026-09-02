import { derived, get, writable, type Writable } from 'svelte/store';
import type { TranslationKey } from './en';
import { ckb } from './ckb';

export type { TranslationKey } from './en';
export { autonomyLabelKey, autonomyValues, type AutonomyLevel } from './autonomy';

export type Translate = (key: TranslationKey, params?: Record<string, string>) => string;

export type Locale = 'en' | 'ckb';
type Dictionary = Readonly<Record<TranslationKey, string>>;

function readStoredLocale(): Locale {
  if (typeof localStorage === 'undefined') return 'ckb';
  const saved = localStorage.getItem('cortex-locale');
  return saved === 'en' || saved === 'ckb' ? saved : 'ckb';
}

const localeState = writable<Locale>(readStoredLocale());
let localeGeneration = 0;
let englishDictionary: Dictionary | null = null;
let englishLoad: Promise<Dictionary> | null = null;

function loadEnglishDictionary(): Promise<Dictionary> {
  if (englishDictionary) return Promise.resolve(englishDictionary);
  if (englishLoad) return englishLoad;
  const pending = import('./en').then(({ en }) => {
    englishDictionary = en;
    return en;
  });
  englishLoad = pending;
  void pending.catch(() => {
    if (englishLoad === pending) englishLoad = null;
  });
  return pending;
}

// Component tests intentionally change locale synchronously. Production keeps English in a real
// on-demand chunk; this test-only preload is removed by Vite's constant-folded production build.
if (import.meta.env.MODE === 'test') {
  englishDictionary = await loadEnglishDictionary();
}

/** Load the persisted secondary locale before the first component mounts. A corrupt/missing local
 * chunk falls back to the primary Sorani dictionary instead of flashing mixed-language copy. */
export async function prepareInitialLocale(): Promise<boolean> {
  if (get(localeState) !== 'en') return true;
  try {
    await loadEnglishDictionary();
    return true;
  } catch {
    localeState.set('ckb');
    return false;
  }
}

/** Publish a locale only after its complete dictionary exists. Concurrent switches are last-write
 * wins, so a delayed English chunk can never override a newer switch back to Sorani. */
export async function setLocale(value: Locale): Promise<boolean> {
  const generation = ++localeGeneration;
  try {
    if (value === 'en' && !englishDictionary) await loadEnglishDictionary();
  } catch {
    return false;
  }
  if (generation !== localeGeneration) return false;
  localeState.set(value);
  return true;
}

export const locale: Writable<Locale> = {
  subscribe: localeState.subscribe,
  set: (value) => void setLocale(value),
  update: (updater) => void setLocale(updater(get(localeState))),
};

localeState.subscribe((value) => {
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem('cortex-locale', value);
  }
});

/** Narrow untrusted/runtime strings before they are allowed into the typed translator. */
export function isTranslationKey(key: string): key is TranslationKey {
  return Object.prototype.hasOwnProperty.call(ckb, key);
}

export const t = derived(locale, ($locale) => {
  const dict = $locale === 'en' ? englishDictionary : ckb;
  return (key: TranslationKey, params?: Record<string, string>): string => {
    // English and Sorani retain a compile-time exact key contract. The Sorani fallback is only a
    // corruption guard: setLocale never publishes English before its complete dictionary exists.
    let text: string = dict?.[key] || ckb[key];
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
