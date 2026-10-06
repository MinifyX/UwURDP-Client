import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Button, IconButton, ICONS } from '@uwusuite/design';
import { hideWindowOnClose, onMacQuit, setMacMenu } from '@uwusuite/design/tauri';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { CertificateChanged, LoginPrompt, TrustCertificate } from './components/ConnectDialogs';
import { GroupLoginDialog } from './components/GroupLoginDialog';
import { GroupDrivesDialog } from './components/DrivesEditor';
import { HostForm } from './components/HostForm';
import { HostList } from './components/HostList';
import { ImportDialog } from './components/ImportDialog';
import { Modal } from './components/Modal';
import { NyuScene } from './components/nyu/scenes';
import { Overview, type OverviewEntry } from './components/Overview';
import { RdpView } from './components/RdpView';
import { SettingsDialog, type SettingsSection } from './components/SettingsDialog';
import { TabBar } from './components/TabBar';
import { TitleBar } from './components/TitleBar';
import { UpdateHint } from './components/UpdateHint';
import { buildInfo } from './lib/build';
import { VaultDialog } from './components/VaultDialog';
import { useAppAppearance } from './lib/appearance';
import { language, t } from './lib/i18n';
import { resolveLink, type DeepLink, type LinkSync } from './lib/link';
import type { Closed, RdpDriver } from './lib/rdp';
import {
  asConnectFailure,
  cancelConnect,
  closeAllSessions,
  connectHost,
  installUpdate,
  listGroups,
  openProjectPage,
  listHosts,
  setFullscreen,
  setHostLogin,
  setH264,
  syncForLink,
  takeDeepLink,
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
import { desktop, SHORTCUTS, withKeys } from './lib/shortcuts';
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

/** Why a link's host can't be connected, after the sync it got. */
function describeUnknownHost(sync: LinkSync): string {
  switch (sync.kind) {
    case 'done':
      return t(
        'Den Host aus dem Link gibt es auf diesem Gerät nicht, auch nach dem Synchronisieren nicht. Vielleicht wurde er gelöscht.',
      );
    case 'failed':
      return t(
        'Den Host aus dem Link gibt es auf diesem Gerät nicht, und das Synchronisieren hat nicht geklappt: {error}',
        { error: sync.message },
      );
    default:
      return t(
        'Den Host aus dem Link gibt es auf diesem Gerät nicht. Er wurde vielleicht gelöscht, oder dieses Gerät synchronisiert nicht mit dem, auf dem er angelegt wurde.',
      );
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
  useAppAppearance(settings);

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
  const [groupDrives, setGroupDrives] = useState<GroupRecord | null>(null);
  const [importing, setImporting] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState<SettingsSection | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);
  const [startupVault, setStartupVault] = useState(false);
  /** A `uwurdp://` link is being handled: start-up leaves the window to it. */
  const linkBusy = useRef(false);
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
    // Wait a tick: StrictMode disposes a first driver right away, and only the
    // one that is still there should start anything.
    window.setTimeout(() => {
      if (drivers.current.get(id) === driver) void startRef.current(id);
    }, 0);
  }, []);

  const onDriverDispose = useCallback((id: string, driver: RdpDriver) => {
    if (drivers.current.get(id) === driver) drivers.current.delete(id);
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
      // A link that started UwURDP asks for the vault itself, if it has to.
      if (linkBusy.current) return;
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

  // `uwurdp://connect/<host-id>`: the host's desktop, as a double click would
  // open it. Rust has already brought the window to the front. Links are
  // handled one after the other, the waiting one taken when Rust says there
  // is one and once on start, for the link that started UwURDP.
  const linkQueue = useRef<Promise<void>>(Promise.resolve());
  const openLink = useCallback(
    async (link: DeepLink) => {
      linkBusy.current = true;
      try {
        // Whatever an earlier page left open is closed first; not the new tab.
        await (boot ?? Promise.resolve());
        const outcome = await resolveLink(link, {
          vaultLocked: async () => (await vaultState().catch(() => null))?.status === 'locked',
          unlock: () => {
            setStartupVault(false);
            return unlock(
              '',
              t('Ein Link öffnet eine Verbindung – dafür muss der Tresor offen sein.'),
            );
          },
          hosts: listHosts,
          sync: syncForLink,
          onSyncing: () =>
            setAppNotice({
              tone: 'info',
              text: t('Der Host aus dem Link ist noch nicht auf diesem Gerät – synchronisiere…'),
            }),
        });
        switch (outcome.kind) {
          case 'connect':
            setAppNotice(null);
            void refreshHosts();
            connect(outcome.host);
            break;
          case 'locked':
            setAppNotice({ tone: 'info', text: t('Nicht verbunden: Der Tresor ist gesperrt.') });
            break;
          case 'unknown':
            void refreshHosts();
            setAppNotice({ tone: 'error', text: describeUnknownHost(outcome.sync) });
            break;
          case 'invalid':
            setAppNotice({
              tone: 'error',
              text: t(
                'Diesen Link kann UwURDP nicht öffnen. Ein Link zu einem Host sieht so aus: uwurdp://connect/<Host-ID>',
              ),
            });
            break;
        }
      } catch (e) {
        setAppNotice({ tone: 'error', text: String(e) });
      } finally {
        linkBusy.current = false;
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [connect, refreshHosts],
  );
  const openLinkRef = useRef(openLink);
  openLinkRef.current = openLink;

  useEffect(() => {
    const next = () => {
      linkQueue.current = linkQueue.current.then(async () => {
        const link = await takeDeepLink().catch(() => null);
        if (link) await openLinkRef.current(link);
      });
    };
    next();
    const stop = listen('deep-link', next);
    return () => void stop.then((unlisten) => unlisten());
  }, []);

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

  // UwULock ended this device's session: the vault was locked with it.
  useEffect(() => {
    const stop = listen<string>('sync:logout', () =>
      setAppNotice({
        tone: 'error',
        text: t(
          'UwULock hat die Sitzung dieses Geräts beendet, der Tresor ist wieder gesperrt. Melde dich unter Einstellungen → Sync neu an, damit UwURDP weiter synchronisiert.',
        ),
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
    if (!buildInfo().updates) return;
    void setUpdateChannel(settings.updateChannel).catch(() => undefined);
  }, [settings.updateChannel]);

  // The H.264 setting lives here; Rust fetches or deletes OpenH264 to match.
  useEffect(() => {
    if (!buildInfo().h264) return;
    void setH264(settings.h264).catch(() => undefined);
  }, [settings.h264]);

  useEffect(() => {
    if (!buildInfo().updates) return;
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
    dialog ||
    form ||
    groupLogin ||
    groupDrives ||
    importing ||
    settingsOpen ||
    confirmClose ||
    startupVault,
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
        return { host, tab: open };
      });
    }
    return rdpTabs.map((other) => ({ host: other.host, tab: other }));
  };

  // ── The macOS menu bar ────────────────────────────────────────────────────
  //
  // On a Mac the window has the system's title bar, so what the title bar has
  // elsewhere lives in the menu bar (package docs/macos.md): Einstellungen …
  // on ⌘, and the app's own entries. ⌘W closes the tab in front; with no tab
  // left the entry has no shortcut, and ⌘W reaches "Fenster schließen", which
  // hides the window (the app stays in the Dock). The menu replaces itself
  // when an entry changes.
  const menuState = useRef({ activeTab, modalOpen });
  menuState.current = { activeTab, modalOpen };
  const activeRdp = activeTab?.kind === 'rdp' ? activeTab : null;
  const activeSmart = activeRdp ? (activeRdp.smart ?? activeRdp.host.rdp.smartSizing) : false;
  useEffect(() => {
    if (desktop !== 'mac') return;
    const rdp = () => {
      const tab = menuState.current.activeTab;
      return tab?.kind === 'rdp' ? tab : null;
    };
    const free = !modalOpen;
    void setMacMenu({
      appName: 'UwURDP',
      lang,
      onSettings: () => setSettingsOpen('appearance'),
      app: buildInfo().updates
        ? [
            {
              text: `${t('Nach Updates suchen')} …`,
              enabled: free,
              action: () => setSettingsOpen('updates'),
            },
          ]
        : [],
      file: [
        {
          text: `${t('Host hinzufügen')} …`,
          accelerator: 'CmdOrCtrl+N',
          enabled: free,
          action: () => setForm({ host: null }),
        },
        {
          text: `${t('Importieren')} …`,
          enabled: free,
          action: () => setImporting(true),
        },
        {
          text: `${t('Exportieren')} …`,
          enabled: free,
          action: () => setSettingsOpen('data'),
        },
        'separator',
        {
          text: t('Tab schließen'),
          accelerator: activeId ? SHORTCUTS.closeTab : undefined,
          enabled: Boolean(activeId) && free,
          action: () => {
            const id = activeRef.current;
            if (id) closeTab(id);
          },
        },
      ],
      view: [
        {
          text: t('Übersicht'),
          accelerator: SHORTCUTS.overview,
          enabled: free,
          action: () => showOverview(null, null),
        },
      ],
      menus: [
        {
          text: t('Verbindung'),
          items: [
            {
              text: t('Strg+Alt+Entf senden'),
              enabled: activeRdp?.status === 'live' && free,
              action: () => {
                const tab = rdp();
                if (tab) drivers.current.get(tab.id)?.ctrlAltDel();
              },
            },
            {
              text: t('Einpassen'),
              checked: activeSmart,
              enabled: Boolean(activeRdp) && free,
              action: () => {
                const tab = rdp();
                if (tab) patchTab(tab.id, { smart: !(tab.smart ?? tab.host.rdp.smartSizing) });
              },
            },
            {
              text: t('Desktop im Vollbild'),
              accelerator: 'CmdOrCtrl+Shift+Enter',
              enabled: Boolean(activeRdp) && free,
              action: () => toggleFullscreen(),
            },
            'separator',
            {
              text: t('Weiteren Tab öffnen'),
              accelerator: SHORTCUTS.duplicateTab,
              enabled: Boolean(activeRdp) && free,
              action: () => duplicate(activeRef.current),
            },
            activeRdp?.status === 'live'
              ? {
                  text: t('Trennen'),
                  enabled: free,
                  action: () => {
                    const tab = rdp();
                    if (tab) disconnectTab(tab.id);
                  },
                }
              : {
                  text: t('Neu verbinden'),
                  enabled: Boolean(activeRdp) && activeRdp?.status !== 'connecting' && free,
                  action: () => {
                    const tab = rdp();
                    if (tab) restart(tab.id);
                  },
                },
          ],
        },
      ],
      help: [
        { text: t('Versionen'), action: () => void openProjectPage('releases') },
        { text: t('Quellcode auf GitHub'), action: () => void openProjectPage('source') },
        { text: t('Problem melden'), action: () => void openProjectPage('issues') },
      ],
    }).catch(() => undefined);
    // The handlers read the tabs through refs; the menu changes with what it shows.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lang, modalOpen, activeId, activeRdp?.status, Boolean(activeRdp), activeSmart]);

  // ── Keyboard ──────────────────────────────────────────────────────────────

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (modalRef.current || event.type !== 'keydown') return;
      const inDesktop =
        document.activeElement instanceof HTMLCanvasElement &&
        document.activeElement.classList.contains('rdp-canvas');
      const action = shortcutFor(event, inDesktop, desktop === 'mac');
      if (!action) return;
      event.preventDefault();
      event.stopPropagation();

      const id = activeRef.current;
      // ⇧⌘1 … 9 inside a desktop: ⌘ is down there as the Windows key.
      if (inDesktop && event.metaKey && id) drivers.current.get(id)?.maskWindowsKey();
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

  /**
   * Asks before open connections end with the window, when the settings say
   * so: closing it on Windows and Linux, quitting the app on a Mac (⌘Q, the
   * Dock, logging out — through uwu-macos, which waits for the answer). On a
   * Mac closing the window only hides it; the sessions go on, and a click on
   * the Dock icon brings them back.
   */
  const askClose = useCallback(async (): Promise<boolean> => {
    if (!getSettings().confirmCloseWithSessions || liveRef.current === 0) return true;
    if (desktop === 'mac') {
      const window = getCurrentWindow();
      await window.show().catch(() => undefined);
      await window.setFocus().catch(() => undefined);
    }
    return new Promise<boolean>((resolve) => {
      closeAnswer.current?.(false);
      closeAnswer.current = resolve;
      setConfirmClose(true);
    });
  }, []);
  const closeAnswer = useRef<((ok: boolean) => void) | null>(null);
  const answerClose = (ok: boolean) => {
    setConfirmClose(false);
    const answer = closeAnswer.current;
    closeAnswer.current = null;
    answer?.(ok);
  };

  useEffect(() => {
    const stops: Promise<() => void>[] = [];
    if (desktop === 'mac') {
      stops.push(hideWindowOnClose(), onMacQuit(askClose));
    } else {
      const window = getCurrentWindow();
      stops.push(
        window.onCloseRequested(async (event) => {
          if (getSettings().confirmCloseWithSessions && liveRef.current > 0) {
            event.preventDefault();
            if (await askClose()) void window.destroy();
          }
        }),
      );
    }
    return () => {
      for (const stop of stops) void stop.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, [askClose]);

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
        <span className="toolbar-actions">
          <Button
            variant="ghost"
            size="sm"
            icon={ICONS.keyboard}
            disabled={activeTab.status !== 'live'}
            onClick={() => drivers.current.get(activeTab.id)?.ctrlAltDel()}
            title={withKeys(t('Strg+Alt+Entf an den Server senden'), SHORTCUTS.ctrlAltDel)}
          >
            {t('Strg+Alt+Entf')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            icon={ICONS.fit}
            className="aria-pressed:bg-pink-tint aria-pressed:text-pink-ink"
            onClick={() =>
              patchTab(activeTab.id, {
                smart: !(activeTab.smart ?? activeTab.host.rdp.smartSizing),
              })
            }
            aria-pressed={activeTab.smart ?? activeTab.host.rdp.smartSizing}
            title={t('Einen zu großen Desktop einpassen statt scrollen')}
          >
            {t('Einpassen')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            icon={ICONS.fullscreen}
            onClick={() => toggleFullscreen()}
            title={withKeys(t('Vollbild'), SHORTCUTS.fullscreen)}
          >
            {t('Vollbild')}
          </Button>
          {activeTab.status === 'live' ? (
            <Button
              variant="ghost"
              size="sm"
              icon={ICONS.disconnect}
              onClick={() => disconnectTab(activeTab.id)}
              title={t('Sitzung trennen; der Tab bleibt offen')}
            >
              {t('Trennen')}
            </Button>
          ) : (
            <Button
              variant="ghost"
              size="sm"
              icon={ICONS.refresh}
              onClick={() => restart(activeTab.id)}
              disabled={activeTab.status === 'connecting'}
            >
              {t('Neu verbinden')}
            </Button>
          )}
        </span>
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
              onGroupDrives={setGroupDrives}
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
                <Button
                  variant="ghost"
                  size="sm"
                  icon={ICONS.keyboard}
                  className="text-stage-ink hover:bg-white/10"
                  onClick={() => drivers.current.get(activeTab.id)?.ctrlAltDel()}
                >
                  {t('Strg+Alt+Entf')}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  icon={ICONS.exitFullscreen}
                  className="text-stage-ink hover:bg-white/10"
                  onClick={() => toggleFullscreen(false)}
                >
                  {t('Vollbild beenden')}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  icon={ICONS.disconnect}
                  className="text-stage-ink hover:bg-white/10"
                  onClick={() => {
                    toggleFullscreen(false);
                    disconnectTab(activeTab.id);
                  }}
                >
                  {t('Trennen')}
                </Button>
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
                  <Button size="sm" onClick={notice.action.run}>
                    {notice.action.label}
                  </Button>
                )}
                <IconButton
                  size="sm"
                  icon={ICONS.close}
                  onClick={() =>
                    activeTab?.notice
                      ? patchTab(activeTab.id, { notice: null })
                      : setAppNotice(null)
                  }
                  label={t('Hinweis schließen')}
                />
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
                      <Button
                        variant="primary"
                        icon={ICONS.refresh}
                        onClick={() => restart(tab.id)}
                      >
                        {t('Neu verbinden')}
                      </Button>
                    </div>
                  )}
                  {tab.kind === 'rdp' && tab.status === 'ended' && (
                    <div className="pane-overlay" data-tone="ended">
                      <p>{t('Getrennt.')}</p>
                      <Button
                        variant="primary"
                        icon={ICONS.refresh}
                        onClick={() => restart(tab.id)}
                      >
                        {t('Neu verbinden')}
                      </Button>
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
                    <Button
                      variant="primary"
                      icon={ICONS.import}
                      onClick={() => setImporting(true)}
                    >
                      {t('RDCMan-Datei importieren')}
                    </Button>
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

      {groupDrives && (
        <GroupDrivesDialog
          group={groupDrives}
          onCancel={() => setGroupDrives(null)}
          onSaved={() => {
            setGroupDrives(null);
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
          size="small"
          onCancel={() => answerClose(false)}
          footer={
            <>
              <Button data-autofocus onClick={() => answerClose(false)}>
                {t('Abbrechen')}
              </Button>
              <Button variant="primary" data-secondary onClick={() => answerClose(true)}>
                {desktop === 'mac' ? t('Beenden') : t('Schließen')}
              </Button>
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
