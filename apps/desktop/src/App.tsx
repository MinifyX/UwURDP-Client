import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { CertificateChanged, LoginPrompt, TrustCertificate } from './components/ConnectDialogs';
import { GroupLoginDialog } from './components/GroupLoginDialog';
import { HostForm } from './components/HostForm';
import { HostList } from './components/HostList';
import { Icon } from './components/Icon';
import { ImportDialog } from './components/ImportDialog';
import { Modal } from './components/Modal';
import { NyuScene } from './components/nyu/scenes';
import { Overview, type OverviewEntry } from './components/Overview';
import { RdpView } from './components/RdpView';
import { SettingsDialog, type SettingsSection } from './components/SettingsDialog';
import { TabBar } from './components/TabBar';
import { TitleBar } from './components/TitleBar';
import { UpdateHint } from './components/UpdateHint';
import { VaultDialog } from './components/VaultDialog';
import { language, t } from './lib/i18n';
import type { Closed, RdpDriver } from './lib/rdp';
import {
  asConnectFailure,
  cancelConnect,
  closeAllSessions,
  connectHost,
  installUpdate,
  listGroups,
  listHosts,
  setFullscreen,
  setHostLogin,
  setUpdateChannel,
  trustCertificate,
  updateStatus,
  vaultState,
  type ConnectFailure,
  type GroupRecord,
  type HostRecord,
  type ObservedCertificate,
  type TypedLogin,
  type UpdateInfo,
  type Viewport,
  type Workspace,
} from './lib/session';
import { getSettings, useSettings } from './lib/settings';
import type { Withheld } from './lib/sync';
import {
  createTab,
  describe,
  neighbourAfterClose,
  newTabId,
  shortcutFor,
  type Notice,
  type Tab,
  type TabKind,
} from './lib/tabs';

type LoginAnswer = { login: TypedLogin; save: boolean };

/** A question one tab's connection needs answered. Asked one at a time, in order. */
type Dialog = { tabId: string; cancel: () => void } & (
  | {
      kind: 'login';
      host: HostRecord;
      gateway: boolean;
      username: string;
      domain: string;
      retry: boolean;
      resolve: (value: LoginAnswer | null) => void;
    }
  | {
      kind: 'trust';
      host: HostRecord;
      observed: ObservedCertificate;
      resolve: (ok: boolean) => void;
    }
  | {
      kind: 'changed';
      host: HostRecord;
      expected: string;
      observed: ObservedCertificate;
      resolve: (accept: boolean) => void;
    }
  | { kind: 'unlock'; reason: string; resolve: (unlocked: boolean) => void }
);

/** How long after a drop an automatic reconnect is still tried; a second drop within it is left alone. */
const RECONNECT_WINDOW_MS = 60_000;

/** Failures that are not a question for the user, in words the user can act on. */
function describeFailure(failure: ConnectFailure, host: HostRecord): string {
  switch (failure.kind) {
    case 'unreachable':
      return t('{address} ist nicht erreichbar: {reason}', {
        address: host.address,
        reason: failure.message,
      });
    case 'timeout':
      return t('{address} antwortet nicht.', { address: host.address });
    case 'negotiation':
      return t('Der Server und UwURDP werden sich nicht einig: {reason}', {
        reason: failure.message,
      });
    case 'gateway':
      return t('Das Gateway lässt die Verbindung nicht durch: {reason}', {
        reason: failure.message,
      });
    case 'auth-failed':
      return t('Die Anmeldung wurde abgelehnt: {reason}', { reason: failure.message });
    case 'protocol':
      return t('RDP-Fehler: {reason}', { reason: failure.message });
    case 'internal':
      return failure.message;
    default:
      return t('Verbindung fehlgeschlagen ({kind}).', { kind: failure.kind });
  }
}

/** What a session that ended says in its tab. */
function describeEnd(closed: Closed | null, host: HostRecord): string {
  switch (closed?.reason) {
    case 'logoff':
      return t('Von {name} abgemeldet.', { name: host.name });
    case 'server':
      return closed.message
        ? t('{name} hat die Sitzung beendet: {reason}', { name: host.name, reason: closed.message })
        : t('{name} hat die Sitzung beendet.', { name: host.name });
    case 'error':
      return closed.message
        ? t('Die Verbindung zu {name} ist abgebrochen: {reason}', {
            name: host.name,
            reason: closed.message,
          })
        : t('Die Verbindung zu {name} ist abgebrochen.', { name: host.name });
    default:
      return t('Die Verbindung zu {name} wurde getrennt.', { name: host.name });
  }
}

/**
 * Whatever a page before this one left open is closed first, exactly once —
 * StrictMode runs effects twice, and a second close would take the new tabs.
 */
let boot: Promise<unknown> | null = null;

