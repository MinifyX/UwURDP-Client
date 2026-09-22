/**
 * The IPC surface, in one file.
 *
 * Everything the UI knows about Tauri lives here.
 */

import { invoke } from '@tauri-apps/api/core';

export type SessionId = string;

export type DataHandler = (bytes: Uint8Array) => void;
export type EndHandler = () => void;

/**
 * A desktop's own line to the engine: a WebSocket on 127.0.0.1 (see
 * `src-tauri/src/frames.rs`). Frames come in as binary messages; acks and
 * input go out on it too, off Tauri's main thread and in order.
 */
export type FrameLink = {
  /** One frame message is drawn: the engine may send the next. */
  ack(): void;
  input(events: InputEvent[]): void;
};

const ACK = new Uint8Array([1]);
const OPEN_TIMEOUT_MS = 5_000;

let endpoint: Promise<{ port: number; token: string }> | null = null;

/**
 * Opens the socket for one connect attempt and waits until the app has it.
 * `onEnd` runs once when the app closes it after the session; `quiet()`
 * closes it without calling `onEnd`, for an attempt that never became one.
 */
async function openLink(
  attempt: string,
  onData: DataHandler,
  onEnd: EndHandler,
): Promise<FrameLink & { quiet(): void }> {
  endpoint ??= invoke<{ port: number; token: string }>('frame_socket');
  const { port, token } = await endpoint;
  const socket = new WebSocket(
    `ws://127.0.0.1:${port}/?t=${encodeURIComponent(token)}&a=${encodeURIComponent(attempt)}`,
  );
  socket.binaryType = 'arraybuffer';
  let quiet = false;
  let ready = false;
  await new Promise<void>((resolve, reject) => {
    const timer = window.setTimeout(() => {
      socket.close();
      reject(new Error('the frame socket did not open'));
    }, OPEN_TIMEOUT_MS);
    socket.onmessage = (event) => {
      if (ready) {
        if (event.data instanceof ArrayBuffer) onData(new Uint8Array(event.data));
        return;
      }
      if (event.data === 'ready') {
        ready = true;
        window.clearTimeout(timer);
        resolve();
      }
    };
    socket.onclose = () => {
      window.clearTimeout(timer);
      if (!ready) reject(new Error('the frame socket closed'));
      else if (!quiet) onEnd();
    };
  });
  const send = (data: string | Uint8Array) => {
    if (socket.readyState === WebSocket.OPEN) socket.send(data);
  };
  return {
    ack: () => send(ACK),
    input: (events) => send(JSON.stringify(events)),
    quiet: () => {
      quiet = true;
      socket.close();
    },
  };
}

// ── Sessions ────────────────────────────────────────────────────────────────

/** What the page sends: keys by scancode, the mouse in desktop pixels. */
export type MouseButton = 'left' | 'right' | 'middle' | 'x1' | 'x2';

export type InputEvent =
  | { type: 'key'; code: number; extended: boolean; down: boolean }
  | { type: 'unicode'; ch: string; down: boolean }
  | { type: 'move'; x: number; y: number }
  | { type: 'button'; button: MouseButton; down: boolean; x: number; y: number }
  | { type: 'wheel'; delta: number; horizontal: boolean }
  | { type: 'releaseAll' }
  | { type: 'ctrlAltDel' };

/** The tab changed size: ask the server for a desktop of `width` × `height` device pixels. */
export function resizeSession(
  id: SessionId,
  width: number,
  height: number,
  scale: number,
): Promise<void> {
  return invoke('resize_session', { id, width, height, scale });
}

/** The session got the focus back: offer this computer's clipboard to the server again. */
export function clipboardChanged(id: SessionId): Promise<void> {
  return invoke('clipboard_changed', { id });
}

export function closeSession(id: SessionId): Promise<void> {
  return invoke('close_session', { id });
}

// ── Hosts ───────────────────────────────────────────────────────────────────

/** Private or business, like UwUMail's workspaces. */
export type Workspace = 'private' | 'business';

