/**
 * How the window looks, from the settings: theme, contrast and motion through
 * @uwusuite/design (useAppearance puts them on <html>; /boot.js does it before
 * the first paint), the font through applyUiFont, the language on <html lang>.
 */

import { applyUiFont, useAppearance } from '@uwusuite/design';
import { useEffect } from 'react';
import { language } from './i18n';
import { getSettings, type Settings } from './settings';

/** Before the first render: font and language (the theme is /boot.js's). */
export function prepareDocument() {
  const settings = getSettings();
  applyUiFont(settings.font);
  document.documentElement.lang = language(settings);
}

/** Keeps <html> in step with the settings and the system. */
export function useAppAppearance(settings: Settings) {
  const resolved = useAppearance({
    theme: settings.theme,
    contrast: settings.contrast,
    motion: settings.motion,
  });
  const lang = language(settings);

  useEffect(() => {
    applyUiFont(settings.font);
  }, [settings.font]);

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  return resolved;
}
