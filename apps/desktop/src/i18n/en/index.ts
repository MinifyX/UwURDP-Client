/**
 * The English catalogue: German string → English string, one file per area of
 * the app. See `lib/i18n.ts`.
 */

import app from './app.json';
import hosts from './hosts.json';
import rdp from './rdp.json';
import settings from './settings.json';

export const EN: Readonly<Record<string, string>> = {
  ...app,
  ...hosts,
  ...rdp,
  ...settings,
};