export type DisplayMode = 'fit' | 'fixed' | 'fullscreen';
export type AudioMode = 'local' | 'remote' | 'off';

export type GatewaySettings = {
  address: string;
  port: number;
  /** Log in to the gateway with the host's own login. */
  useHostLogin: boolean;
  bypassLocal: boolean;
};

/** Mirrors `uwurdp_proto::RdpSettings`. */
export type RdpSettings = {
  display: DisplayMode;
  width: number;
  height: number;
  /** Scale a desktop that doesn't fit instead of scrolling it. */
  smartSizing: boolean;
  colorDepth: number;
  audio: AudioMode;
  clipboard: boolean;
  /** The console session, like `mstsc /admin`. */
  admin: boolean;
  nla: boolean;
  wallpaper: boolean;
  gateway?: GatewaySettings | null;
};

export const DEFAULT_RDP: RdpSettings = {
  display: 'fit',
  width: 1920,
  height: 1080,
  smartSizing: true,
  colorDepth: 32,
  audio: 'local',
  clipboard: true,
  admin: false,
  nla: true,
  wallpaper: true,
  gateway: null,
};

export type HostRecord = {
  id: string;
  name: string;
  address: string;
  port: number;
  /** The host's own login; empty when it uses its group's. */
  username: string;
  domain: string;
  /** A password for the host's own login is stored in the vault. */
  hasPassword: boolean;
  groupPath: string | null;
  lastConnectedMs: number | null;
  workspace: Workspace;
  position: number;
  rdp: RdpSettings;
  comment: string;
  gatewayUsername: string;
  gatewayDomain: string;
  hasGatewayPassword: boolean;
};

export type PasswordChange = { kind: 'keep' } | { kind: 'set'; value: string } | { kind: 'forget' };

export type HostDraft = {
  id: string | null;
  name: string;
  address: string;
  port: number;
  username: string;
  domain: string;
  groupPath: string | null;
  workspace: Workspace | null;
  password: PasswordChange;
  rdp: RdpSettings;
  comment: string;
  gatewayUsername: string;
  gatewayDomain: string;
  gatewayPassword: PasswordChange;
};

export type SaveFailure =
  | { kind: 'invalid'; field: string; problem: string }
  | { kind: 'vault-locked' }
  | { kind: 'error'; message: string };

export function listHosts(): Promise<HostRecord[]> {
  return invoke<HostRecord[]>('list_hosts');
}

export function saveHost(draft: HostDraft): Promise<HostRecord> {
  return invoke<HostRecord>('save_host', { draft });
}

export function deleteHost(id: string): Promise<void> {
  return invoke('delete_host', { id });
}

/** `DOMAIN\user`, or just the user. */
export function loginLabel(username: string, domain: string): string {
  return domain ? `${domain}\\${username}` : username;
}

// ── Groups and order ────────────────────────────────────────────────────────

export type GroupRecord = {
  workspace: Workspace;
  name: string;
  position: number;
  /** The login the group hands to its hosts; empty for none. */
  username: string;
  domain: string;
  hasPassword: boolean;
};

export function listGroups(): Promise<GroupRecord[]> {
  return invoke<GroupRecord[]>('list_groups');
}

export function createGroup(workspace: Workspace, name: string): Promise<GroupRecord> {
  return invoke<GroupRecord>('create_group', { workspace, name });
}

export function renameGroup(workspace: Workspace, from: string, to: string): Promise<void> {
  return invoke('rename_group', { workspace, from, to });
}

export function deleteGroup(workspace: Workspace, name: string): Promise<void> {
  return invoke('delete_group', { workspace, name });
}

/** The login a group hands down; an empty username removes it. */
export function setGroupLogin(
  workspace: Workspace,
  name: string,
  username: string,
  domain: string,
  password: PasswordChange,
): Promise<void> {
  return invoke('set_group_login', { workspace, name, username, domain, password });
}

export function moveGroup(
  workspace: Workspace,
  name: string,
  to: Workspace,
  before: string | null,
): Promise<void> {
  return invoke('move_group', { workspace, name, to, before });
}

