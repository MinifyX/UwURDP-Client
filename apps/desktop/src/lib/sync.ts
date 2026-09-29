/**
 * Settings → Sync, the page's side: connecting a UwUSync server, pairing
 * devices, the device list — or signing in to UwULock instead, and the move
 * from one to the other.
 *
 * Every secret the page types — the master password, the codes — goes
 * straight into one command and is not kept. What comes back holds no secret
 * except the recovery code: right after the account was made, and when asked
 * for again with the master password.
 */

import { invoke } from '@tauri-apps/api/core';
import type { VaultStatus } from './session';

export type SyncReport = {
  pulled: number;
  pushed: number;
  conflicts: number;
  rounds: number;
  apply: { applied: number; rejected: number; hostKeyConflicts: number };
  withheld: Withheld;
};

/**
 * What the other devices' manifests say this one should have and does not:
 * the server is keeping records back or handing out old versions. While
 * `hostKeys` is set, host keys from other devices are not trusted.
 */
export type Withheld = { records: number; hostKeys: boolean };

export type SyncStatus = {
  paired: boolean;
  serverUrl: string | null;
  tlsFingerprint: string | null;
  deviceId: string | null;
  pairedMs: number | null;
  lastSyncMs: number | null;
  last: { atMs: number; report: SyncReport | null; error: string | null } | null;
  pending: number;
  running: boolean;
  vault: VaultStatus;
  deviceName: string;
  offering: boolean;
  withheld: Withheld;
  backend: Backend;
  lock: LockStatus;
};

/** Where this device syncs, if anywhere. */
export type Backend = 'none' | 'uwusync' | 'uwulock';

export type LockStatus = {
  serverUrl: string | null;
  email: string | null;
  /** The session ended: the master password is needed to go on syncing. */
  needsSignIn: boolean;
  /** UwULock's realtime channel is open. */
  live: boolean;
  liveRefused: boolean;
  signedInMs: number | null;
  /** A move from UwUSync began and did not finish. */
  moveStartedMs: number | null;
  /** Moved, and this device may still be removed from UwUSync. */
  leftBehind: boolean;
};

/** One way of two-step login the UwULock account has set up. */
export type TwoFactorMethod = {
  provider: number;
  kind: 'authenticator' | 'email' | 'yubikey' | 'duo' | 'u2f' | 'webauthn' | 'other';
  supported: boolean;
  /** For email codes: the masked address the code goes to. */
  hint: string | null;
};

export type TwoFactorInput = { provider: number; code: string; remember: boolean };

export type MoveReport = {
  read: number;
  copied: number;
  alreadyThere: number;
  newerThere: number;
  unreadable: number;
};

export type Difference = { id: string; kind: string; problem: 'missing' | 'older' };

export type LockOutcome =
  | { kind: 'signed-in'; madeSpace: boolean }
  | { kind: 'moved'; report: MoveReport; lastDevice: boolean }
  | { kind: 'two-factor'; methods: TwoFactorMethod[]; message: string | null };

export type SyncFailure =
  | { kind: 'vault-locked' }
  | { kind: 'password-wrong' }
  | { kind: 'bad-code' }
  | { kind: 'unreachable'; message: string }
  | { kind: 'refused'; message: string }
  | { kind: 'pairing-failed'; message: string }
  | { kind: 'sign-in' }
  | { kind: 'login-refused'; message: string }
  | { kind: 'keys-lost' }
  | { kind: 'no-key-pair' }
  | { kind: 'weaker-kdf'; message: string }
  | { kind: 'move-check'; differences: Difference[] }
  | { kind: 'error'; message: string };

export function asSyncFailure(error: unknown): SyncFailure {
  if (typeof error === 'object' && error !== null && 'kind' in error) {
    return error as SyncFailure;
  }
  return { kind: 'error', message: String(error) };
}

export type Connected = {
  recoveryCode: string;
  serverUrl: string;
  tlsFingerprint: string | null;
};

export type Offer = {
  spoken: string;
  pasteable: string;
  serverUrl: string;
  tlsFingerprint: string | null;
  expiresMs: number;
};

export type Device = {
  id: string;
  name: string;
  createdMs: number;
  lastSeenMs: number | null;
  revokedMs: number | null;
  current: boolean;
};

export const syncStatus = () => invoke<SyncStatus>('sync_status');

export const syncConnect = (code: string, password: string, name: string) =>
  invoke<Connected>('sync_connect', { code, password, name });

export const syncJoin = (
  code: string,
  password: string,
  name: string,
  serverUrl: string | null,
  fingerprint: string | null,
) => invoke<void>('sync_join', { code, password, name, serverUrl, fingerprint });

export const syncOffer = (password: string) => invoke<Offer>('sync_offer', { password });
export const syncWaitForDevice = () => invoke<{ name: string }>('sync_wait_for_device');
export const syncCancelOffer = () => invoke<void>('sync_cancel_offer');
export const syncDevices = () => invoke<Device[]>('sync_devices');
export const syncRevoke = (deviceId: string, password: string) =>
  invoke<void>('sync_revoke', { deviceId, password });
export const syncNow = () => invoke<void>('sync_now');
export const syncDisconnect = (password: string) => invoke<void>('sync_disconnect', { password });
/** The recovery kit again, on a paired device that kept the account key. */
export const syncRecoveryCode = (password: string) =>
  invoke<Connected>('sync_recovery_code', { password });

/**
 * Sign in to UwULock and sync through it. `twoFactor` once the server asked
 * for a code.
 */
export const lockSignIn = (
  serverUrl: string,
  email: string,
  password: string,
  twoFactor: TwoFactorInput | null,
) => invoke<LockOutcome>('lock_sign_in', { serverUrl, email, password, twoFactor });

export const lockSendEmailCode = (serverUrl: string, email: string, password: string) =>
  invoke<void>('lock_send_email_code', { serverUrl, email, password });

/** The one-click move from UwUSync to UwULock. */
export const lockMove = (
  serverUrl: string,
  email: string,
  password: string,
  twoFactor: TwoFactorInput | null,
) => invoke<LockOutcome>('lock_move', { serverUrl, email, password, twoFactor });

/** After the move: remove this device from UwUSync, or leave it listed there. */
export const lockLeaveUwusync = (revoke: boolean) => invoke<void>('lock_leave_uwusync', { revoke });
export const lockForgetMove = () => invoke<void>('lock_forget_move');
export const lockSignOut = (password: string) => invoke<void>('lock_sign_out', { password });

/** A pasted setup code, from `uwusync-server invite`. */
export const isSetupCode = (text: string) => text.trim().startsWith('uwu1_');
/** A pasted pairing code, from "add a device" on another device. */
export const isPasteablePairing = (text: string) => text.trim().startsWith('uwu2_');