export function App() {
  const settings = useSettings();

  // ── Tabs ──────────────────────────────────────────────────────────────────
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const tabsRef = useRef<Tab[]>([]);
  tabsRef.current = tabs;
  const activeRef = useRef<string | null>(null);
  activeRef.current = activeId;
  /** When each tab was last in front, so a click on its host can bring back the right one. */
  const lastShown = useRef(new Map<string, number>());
  useEffect(() => {
    if (activeId) lastShown.current.set(activeId, Date.now());
  }, [activeId]);
  /** The desktop of each tab, outside React. */
  const drivers = useRef(new Map<string, RdpDriver>());
  /** Tabs with a start in flight, so a double click on "reconnect" starts one. */
  const starting = useRef(new Set<string>());
  /** Tabs the user disconnected on purpose: no automatic reconnect for those. */
  const disconnecting = useRef(new Set<string>());
  /** When each tab last reconnected on its own. */
  const reconnected = useRef(new Map<string, number>());
  /** Bumped when a driver comes or goes, so the overview picks it up. */
  const [driversVersion, setDriversVersion] = useState(0);

  const [hosts, setHosts] = useState<HostRecord[]>([]);
  const [groups, setGroups] = useState<GroupRecord[]>([]);
  const hostsRef = useRef<HostRecord[]>([]);
  hostsRef.current = hosts;
  const [appNotice, setAppNotice] = useState<Notice | null>(null);
  const [dialogs, setDialogs] = useState<Dialog[]>([]);
  const dialogsRef = useRef<Dialog[]>([]);
  dialogsRef.current = dialogs;
  const [form, setForm] = useState<{
    host: HostRecord | null;
    workspace?: Workspace;
    group?: string | null;
  } | null>(null);
  const [groupLogin, setGroupLogin] = useState<GroupRecord | null>(null);
  const [importing, setImporting] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState<SettingsSection | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);
  const [startupVault, setStartupVault] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [updateDismissed, setUpdateDismissed] = useState(false);
  const [fullscreen, setFullscreenState] = useState(false);
  const fullscreenRef = useRef(false);
  fullscreenRef.current = fullscreen;
  const backgroundRef = useRef<HTMLDivElement>(null);

  const patchTab = useCallback((id: string, patch: Partial<Tab>) => {
    setTabs((current) =>
      current.map((tab) => (tab.id === id ? ({ ...tab, ...patch } as Tab) : tab)),
    );
  }, []);

  /** Still the same desktop in the same open tab? Anything else means: stop. */
  const alive = (id: string, driver: RdpDriver) => drivers.current.get(id) === driver;

  const refreshHosts = useCallback(async () => {
    try {
      const [loadedHosts, loadedGroups] = await Promise.all([listHosts(), listGroups()]);
      setHosts(loadedHosts);
      setGroups(loadedGroups);
      // Open tabs show the newest record: name, address, login.
      setTabs((current) =>
        current.map((tab) => {
          if (tab.kind !== 'rdp') return tab;
          const fresh = loadedHosts.find((h) => h.id === tab.host.id);
          return fresh && fresh !== tab.host
            ? ({ ...tab, host: fresh, ...describe({ kind: 'rdp', host: fresh }) } as Tab)
            : tab;
        }),
      );
    } catch (e) {
      setAppNotice({
        tone: 'error',
        text: t('Hosts konnten nicht geladen werden: {error}', { error: String(e) }),
      });
    }
  }, []);

  /** Show a dialog for a tab and wait for its answer. */
  function ask<T>(
    tabId: string,
    cancelValue: T,
    build: (resolve: (value: T) => void) => Omit<Dialog, 'tabId' | 'cancel'>,
  ): Promise<T> {
    return new Promise<T>((resolve) => {
      let done = false;
      const finish = (value: T) => {
        if (done) return;
        done = true;
        setDialogs((current) => current.filter((dialog) => dialog !== entry));
        resolve(value);
      };
      const entry = {
        ...build(finish),
        tabId,
        cancel: () => finish(cancelValue),
      } as Dialog;
      setDialogs((current) => [...current, entry]);
    });
  }

  const unlock = (tabId: string, reason: string) =>
    ask<boolean>(tabId, false, (resolve) => ({ kind: 'unlock', reason, resolve }));

  // ── Full screen ───────────────────────────────────────────────────────────

  const toggleFullscreen = useCallback((on?: boolean) => {
    const next = on ?? !fullscreenRef.current;
    if (next === fullscreenRef.current) return;
    setFullscreenState(next);
    void setFullscreen(next).catch(() => undefined);
    window.setTimeout(() => {
      const id = activeRef.current;
      if (id) drivers.current.get(id)?.focus();
    }, 50);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.fullscreen = fullscreen ? 'on' : 'off';
  }, [fullscreen]);

  // ── Connecting: the conversation every connection goes through ────────────

  /** The size to ask the server for, by the host's display setting. */
  const viewportFor = (host: HostRecord, driver: RdpDriver): Viewport => {
    const scale = window.devicePixelRatio || 1;
    switch (host.rdp.display) {
      case 'fixed':
        return { width: host.rdp.width, height: host.rdp.height, scale: 100 };
      case 'fullscreen':
        return {
          width: Math.round(window.screen.width * scale),
          height: Math.round(window.screen.height * scale),
          scale: Math.round(scale * 100),
        };
      default:
        return driver.viewport();
    }
  };

  /** The login a typed one should be kept as, with the vault opened if it has to be. */
  const keepLogin = (tabId: string, host: HostRecord, login: TypedLogin) => {
    void (async () => {
      for (let tries = 0; tries < 2; tries += 1) {
        try {
          await setHostLogin(host.id, login.username, login.domain, login.password || null);
          void refreshHosts();
          return;
        } catch (error) {
          const failure = error as { kind?: string };
          if (failure?.kind !== 'vault-locked') throw error;
          const opened = await unlock(
            tabId,
            t('Das Passwort wird verschlüsselt im Tresor gespeichert.'),
          );
          if (!opened) return;
        }
      }
    })().catch((e) =>
      setAppNotice({
        tone: 'error',
        text: t('Anmeldung nicht gespeichert: {error}', { error: String(e) }),
      }),
    );
  };

  /** A session ended: say so in its tab, and maybe come back on our own. */
  const onSessionEnd = (id: string, driver: RdpDriver, host: HostRecord, closed: Closed | null) => {
    if (!alive(id, driver)) return;
    const wanted = disconnecting.current.delete(id);
    const dropped = !wanted && (closed === null || closed.reason === 'error');
    const last = reconnected.current.get(id) ?? 0;
    if (dropped && getSettings().autoReconnect && Date.now() - last > RECONNECT_WINDOW_MS) {
      reconnected.current.set(id, Date.now());
      patchTab(id, {
        status: 'connecting',
        notice: { tone: 'info', text: t('Verbindung verloren – verbinde neu…') },
      });
      window.setTimeout(() => restart(id), 1_500);
      return;
    }
    patchTab(id, {
      status: 'ended',
      notice: {
        tone: dropped ? 'error' : 'info',
        text: wanted ? t('Getrennt.') : describeEnd(closed, host),
        action: { label: t('Neu verbinden'), run: () => restart(id) },
      },
    });
    if (fullscreenRef.current && activeRef.current === id) toggleFullscreen(false);
  };

  /**
   * Connect until it works or the user stops: trust a new certificate, accept
   * or reject a changed one, open the vault, ask for a login. A typed password
   * lives in a local variable for the attempts that need it.
   */
  const runConnect = async (id: string, driver: RdpDriver, host: HostRecord) => {
    patchTab(id, { status: 'connecting', notice: null });
    const reconnect = { label: t('Neu verbinden'), run: () => restart(id) };
    let login: TypedLogin | null = null;
    let gatewayLogin: TypedLogin | null = null;
    let save = false;
    let lastKind: ConnectFailure['kind'] | null = null;
    const stop = (notice: Notice | null) => {
      if (alive(id, driver)) patchTab(id, { status: 'failed', notice });
    };

    for (;;) {
      if (!alive(id, driver)) return;
      try {
        await driver.attach(
          (onData, onEnd) =>
            connectHost(host.id, id, viewportFor(host, driver), login, gatewayLogin, onData, onEnd),
          (closed) => onSessionEnd(id, driver, host, closed),
        );
        if (!alive(id, driver)) return;
        if (login && save) keepLogin(id, host, login);
        patchTab(id, { status: 'live', notice: null });
        if (activeRef.current === id) driver.focus();
        if (host.rdp.display === 'fullscreen' && activeRef.current === id) toggleFullscreen(true);
        void refreshHosts();
        return;
      } catch (raw) {
        if (!alive(id, driver)) {
          void cancelConnect(id).catch(() => undefined);
          return;
        }
        const failure = asConnectFailure(raw);
        const retry = lastKind === failure.kind;
        lastKind = failure.kind;

        switch (failure.kind) {
          case 'unknown-certificate': {
            const trusted = await ask<boolean>(id, false, (resolve) => ({
              kind: 'trust',
              host,
              observed: failure.observed,
              resolve,
            }));
            if (!trusted) {
              return stop({
                tone: 'info',
                text: t('Nicht verbunden: Das Zertifikat wurde nicht bestätigt.'),
                action: reconnect,
              });
            }
            await trustCertificate(host.address, host.port, failure.observed.fingerprint);
            continue;
          }
          case 'certificate-changed': {
            const accepted = await ask<boolean>(id, false, (resolve) => ({
              kind: 'changed',
              host,
              expected: failure.expected,
              observed: failure.observed,
              resolve,
            }));
            if (!accepted) {
              return stop({
                tone: 'error',
                text: t('Nicht verbunden: Das Zertifikat von {address} hat sich geändert.', {
                  address: host.address,
                }),
              });
            }
            await trustCertificate(host.address, host.port, failure.observed.fingerprint, true);
            continue;
          }
          case 'vault-locked': {
            const opened = await unlock(
              id,
              t('Die Anmeldedaten für {name} liegen im Tresor.', { name: host.name }),
            );
            if (!opened) {
              return stop({
                tone: 'info',
                text: t('Nicht verbunden: Der Tresor ist gesperrt.'),
                action: reconnect,
              });
            }
            continue;
          }
          case 'login-required':
          case 'auth-failed': {
            const known: { username: string; domain: string } =
              failure.kind === 'login-required'
                ? { username: failure.username, domain: failure.domain }
                : (login ?? { username: host.username, domain: host.domain });
            const answer: LoginAnswer | null = await ask<LoginAnswer | null>(
              id,
              null,
              (resolve) => ({
                kind: 'login',
                host,
                gateway: false,
                username: known.username,
                domain: known.domain,
                retry: failure.kind === 'auth-failed',
                resolve,
              }),
            );
            if (!answer) {
              return stop({ tone: 'info', text: t('Nicht verbunden.'), action: reconnect });
            }
            login = answer.login;
            save = answer.save;
            continue;
          }
          case 'gateway-login-required': {
            const answer = await ask<LoginAnswer | null>(id, null, (resolve) => ({
              kind: 'login',
              host,
              gateway: true,
              username: failure.username,
              domain: failure.domain,
              retry: gatewayLogin !== null,
              resolve,
            }));
            if (!answer) {
              return stop({ tone: 'info', text: t('Nicht verbunden.'), action: reconnect });
            }
            gatewayLogin = answer.login;
            continue;
          }
          case 'cancelled':
            return stop(null);
          default:
            return stop({
              tone: 'error',
              text: describeFailure(failure, host),
              action: retry ? undefined : { label: t('Nochmal'), run: () => restart(id) },
            });
        }
      }
    }
  };

  /** Starts whatever the tab is for, on its current desktop. */
  const start = async (id: string) => {
    const driver = drivers.current.get(id);
    const tab = tabsRef.current.find((candidate) => candidate.id === id);
    if (!driver || !tab || tab.kind !== 'rdp' || starting.current.has(id)) return;
    starting.current.add(id);
    try {
      // The newest record for the host, in case it was edited meanwhile.
      const host = hostsRef.current.find((candidate) => candidate.id === tab.host.id) ?? tab.host;
      await runConnect(id, driver, host);
    } catch (e) {
      if (drivers.current.get(id) === driver)
        patchTab(id, { status: 'failed', notice: { tone: 'error', text: String(e) } });
    } finally {
      starting.current.delete(id);
    }
  };
  const startRef = useRef(start);
  startRef.current = start;

  const restart = (id: string) => {
    const driver = drivers.current.get(id);
    if (driver?.session) {
      disconnecting.current.add(id);
      driver.disconnect();
    }
    void startRef.current(id);
  };

  /** Ends the session but keeps the tab and its last picture. */
  const disconnectTab = useCallback((id: string) => {
    const driver = drivers.current.get(id);
    if (!driver?.session) return;
    disconnecting.current.add(id);
    driver.disconnect();
  }, []);

  // ── Opening, closing, switching ───────────────────────────────────────────

  const openTab = useCallback((kind: TabKind, focus = true) => {
    setAppNotice(null);
    // Functional updates only: a status change another tab queued in the
    // same tick must not be overwritten by a stale list.
    const id = newTabId();
    setTabs((current) => [...current, createTab(current, kind, id)]);
    if (focus) setActiveId(id);
    return id;
  }, []);

  const closeTab = useCallback((id: string) => {
    const current = tabsRef.current;
    if (!current.some((tab) => tab.id === id)) return;
    // Questions this tab was waiting for are answered with "cancel".
    for (const dialog of dialogsRef.current) if (dialog.tabId === id) dialog.cancel();
    if (activeRef.current === id) setActiveId(neighbourAfterClose(current, id));
    setTabs((list) => list.filter((tab) => tab.id !== id));
    // Unmounting the view disposes its driver, which closes the session.
  }, []);

  const onDriverReady = useCallback((id: string, driver: RdpDriver) => {
    drivers.current.set(id, driver);
    setDriversVersion((v) => v + 1);
    // Wait a tick: StrictMode disposes a first driver right away, and only the
    // one that is still there should start anything.
    window.setTimeout(() => {
      if (drivers.current.get(id) === driver) void startRef.current(id);
    }, 0);
  }, []);

  const onDriverDispose = useCallback((id: string, driver: RdpDriver) => {
    if (drivers.current.get(id) === driver) drivers.current.delete(id);
    setDriversVersion((v) => v + 1);
  }, []);

  /** The open tab matching `match` that was in front last, if there is one. */
  const recentTab = (match: (tab: Tab) => boolean) =>
    tabsRef.current
      .filter(match)
      .sort((a, b) => (lastShown.current.get(b.id) ?? 0) - (lastShown.current.get(a.id) ?? 0))[0];

  /**
   * A click in the sidebar: a host that already has a tab gets that tab
   * brought to the front, one without gets a new one. Another tab to the same
   * host is in the host's context menu.
   */
  const connect = useCallback(
    (host: HostRecord, focus = true) => {
      const open = recentTab((tab) => tab.kind === 'rdp' && tab.host.id === host.id);
      if (open) {
        setAppNotice(null);
        if (focus) setActiveId(open.id);
        // A tab whose connection ended or never came up connects again: a
        // click on the host means "connect me", not "show me the error".
        if (open.status === 'failed' || open.status === 'ended') restart(open.id);
        return open.id;
      }
      return openTab({ kind: 'rdp', host }, focus);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [openTab],
  );
  const connectAnother = useCallback(
    (host: HostRecord) => openTab({ kind: 'rdp', host }),
    [openTab],
  );
  const disconnectHost = useCallback(
    (host: HostRecord) => {
      for (const tab of tabsRef.current) {
        if (tab.kind === 'rdp' && tab.host.id === host.id) closeTab(tab.id);
      }
    },
    [closeTab],
  );

  const groupHosts = (workspace: Workspace, group: string) =>
    hostsRef.current
      .filter(
        (host) =>
          (getSettings().workspaces ? host.workspace === workspace : true) &&
          host.groupPath === group,
      )
      .sort((a, b) => a.position - b.position || a.name.localeCompare(b.name, 'de'));

  const showOverview = useCallback(
    (workspace: Workspace | null, group: string | null) => {
      const open = recentTab(
        (tab) => tab.kind === 'overview' && tab.group === group && tab.workspace === workspace,
      );
      if (open) {
        setActiveId(open.id);
        return;
      }
      openTab({ kind: 'overview', workspace, group });
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [openTab],
  );

  /** RDCMan's "connect group": every host of the group, each in its own tab, in the background. */
  const connectGroup = useCallback(
    (workspace: Workspace, group: string) => {
      const members = groupHosts(workspace, group);
      for (const host of members) connect(host, false);
      showOverview(workspace, group);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [connect, showOverview],
  );
  const disconnectGroup = useCallback(
    (workspace: Workspace, group: string) => {
      for (const host of groupHosts(workspace, group)) disconnectHost(host);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [disconnectHost],
  );

  const duplicate = useCallback(
    (id: string | null) => {
      const tab = tabsRef.current.find((candidate) => candidate.id === id);
      if (tab?.kind === 'rdp') openTab({ kind: 'rdp', host: tab.host });
    },
    [openTab],
  );

  // ── Start-up ──────────────────────────────────────────────────────────────

  useEffect(() => {
    void refreshHosts();
    boot ??= closeAllSessions().catch(() => 0);
    let cancelled = false;
    void boot.then(async () => {
      if (cancelled) return;
      // A vault this device doesn't open on its own asks once, now, instead
      // of on the first host that needs it.
      const vault = await vaultState().catch(() => null);
      if (!cancelled && vault?.status === 'locked' && !vault.remembered) setStartupVault(true);
      // A vault a refused sync connect left stranded gets its password back now.
      if (!cancelled && vault?.stranded && vault.remembered) setStartupVault(true);
      // Nothing opens on its own unless the settings say so: the overview,
      // or the chosen hosts, each in its own tab, the first one in front.
      if (cancelled || tabsRef.current.length > 0) return;
      const { startup, startupHosts } = getSettings();
      if (startup === 'overview') {
        openTab({ kind: 'overview', workspace: null, group: null });
      } else if (startup === 'hosts' && startupHosts.length > 0) {
        const known = await listHosts().catch(() => [] as HostRecord[]);
        if (cancelled || tabsRef.current.length > 0) return;
        const chosen = startupHosts.flatMap((id) => known.filter((host) => host.id === id));
        const ids = chosen.map((host) => openTab({ kind: 'rdp', host }));
        if (ids[0]) setActiveId(ids[0]);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [openTab, refreshHosts]);

  // Another device changed hosts or groups: the list loads again.
  useEffect(() => {
    const stop = listen('sync:changed', () => void refreshHosts());
    return () => void stop.then((unlisten) => unlisten());
  }, [refreshHosts]);

  // The server keeps records back or hands out old versions. Said once, when
  // it begins, whatever is open; the sync settings keep saying it.
  useEffect(() => {
    const stop = listen<Withheld>('sync:withheld', ({ payload }) =>
      setAppNotice({
        tone: 'error',
        text: [
          t(
            'Der Server liefert nicht den neuesten Stand – er hält Daten zurück oder spielt alte Versionen ein.',
          ),
          payload.hostKeys
            ? t(
                'Zertifikaten aus dem Sync wird bis dahin nicht vertraut – beim nächsten Verbinden fragt UwURDP wieder nach.',
              )
            : null,
        ]
          .filter(Boolean)
          .join(' '),
        action: { label: t('Sync-Einstellungen'), run: () => setSettingsOpen('sync') },
      }),
    );
    return () => void stop.then((unlisten) => unlisten());
  }, []);

  // Dev builds only: lets end-to-end tests reach the active desktop. Stripped
  // from release.
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    Object.defineProperty(window, '__uwurdpDriver', {
      configurable: true,
      get: () => (activeRef.current ? drivers.current.get(activeRef.current) : undefined),
    });
  }, []);

  // Tab names follow the language.
  const lang = language(settings);
  const tabsLanguage = useRef(lang);
  useEffect(() => {
    if (tabsLanguage.current === lang) return;
    tabsLanguage.current = lang;
    setTabs((current) => current.map((tab) => ({ ...tab, ...describe(tab) }) as Tab));
  }, [lang]);

  // ── Updates ───────────────────────────────────────────────────────────────

  useEffect(() => {
    void setUpdateChannel(settings.updateChannel).catch(() => undefined);
  }, [settings.updateChannel]);

  useEffect(() => {
    void updateStatus()
      .then((ready) => ready && setUpdate(ready))
      .catch(() => undefined);
    const stop = listen<UpdateInfo>('update:ready', (event) => {
      setUpdate(event.payload);
      setUpdateDismissed(false);
    });
    return () => void stop.then((unlisten) => unlisten());
  }, []);

  // ── Derived state ─────────────────────────────────────────────────────────

  const activeTab = tabs.find((tab) => tab.id === activeId) ?? null;
  const liveConnections = tabs.filter((tab) => tab.kind === 'rdp' && tab.status === 'live').length;
  const liveRef = useRef(0);
  liveRef.current = liveConnections;
  const openHostIds = useMemo(
    () => new Set(tabs.flatMap((tab) => (tab.kind === 'rdp' ? [tab.host.id] : []))),
    [tabs],
  );
  const onlineIds = useMemo(
    () =>
      new Set(
        tabs.flatMap((tab) => (tab.kind === 'rdp' && tab.status === 'live' ? [tab.host.id] : [])),
      ),
    [tabs],
  );
  const connectingIds = useMemo(
    () =>
      new Set(
        tabs.flatMap((tab) =>
          tab.kind === 'rdp' && tab.status === 'connecting' ? [tab.host.id] : [],
        ),
      ),
    [tabs],
  );
  const dialog = dialogs[0] ?? null;
  const modalOpen = Boolean(
    dialog || form || groupLogin || importing || settingsOpen || confirmClose || startupVault,
  );
  const modalRef = useRef(false);
  modalRef.current = modalOpen;

  useEffect(() => {
    if (backgroundRef.current) backgroundRef.current.inert = modalOpen;
  }, [modalOpen]);

  // A question belongs to its tab: show that tab while it is asked.
  useEffect(() => {
    if (
      dialog &&
      dialog.tabId !== activeRef.current &&
      tabsRef.current.some((tab) => tab.id === dialog.tabId)
    ) {
      setActiveId(dialog.tabId);
    }
  }, [dialog]);

  // The shown desktop gets the keyboard.
  useEffect(() => {
    if (!activeId || modalOpen) return;
    const frame = window.requestAnimationFrame(() => drivers.current.get(activeId)?.focus());
    return () => window.cancelAnimationFrame(frame);
  }, [activeId, modalOpen]);

  // Full screen belongs to a desktop: showing something else leaves it.
  useEffect(() => {
    if (fullscreen && activeTab?.kind !== 'rdp') toggleFullscreen(false);
  }, [activeTab, fullscreen, toggleFullscreen]);

  const overviewEntries = (tab: Tab): OverviewEntry[] => {
    if (tab.kind !== 'overview') return [];
    const rdpTabs = tabs.filter((other) => other.kind === 'rdp') as (Tab & {
      kind: 'rdp';
    })[];
    if (tab.group && tab.workspace) {
      return groupHosts(tab.workspace, tab.group).map((host) => {
        const open =
          rdpTabs
            .filter((other) => other.host.id === host.id)
            .sort(
              (a, b) => (lastShown.current.get(b.id) ?? 0) - (lastShown.current.get(a.id) ?? 0),
            )[0] ?? null;
        return { host, tab: open, driver: open ? (drivers.current.get(open.id) ?? null) : null };
      });
    }
    return rdpTabs.map((other) => ({
      host: other.host,
      tab: other,
      driver: drivers.current.get(other.id) ?? null,
    }));
  };
  void driversVersion;

  // ── Keyboard ──────────────────────────────────────────────────────────────

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (modalRef.current || event.type !== 'keydown') return;
      const inDesktop =
        document.activeElement instanceof HTMLCanvasElement &&
        document.activeElement.classList.contains('rdp-canvas');
      const action = shortcutFor(event, inDesktop);
      if (!action) return;
      event.preventDefault();
      event.stopPropagation();

      const id = activeRef.current;
      const list = tabsRef.current;
      const index = list.findIndex((tab) => tab.id === id);
      switch (action.kind) {
        case 'overview':
          showOverview(null, null);
          break;
        case 'close-tab':
          if (id) closeTab(id);
          break;
        case 'duplicate-tab':
          duplicate(id);
          break;
        case 'next-tab':
        case 'previous-tab': {
          if (list.length < 2) break;
          const step = action.kind === 'next-tab' ? 1 : -1;
          setActiveId(list[(index + step + list.length) % list.length]!.id);
          break;
        }
        case 'select-tab': {
          const target = list[action.index];
          if (target) setActiveId(target.id);
          break;
        }
        case 'settings':
          setSettingsOpen('appearance');
          break;
        case 'fullscreen':
          if (list[index]?.kind === 'rdp') toggleFullscreen();
          break;
        case 'ctrl-alt-del':
          if (id) drivers.current.get(id)?.ctrlAltDel();
          break;
        case 'release-focus': {
          if (fullscreenRef.current) toggleFullscreen(false);
          (document.activeElement as HTMLElement | null)?.blur();
          document.querySelector<HTMLElement>('.tab[data-active="true"] .tab-select')?.focus();
          break;
        }
      }
    };
    // Capture phase: the desktop must not see the app's own shortcuts.
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [closeTab, duplicate, showOverview, toggleFullscreen]);

  // ── Closing the window ────────────────────────────────────────────────────

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let stopped = false;
    void getCurrentWindow()
      .onCloseRequested((event) => {
        if (getSettings().confirmCloseWithSessions && liveRef.current > 0) {
          event.preventDefault();
          setConfirmClose(true);
        }
      })
      .then((stop) => {
        if (stopped) stop();
        else unlisten = stop;
      })
      .catch(() => undefined);
    return () => {
      stopped = true;
      unlisten?.();
    };
  }, []);

  // ── Render ────────────────────────────────────────────────────────────────

  const sidebarActive =
    activeTab?.kind === 'rdp'
      ? activeTab.host.id
      : activeTab?.kind === 'overview' && !activeTab.group
        ? 'overview'
        : null;
  const notice = activeTab?.notice ?? appNotice;

  const toolbar = activeTab && (
    <div className="toolbar">
      <span className="session-title">
        <b>{activeTab.title}</b>
        {activeTab.status === 'connecting' && activeTab.kind === 'rdp' ? (
          <span className="meta">{t('verbindet…')}</span>
        ) : (
          activeTab.subtitle && <span className="meta">{activeTab.subtitle}</span>
        )}
      </span>
      <span className="spacer" />
      {activeTab.kind === 'rdp' && (
        <>
          <button
            className="quiet toolbar-button"
            disabled={activeTab.status !== 'live'}
            onClick={() => drivers.current.get(activeTab.id)?.ctrlAltDel()}
            title={t('Strg+Alt+Entf an den Server senden (Strg+Alt+Ende)')}
          >
            <Icon name="keyboard" size={15} />
            {t('Strg+Alt+Entf')}
          </button>
          <button
            className="quiet toolbar-button"
            onClick={() =>
              patchTab(activeTab.id, {
                smart: !(activeTab.smart ?? activeTab.host.rdp.smartSizing),
              })
            }
            aria-pressed={activeTab.smart ?? activeTab.host.rdp.smartSizing}
            title={t('Einen zu großen Desktop einpassen statt scrollen')}
          >
            <Icon name="scale" size={15} />
            {t('Einpassen')}
          </button>
          <button
            className="quiet toolbar-button"
            onClick={() => toggleFullscreen()}
            title={t('Vollbild (Strg+Alt+Pause)')}
          >
            <Icon name="fullscreen" size={15} />
            {t('Vollbild')}
          </button>
          {activeTab.status === 'live' ? (
            <button
              className="quiet toolbar-button"
              onClick={() => disconnectTab(activeTab.id)}
              title={t('Sitzung trennen; der Tab bleibt offen')}
            >
              <Icon name="power" size={15} />
              {t('Trennen')}
            </button>
          ) : (
            <button
              className="quiet toolbar-button"
              onClick={() => restart(activeTab.id)}
              disabled={activeTab.status === 'connecting'}
            >
              <Icon name="refresh" size={15} />
              {t('Neu verbinden')}
            </button>
          )}
        </>
      )}
    </div>
  );

  return (
    <div className="shell" data-fullscreen={fullscreen || undefined}>
      <div ref={backgroundRef} className="background">
        {!fullscreen && <TitleBar onSettings={() => setSettingsOpen('appearance')} />}

        <div className="body">
          {!fullscreen && (
            <HostList
              hosts={hosts}
              groups={groups}
              activeId={sidebarActive}
              onlineIds={onlineIds}
              connectingIds={connectingIds}
              openIds={openHostIds}
              onConnect={(host) => connect(host)}
              onConnectAnother={connectAnother}
              onDisconnect={disconnectHost}
              onOverview={showOverview}
              onConnectGroup={connectGroup}
              onDisconnectGroup={disconnectGroup}
              onGroupLogin={setGroupLogin}
              onAdd={(workspace, group) => setForm({ host: null, workspace, group })}
              onEdit={(host) => setForm({ host })}
              onImport={() => setImporting(true)}
              onChanged={() => void refreshHosts()}
              onError={(text) => setAppNotice({ tone: 'error', text })}
            />
          )}

          <main className="main">
            {!fullscreen && (
              <TabBar
                tabs={tabs}
                activeId={activeId}
                onSelect={setActiveId}
                onClose={closeTab}
                onOverview={() => showOverview(null, null)}
              />
            )}

            {fullscreen && activeTab?.kind === 'rdp' ? (
              <div className="fullscreen-bar" role="toolbar" aria-label={t('Verbindungsleiste')}>
                <b>{activeTab.title}</b>
                <button
                  className="quiet toolbar-button"
                  onClick={() => drivers.current.get(activeTab.id)?.ctrlAltDel()}
                >
                  <Icon name="keyboard" size={15} />
                  {t('Strg+Alt+Entf')}
                </button>
                <button className="quiet toolbar-button" onClick={() => toggleFullscreen(false)}>
                  <Icon name="fullscreen" size={15} />
                  {t('Vollbild beenden')}
                </button>
                <button
                  className="quiet toolbar-button"
                  onClick={() => {
                    toggleFullscreen(false);
                    disconnectTab(activeTab.id);
                  }}
                >
                  <Icon name="power" size={15} />
                  {t('Trennen')}
                </button>
              </div>
            ) : (
              toolbar
            )}

            {notice && !fullscreen ? (
              <div
                className="notice"
                data-tone={notice.tone}
                role={notice.tone === 'error' ? 'alert' : 'status'}
              >
                <span>{notice.text}</span>
                <span className="spacer" />
                {notice.action && (
                  <button onClick={notice.action.run}>{notice.action.label}</button>
                )}
                <button
                  className="icon-button"
                  onClick={() =>
                    activeTab?.notice
                      ? patchTab(activeTab.id, { notice: null })
                      : setAppNotice(null)
                  }
                  aria-label={t('Hinweis schließen')}
                >
                  ×
                </button>
              </div>
            ) : null}

            <div className="session-wrap">
              {tabs.map((tab) => (
                <div
                  key={tab.id}
                  className="session-pane"
                  data-kind={tab.kind}
                  data-status={tab.status}
                  hidden={tab.id !== activeId}
                  role="tabpanel"
                  aria-label={tab.title}
                >
                  {tab.kind === 'rdp' ? (
                    <RdpView
                      fit={{
                        follow: tab.host.rdp.display === 'fit',
                        smartSizing: tab.smart ?? tab.host.rdp.smartSizing,
                      }}
                      onReady={(driver) => onDriverReady(tab.id, driver)}
                      onDispose={(driver) => onDriverDispose(tab.id, driver)}
                    />
                  ) : (
                    <Overview
                      entries={overviewEntries(tab)}
                      group={tab.group}
                      onShow={setActiveId}
                      onConnect={(host) => connect(host, false)}
                      onConnectAll={() =>
                        tab.workspace && tab.group && connectGroup(tab.workspace, tab.group)
                      }
                      onDisconnect={disconnectTab}
                    />
                  )}
                  {tab.kind === 'rdp' &&
                    tab.status === 'connecting' &&
                    !dialogs.some((d) => d.tabId === tab.id) && (
                      <div className="pane-overlay" aria-live="polite">
                        <NyuScene name="connecting" className="pane-scene" />
                        <p>{t('Verbinde mit {name}…', { name: tab.host.name })}</p>
                      </div>
                    )}
                  {tab.kind === 'rdp' && tab.status === 'failed' && (
                    <div className="pane-overlay" data-tone="failed">
                      <NyuScene name="loadError" className="pane-scene" />
                      <p>{t('Nicht verbunden.')}</p>
                      <button className="primary" onClick={() => restart(tab.id)}>
                        {t('Neu verbinden')}
                      </button>
                    </div>
                  )}
                  {tab.kind === 'rdp' && tab.status === 'ended' && (
                    <div className="pane-overlay" data-tone="ended">
                      <p>{t('Getrennt.')}</p>
                      <button className="primary" onClick={() => restart(tab.id)}>
                        {t('Neu verbinden')}
                      </button>
                    </div>
                  )}
                </div>
              ))}
              {tabs.length === 0 && (
                <div className="no-tabs">
                  <NyuScene name="pick" className="no-tabs-scene" />
                  <p className="no-tabs-title">{t('Kein Tab offen')}</p>
                  <p className="no-tabs-text">
                    {t(
                      'Klick links einen Host an – jeder Desktop bekommt seinen eigenen Tab. Mit Rechtsklick auf eine Gruppe verbindest du alle ihre Hosts auf einmal.',
                    )}
                  </p>
                  {hosts.length === 0 && (
                    <button className="primary" onClick={() => setImporting(true)}>
                      {t('RDCMan-Datei importieren')}
                    </button>
                  )}
                </div>
              )}
            </div>
          </main>
        </div>
      </div>

      {update && !updateDismissed && !settingsOpen && !fullscreen && (
        <UpdateHint
          update={update}
          openConnections={liveConnections}
          onLater={() => setUpdateDismissed(true)}
          onRestart={installUpdate}
        />
      )}

      {form && (
        <HostForm
          host={form.host}
          workspace={form.workspace}
          group={form.group}
          groups={groups}
          onCancel={() => setForm(null)}
          onSaved={() => {
            setForm(null);
            // Open tabs of this host show the new record; they reconnect with the new data.
            void refreshHosts();
          }}
          onDeleted={() => {
            setForm(null);
            void refreshHosts();
          }}
        />
      )}

      {groupLogin && (
        <GroupLoginDialog
          group={groupLogin}
          onCancel={() => setGroupLogin(null)}
          onSaved={() => {
            setGroupLogin(null);
            void refreshHosts();
          }}
        />
      )}

      {importing && (
        <ImportDialog onClose={() => setImporting(false)} onImported={() => void refreshHosts()} />
      )}

      {settingsOpen && (
        <SettingsDialog
          initial={settingsOpen}
          onClose={() => setSettingsOpen(null)}
          update={update}
          onUpdateFound={(found) => {
            setUpdate(found);
            setUpdateDismissed(false);
          }}
          onInstallUpdate={() => {
            setSettingsOpen(null);
            setUpdateDismissed(false);
          }}
          onImport={() => {
            setSettingsOpen(null);
            setImporting(true);
          }}
          onChanged={() => void refreshHosts()}
        />
      )}

      {startupVault && !dialog && (
        <VaultDialog
          reason={t(
            'Einmal entsperren – dann verbinden alle Hosts mit gespeicherten Passwörtern, ohne weiter zu fragen.',
          )}
          cancelLabel={t('Später')}
          onDone={() => {
            setStartupVault(false);
            void refreshHosts();
          }}
          onCancel={() => setStartupVault(false)}
        />
      )}

      {confirmClose && (
        <Modal
          title={t('UwURDP schließen?')}
          onCancel={() => setConfirmClose(false)}
          footer={
            <>
              <span className="spacer" />
              <button data-autofocus onClick={() => setConfirmClose(false)}>
                {t('Abbrechen')}
              </button>
              <button
                className="primary"
                data-secondary
                onClick={() => void getCurrentWindow().destroy()}
              >
                {t('Schließen')}
              </button>
            </>
          }
        >
          <NyuScene name="goodbye" className="dialog-scene" />
          <p className="dialog-lead">
            {liveConnections === 1
              ? t('Eine Verbindung ist noch offen und wird getrennt.')
              : t('{count} Verbindungen sind noch offen und werden getrennt.', {
                  count: liveConnections,
                })}
          </p>
        </Modal>
      )}

      {dialog?.kind === 'login' && (
        <LoginPrompt
          host={dialog.host}
          gateway={dialog.gateway}
          username={dialog.username}
          domain={dialog.domain}
          retry={dialog.retry}
          canSave={!dialog.gateway}
          onSubmit={(login, save) => dialog.resolve({ login, save })}
          onCancel={() => dialog.resolve(null)}
        />
      )}
      {dialog?.kind === 'trust' && (
        <TrustCertificate
          host={dialog.host}
          observed={dialog.observed}
          onTrust={() => dialog.resolve(true)}
          onCancel={() => dialog.resolve(false)}
        />
      )}
      {dialog?.kind === 'changed' && (
        <CertificateChanged
          host={dialog.host}
          expected={dialog.expected}
          observed={dialog.observed}
          onAccept={() => dialog.resolve(true)}
          onReject={() => dialog.resolve(false)}
        />
      )}
      {dialog?.kind === 'unlock' && (
        <VaultDialog
          reason={dialog.reason}
          onDone={() => dialog.resolve(true)}
          onCancel={() => dialog.resolve(false)}
        />
      )}
    </div>
  );
}