export function moveHost(
  id: string,
  to: Workspace,
  group: string | null,
  before: string | null,
): Promise<HostRecord> {
  return invoke<HostRecord>('move_host', { id, to, group, before });
}

// ── Connecting ──────────────────────────────────────────────────────────────

export type ObservedCertificate = {
  /** `SHA256:…`, like ssh-keygen prints fingerprints. */
  fingerprint: string;
  subject: string;
  issuer: string;
  notBefore: string;
  notAfter: string;
  /** The certificate itself, base64 DER. */
  derBase64: string;
};

/** Mirrors `RdpError` plus the desktop layer's own kinds. */
export type ConnectFailure =
  | { kind: 'unknown-certificate'; observed: ObservedCertificate }
  | { kind: 'certificate-changed'; expected: string; observed: ObservedCertificate }
  | { kind: 'auth-failed'; message: string }
  | { kind: 'unreachable'; message: string }
  | { kind: 'timeout' }
  | { kind: 'negotiation'; message: string }
  | { kind: 'gateway'; message: string }
  | { kind: 'cancelled' }
  | { kind: 'protocol'; message: string }
  /** No login, or no password for it: ask, prefilled with what is known. */
  | { kind: 'login-required'; username: string; domain: string; fromGroup: boolean }
  /** The gateway needs a login the host doesn't have. */
  | { kind: 'gateway-login-required'; username: string; domain: string }
  | { kind: 'vault-locked' }
  | { kind: 'internal'; message: string };

export function asConnectFailure(error: unknown): ConnectFailure {
  if (typeof error === 'object' && error !== null && 'kind' in error) {
    return error as ConnectFailure;
  }
  return { kind: 'internal', message: String(error) };
}

/** A login typed into the connect dialog, for one attempt. */
export type TypedLogin = { username: string; domain: string; password: string };

export type Viewport = { width: number; height: number; scale: number };

/**
 * Open a remote desktop. `attempt` names the tab that connects, so closing it
 * can cancel exactly that attempt. `login` and `gatewayLogin` are what the
 * connect dialog asked for — sent for this one call and not kept anywhere.
 * `onData` gets the link with each message, so frames that arrive before the
 * session's id can already be acknowledged.
 */
export async function connectHost(
  id: string,
  attempt: string,
  viewport: Viewport,
  login: TypedLogin | null,
  gatewayLogin: TypedLogin | null,
  onData: (bytes: Uint8Array, link: FrameLink) => void,
  onEnd: EndHandler,
): Promise<{ session: SessionId; link: FrameLink }> {
  const link: FrameLink & { quiet(): void } = await openLink(
    attempt,
    (bytes) => onData(bytes, link),
    onEnd,
  );
  try {
    const session = await invoke<SessionId>('connect_host', {
      id,
      attempt,
      width: viewport.width,
      height: viewport.height,
      scale: viewport.scale,
      login,
      gatewayLogin,
    });
    return { session, link };
  } catch (error) {
    link.quiet();
    throw error;
  }
}

/** The user closed the tab or a dialog: stop the attempt that was waiting. */
export function cancelConnect(attempt: string): Promise<void> {
  return invoke('cancel_connect', { attempt });
}

/**
 * Trust the certificate the server just presented. Replacing one that was
 * already trusted needs `replace`: the user's explicit yes in the warning.
 */
export function trustCertificate(
  address: string,
  port: number,
  fingerprint: string,
  replace = false,
): Promise<void> {
  return invoke('trust_certificate', { address, port, fingerprint, replace });
}

/** Keep a login typed into the connect dialog as the host's own. */
export function setHostLogin(
  id: string,
  username: string,
  domain: string,
  password: string | null,
): Promise<HostRecord> {
  return invoke<HostRecord>('set_host_login', { id, username, domain, password });
}

// ── Vault ─────────────────────────────────────────────────────────────────

export type VaultStatus = 'absent' | 'locked' | 'unlocked';

export function vaultStatus(): Promise<VaultStatus> {
  return invoke<VaultStatus>('vault_status');
}

