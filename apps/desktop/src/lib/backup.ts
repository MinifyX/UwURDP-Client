/** Export files (`.uwurdp`): everything out, and back in. */

import { invoke } from '@tauri-apps/api/core';
import type { ImportReport } from './session';

export type BackupSummary = {
  hosts: number;
  groups: number;
  knownHosts: number;
  passwords: number;
};

export type BackupFailure =
  | { kind: 'vault-locked' }
  | { kind: 'password-required' }
  | { kind: 'password-wrong' }
  | { kind: 'error'; message: string };

export function asBackupFailure(error: unknown): BackupFailure {
  if (typeof error === 'object' && error !== null && 'kind' in error) {
    return error as BackupFailure;
  }
  return { kind: 'error', message: String(error) };
}

/** Opens a save dialog; `null` when it was cancelled. */
export function exportHosts(
  secrets: boolean,
  password: string | null,
): Promise<{ fileName: string; summary: BackupSummary } | null> {
  return invoke('export_hosts', { secrets, password });
}

export type PickedExport = {
  token: string;
  fileName: string;
  sealed: boolean;
  summary: BackupSummary | null;
};

/** Opens a file dialog; `null` when it was cancelled. */
export function pickExportFile(): Promise<PickedExport | null> {
  return invoke<PickedExport | null>('pick_export_file');
}

export function readExportFile(token: string, password: string | null): Promise<BackupSummary> {
  return invoke<BackupSummary>('read_export_file', { token, password });
}

export function importExportFile(token: string, password: string | null): Promise<ImportReport> {
  return invoke<ImportReport>('import_export_file', { token, password });
}
