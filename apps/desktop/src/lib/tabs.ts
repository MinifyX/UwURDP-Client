/**
 * Tabs: one remote desktop each, or an overview of several, as plain data.
 * The desktops themselves live in {@link RdpDriver}s outside React; a tab only
 * says what it is, how it is doing and what to tell the user.
 */

import { t } from './i18n';
import { loginLabel, type HostRecord, type Workspace } from './session';

export type TabKind =
  | { kind: 'rdp'; host: HostRecord }
  /** Thumbnails, like RDCMan's group view: every open desktop, or one group's hosts. */
  | { kind: 'overview'; workspace: Workspace | null; group: string | null };

/**
 * - `connecting` — logging in, possibly waiting for a dialog
 * - `live` — a session is attached
 * - `ended` — the session stopped; the last picture stays
 * - `failed` — never got a session
 */
export type TabStatus = 'connecting' | 'live' | 'ended' | 'failed';

export type Notice = {
  tone: 'info' | 'error';
  text: string;
  action?: { label: string; run: () => void };
};

export type Tab = TabKind & {
  id: string;
  title: string;
  subtitle: string | null;
  /** 2 for the second open tab to the same host, and so on. */
  ordinal: number;
  status: TabStatus;
  notice: Notice | null;
  /** Einpassen switched in the toolbar, for this tab only; unset follows the host. */
  smart?: boolean;
};

let counter = 0;

/** Tab ids double as connection-attempt names on the Rust side: plain ASCII. */
export function newTabId(): string {
  counter += 1;
  return `tab-${Date.now().toString(36)}-${counter}`;
}

/** What makes two tabs "the same thing": the host, or the group an overview shows. */
export function sameKey(kind: TabKind): string {
  return kind.kind === 'rdp'
    ? `rdp:${kind.host.id}`
    : `overview:${kind.workspace ?? ''}/${kind.group ?? ''}`;
}

/** Where a host is: `DOMAIN\user @ address:port`, leaving out what's default or empty. */
export function hostLine(host: HostRecord): string {
  const port = host.port === 3389 ? '' : `:${host.port}`;
  const login = host.username ? `${loginLabel(host.username, host.domain)} @ ` : '';
  return `${login}${host.address}${port}`;
}

export function describe(kind: TabKind): { title: string; subtitle: string | null } {
  switch (kind.kind) {
    case 'rdp':
      return { title: kind.host.name, subtitle: hostLine(kind.host) };
    case 'overview':
      return kind.group
        ? { title: kind.group, subtitle: t('Übersicht der Gruppe') }
        : { title: t('Übersicht'), subtitle: t('Alle offenen Sitzungen') };
  }
}

/** The smallest number no open tab of the same kind uses yet. */
export function nextOrdinal(tabs: Tab[], kind: TabKind): number {
  const key = sameKey(kind);
  const used = new Set(tabs.filter((tab) => sameKey(tab) === key).map((tab) => tab.ordinal));
  let ordinal = 1;
  while (used.has(ordinal)) ordinal += 1;
  return ordinal;
}

export function createTab(tabs: Tab[], kind: TabKind, id: string = newTabId()): Tab {
  return {
    ...kind,
    ...describe(kind),
    id,
    ordinal: nextOrdinal(tabs, kind),
    status: kind.kind === 'overview' ? 'live' : 'connecting',
    notice: null,
  };
}

/** Which tab to show after closing `id`: the one to its right, else to its left. */
export function neighbourAfterClose(tabs: Tab[], id: string): string | null {
  const index = tabs.findIndex((tab) => tab.id === id);
  if (index < 0) return null;
  const rest = tabs.filter((tab) => tab.id !== id);
  return rest[Math.min(index, rest.length - 1)]?.id ?? null;
}

export type ShortcutAction =
  | { kind: 'overview' }
  | { kind: 'close-tab' }
  | { kind: 'duplicate-tab' }
  | { kind: 'next-tab' }
  | { kind: 'previous-tab' }
  | { kind: 'select-tab'; index: number }
  | { kind: 'settings' }
  | { kind: 'fullscreen' }
  | { kind: 'ctrl-alt-del' }
  | { kind: 'release-focus' };

type KeyLike = Pick<KeyboardEvent, 'key' | 'code' | 'ctrlKey' | 'shiftKey' | 'altKey' | 'metaKey'>;

/**
 * The app's own shortcuts.
 *
 * Inside a remote desktop every key belongs to the server — Ctrl+Shift+Esc,
 * Ctrl+Tab and friends are Windows' own. Only the combinations mstsc and
 * RDCMan reserve stay here: Ctrl+Alt+End for Ctrl+Alt+Del, Ctrl+Alt+Break for
 * full screen, Ctrl+Alt+Home to take the keyboard back, and Ctrl+Alt+PageUp/
 * PageDown between tabs. Outside a desktop the usual Ctrl+Shift ones work too.
 */
export function shortcutFor(event: KeyLike, inDesktop: boolean): ShortcutAction | null {
  if (!event.ctrlKey || event.metaKey) return null;
  if (event.altKey) {
    switch (event.code) {
      case 'End':
        return { kind: 'ctrl-alt-del' };
      case 'Pause':
      case 'Cancel':
        return { kind: 'fullscreen' };
      case 'Home':
        return { kind: 'release-focus' };
      case 'PageDown':
        return { kind: 'next-tab' };
      case 'PageUp':
        return { kind: 'previous-tab' };
    }
    return null;
  }
  if (inDesktop) return null;
  if (event.key === 'Tab' || event.code === 'PageDown' || event.code === 'PageUp') {
    const back = event.code === 'PageUp' || (event.key === 'Tab' && event.shiftKey);
    return { kind: back ? 'previous-tab' : 'next-tab' };
  }
  if (!event.shiftKey) {
    if (event.code === 'Comma') return { kind: 'settings' };
    return null;
  }
  switch (event.code) {
    case 'KeyO':
      return { kind: 'overview' };
    case 'KeyW':
      return { kind: 'close-tab' };
    case 'KeyD':
      return { kind: 'duplicate-tab' };
    case 'Enter':
      return { kind: 'fullscreen' };
  }
  const digit = /^Digit([1-9])$/.exec(event.code);
  if (digit) return { kind: 'select-tab', index: Number(digit[1]) - 1 };
  return null;
}