export type VaultState = {
  status: VaultStatus;
  remembered: boolean;
  /** Synced, and this device lost its copy of the account key: the kit's code is needed. */
  needsRecoveryCode: boolean;
  /** Needs an account key although this device isn't paired; a new master password frees it. */
  stranded: boolean;
};

export function vaultState(): Promise<VaultState> {
  return invoke<VaultState>('vault_state');
}

/** `remember`: this user account opens the vault without the master password. */
export function createVault(password: string, remember: boolean): Promise<void> {
  return invoke('create_vault', { password, remember });
}

/**
 * `remember` changes whether this device keeps the key; `null` leaves it.
 * `recoveryCode` is the recovery kit's code, for a synced vault whose account
 * key this device no longer has.
 */
export function unlockVault(
  password: string,
  remember: boolean | null = null,
  recoveryCode: string | null = null,
): Promise<void> {
  return invoke('unlock_vault', { password, remember, recoveryCode });
}

/** A new master password for a stranded vault this device still opens. */
export function repairVault(password: string, remember: boolean): Promise<void> {
  return invoke('repair_vault', { password, remember });
}

export function setVaultRemembered(remember: boolean): Promise<void> {
  return invoke('set_vault_remembered', { remember });
}

export function lockVault(): Promise<void> {
  return invoke('lock_vault');
}

// ── Import ────────────────────────────────────────────────────────────────

/** A file RDCMan itself had open, found in its settings. */
export type RdcManFile = { token: string; name: string; folder: string };

/** What a picked source holds, in counts. Never a host or a secret. */
export type ImportSummary = {
  /** What was read: a file name, or "3 files". */
  label: string;
  hosts: number;
  groups: number;
  logins: number;
  passwords: number;
  gateways: number;
  /** Writing this import needs an unlocked vault (it has passwords to seal). */
  needsVault: boolean;
  skipped: string[];
};

export type ImportReport = {
  hostsAdded: number;
  hostsSkipped: number;
  groupsAdded: number;
  loginsAdded: number;
  passwordsAdded: number;
  skipped: string[];
};

/** RDCMan's recently opened files, on Windows. */
export function rdcmanFiles(): Promise<RdcManFile[]> {
  return invoke<RdcManFile[]>('rdcman_files');
}

/** Asks for `.rdg` or `.rdp` files; null when the dialog was cancelled. */
export function pickImportFiles(kind: 'rdg' | 'rdp'): Promise<ImportSummary | null> {
  return invoke<ImportSummary | null>('pick_import_files', { kind });
}

/** Reads one of the files RDCMan had open. */
export function scanRdcmanFile(token: string): Promise<ImportSummary> {
  return invoke<ImportSummary>('scan_rdcman_file', { token });
}

/** Writes what the last pick or scan found, into `workspace`. */
export function runImport(workspace: Workspace): Promise<ImportReport> {
  return invoke<ImportReport>('run_import', { workspace });
}

// ── App ─────────────────────────────────────────────────────────────────────

/** A page just started: close whatever an earlier page left open. */
export function closeAllSessions(): Promise<number> {
  return invoke<number>('close_all_sessions');
}

export type UpdateInfo = { version: string; notes: string | null };

export function setUpdateChannel(channel: 'stable' | 'beta'): Promise<void> {
  return invoke('set_update_channel', { channel });
}

export function updateStatus(): Promise<UpdateInfo | null> {
  return invoke<UpdateInfo | null>('update_status');
}

export function checkForUpdates(): Promise<UpdateInfo | null> {
  return invoke<UpdateInfo | null>('check_for_updates');
}

/** Hands over to the downloaded setup; the app quits on success. */
export function installUpdate(): Promise<void> {
  return invoke('install_update');
}

export type ProjectPage = 'source' | 'releases' | 'issues' | 'license';

export function openProjectPage(page: ProjectPage): Promise<void> {
  return invoke('open_project_page', { page });
}

/** Full screen for the whole window; the page hides its own chrome. */
export function setFullscreen(on: boolean): Promise<void> {
  return invoke('set_fullscreen', { on });
}
