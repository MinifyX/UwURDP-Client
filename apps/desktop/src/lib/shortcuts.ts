/**
 * Shortcuts are written once, as Tauri accelerators (`CmdOrCtrl+,`), and shown
 * the platform's way: `⌘,` on a Mac, `Strg+,` or `Ctrl+,` elsewhere. The same
 * accelerators go into the macOS menu bar (App.tsx), so menu and tooltip agree.
 *
 * The desktop's own combinations (Ctrl+Alt+End and friends, lib/tabs.ts) are
 * Ctrl on every system, like in mstsc: ⌘ belongs to the Mac.
 */

import { detectPlatform, shortcutText, withShortcut } from '@uwusuite/design';
import { language } from './i18n';

/** The desktop platform for chrome decisions: title bar or menu bar. */
export const desktop = detectPlatform();

/** `CmdOrCtrl+,` as this platform writes it. */
export function keys(accelerator: string): string {
  return shortcutText(accelerator, desktop, language());
}

/** A tooltip with its shortcut: `Einstellungen (⌘,)`. */
export function withKeys(label: string, accelerator: string): string {
  return withShortcut(label, accelerator, desktop, language());
}

/**
 * The app's shortcuts outside a desktop, as accelerators. On a Mac ⌘W closes
 * the tab (and the window once no tab is left, App.tsx); elsewhere Ctrl+W
 * would belong to the remote desktop's programs, so it is Ctrl+Shift+W there.
 */
export const SHORTCUTS = {
  settings: 'CmdOrCtrl+,',
  overview: 'CmdOrCtrl+Shift+O',
  closeTab: desktop === 'mac' ? 'CmdOrCtrl+W' : 'CmdOrCtrl+Shift+W',
  duplicateTab: 'CmdOrCtrl+Shift+D',
  /** mstsc's own; Ctrl on a Mac too, like there. */
  ctrlAltDel: 'Ctrl+Alt+End',
  fullscreen: 'Ctrl+Alt+Pause',
} as const;
