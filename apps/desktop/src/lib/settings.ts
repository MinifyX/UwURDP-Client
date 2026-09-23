/**
 * The settings, kept in the page's own storage.
 *
 * Only preferences live here — how things look and behave. Nothing about a
 * host and no secret: those are in the store and the vault, on the Rust side.
 * A setting that Rust needs (the update channel) is handed over on start and
 * on every change.
 */

import { useSyncExternalStore } from 'react';
import pkg from '../../package.json';
import { language, t } from './i18n';
import type { Workspace } from './session';

export type ThemeSetting = 'system' | 'light' | 'dark';
/** German or English; "system" follows the language the system prefers. */
export type LanguageSetting = 'system' | 'de' | 'en';
/** Animations: follow the system's reduced-motion setting, or override it. */
export type MotionSetting = 'system' | 'on' | 'off';
/** What opens by itself when UwURDP starts: nothing, the overview, or chosen hosts. */
export type StartupSetting = 'nothing' | 'overview' | 'hosts';
/** Beta gets pre-releases (tags like v0.1.0-beta.1) before everyone else. */
export type UpdateChannel = 'stable' | 'beta';
/** How big the overview draws each desktop. */

export type Settings = {
  language: LanguageSetting;
  theme: ThemeSetting;
  motion: MotionSetting;
  startup: StartupSetting;
  /** The hosts `startup: 'hosts'` connects to, by id, in this order. */
  startupHosts: string[];
  confirmCloseWithSessions: boolean;
  /** A session that drops without being ended gets one quiet try to come back. */
  autoReconnect: boolean;
  /**
   * H.264 through Cisco's OpenH264, which the app downloads when this is
   * turned on. Off by default: the user decides whether Cisco's binary comes
   * onto the machine.
   */
  h264: boolean;
  updateChannel: UpdateChannel;
  /** Private and business hosts apart, like UwUMail's workspaces. */
  workspaces: boolean;
  activeWorkspace: Workspace;
  workspaceNames: Record<Workspace, string>;
  /** Groups folded away in the host list, as `workspace/name`. */
  collapsedGroups: string[];
};

export const DEFAULT_SETTINGS: Settings = {
  language: 'system',
  theme: 'dark',
  motion: 'system',
  startup: 'nothing',
  startupHosts: [],
  confirmCloseWithSessions: true,
  autoReconnect: true,
  h264: false,
  // Someone who installed a beta wants the next beta too.
  updateChannel: pkg.version.includes('-') ? 'beta' : 'stable',
  workspaces: true,
  activeWorkspace: 'private',
  workspaceNames: { private: '', business: '' },
  collapsedGroups: [],
};

const KEY = 'uwurdp.settings';

/** Stored values are checked one by one; anything unexpected falls back to its default. */
export function sanitize(raw: unknown): Settings {
  const input = typeof raw === 'object' && raw !== null ? (raw as Record<string, unknown>) : {};
  const oneOf = <T>(value: unknown, allowed: readonly T[], fallback: T): T =>
    allowed.includes(value as T) ? (value as T) : fallback;
  const bool = (value: unknown, fallback: boolean) =>
    typeof value === 'boolean' ? value : fallback;
  const d = DEFAULT_SETTINGS;
  const names =
    typeof input.workspaceNames === 'object' && input.workspaceNames !== null
      ? (input.workspaceNames as Record<string, unknown>)
      : {};
  const name = (value: unknown) => (typeof value === 'string' ? value.slice(0, 24) : '');
  return {
    language: oneOf(input.language, ['system', 'de', 'en'] as const, d.language),
    theme: oneOf(input.theme, ['system', 'light', 'dark'] as const, d.theme),
    motion: oneOf(input.motion, ['system', 'on', 'off'] as const, d.motion),
    startup: oneOf(input.startup, ['nothing', 'overview', 'hosts'] as const, d.startup),
    startupHosts: Array.isArray(input.startupHosts)
      ? [
          ...new Set(
            input.startupHosts
              .filter((id): id is string => typeof id === 'string' && id.length <= 64)
              .slice(0, 50),
          ),
        ]
      : [],
    confirmCloseWithSessions: bool(input.confirmCloseWithSessions, d.confirmCloseWithSessions),
    autoReconnect: bool(input.autoReconnect, d.autoReconnect),
    h264: bool(input.h264, d.h264),
    updateChannel: oneOf(input.updateChannel, ['stable', 'beta'] as const, d.updateChannel),
    workspaces: bool(input.workspaces, d.workspaces),
    activeWorkspace: oneOf(
      input.activeWorkspace,
      ['private', 'business'] as const,
      d.activeWorkspace,
    ),
    workspaceNames: { private: name(names.private), business: name(names.business) },
    collapsedGroups: Array.isArray(input.collapsedGroups)
      ? input.collapsedGroups
          .filter((g): g is string => typeof g === 'string')
          .map((g) => g.slice(0, 120))
          .slice(0, 500)
      : [],
  };
}

function load(): Settings {
  try {
    const raw = window.localStorage.getItem(KEY);
    return sanitize(raw ? JSON.parse(raw) : {});
  } catch {
    return DEFAULT_SETTINGS;
  }
}

let current = load();
const listeners = new Set<() => void>();

export function getSettings(): Settings {
  return current;
}

export function updateSettings(patch: Partial<Settings>) {
  current = sanitize({ ...current, ...patch });
  try {
    window.localStorage.setItem(KEY, JSON.stringify(current));
  } catch {
    // Private storage can be unavailable; the change still holds for this run.
  }
  for (const listener of listeners) listener();
}

export function subscribeSettings(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSettings(): Settings {
  return useSyncExternalStore(subscribeSettings, getSettings);
}

/** "Privat" and "Business", or the names the user gave them. */
export function workspaceName(workspace: Workspace, settings: Settings): string {
  const own = settings.workspaceNames[workspace].trim();
  if (own) return own;
  return workspace === 'private' ? t('Privat') : t('Business');
}

const darkQuery = () => window.matchMedia('(prefers-color-scheme: dark)');
const reducedQuery = () => window.matchMedia('(prefers-reduced-motion: reduce)');

/** Whether animations should play right now, by setting and system. */
export function motionAllowed(): boolean {
  const { motion } = current;
  return motion === 'on' || (motion === 'system' && !reducedQuery().matches);
}

/** Puts theme and motion on <html>, now and whenever the setting or the system changes. */
export function applyAppearance() {
  const apply = () => {
    const { theme } = current;
    const dark = theme === 'dark' || (theme === 'system' && darkQuery().matches);
    document.documentElement.dataset.theme = dark ? 'dark' : 'light';
    document.documentElement.lang = language(current);
    if (motionAllowed()) delete document.documentElement.dataset.motion;
    else document.documentElement.dataset.motion = 'reduced';
  };
  apply();
  subscribeSettings(apply);
  darkQuery().addEventListener('change', apply);
  reducedQuery().addEventListener('change', apply);
}
