/**
 * The app in German or English.
 *
 * German is the source language: every string is written in German where it
 * is used and wrapped in `t()`, and the English catalogue in `src/i18n/en/`
 * maps each German string to its English one. A string without an English
 * entry stays German; `pnpm lint` runs `scripts/check-i18n.mjs`, which lists
 * every one that is missing.
 *
 * Placeholders are `{name}`, filled from `vars`. Strings kept in module-level
 * constants are marked with `N_()` — which does nothing but lets the check find
 * them — and translated with `t()` where they are shown.
 *
 * A component that shows text calls `useLanguage()` (or `useSettings()`), so it
 * draws again when the language changes.
 */

import { EN } from '../i18n/en';
import { getSettings, useSettings, type LanguageSetting, type Settings } from './settings';

export type Language = 'de' | 'en';

export type Vars = Record<string, string | number>;

/** "System" is German when Windows prefers German, English otherwise. */
export function resolveLanguage(setting: LanguageSetting): Language {
  if (setting !== 'system') return setting;
  const preferred = navigator.languages?.[0] ?? navigator.language ?? '';
  return preferred.toLowerCase().startsWith('de') ? 'de' : 'en';
}

export function language(settings: Settings = getSettings()): Language {
  return resolveLanguage(settings.language);
}

function fill(template: string, vars?: Vars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in vars ? String(vars[name]) : whole,
  );
}

export function translate(lang: Language, text: string, vars?: Vars): string {
  return fill(lang === 'en' ? (EN[text] ?? text) : text, vars);
}

/** `text` (German) in the current language, placeholders filled. */
export function t(text: string, vars?: Vars): string {
  return translate(language(), text, vars);
}

/** Marks a German string kept in a constant for translation where it is shown. */
export function N_(text: string): string {
  return text;
}

/** The current language; the calling component draws again when it changes. */
export function useLanguage(): Language {
  return language(useSettings());
}

/** For dates and numbers. */
export function locale(lang: Language = language()): string {
  return lang === 'de' ? 'de-DE' : 'en-GB';
}
