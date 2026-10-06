/**
 * What this build of UwURDP can do (`build_info` in `src-tauri/src/system.rs`).
 *
 * Two builds come out of the same code: the downloads from GitHub, which
 * update themselves and fetch OpenH264 on request, and the Mac App Store
 * build, which is sandboxed and updated by the store (docs/app-store.md).
 * The page asks here before it shows anything only one of them has.
 *
 * Plain module state, no React: the answer never changes while the app runs.
 * Call {@link loadBuildInfo} once at start-up (before the first render, or in
 * an effect); {@link buildInfo} then answers synchronously. Until the answer
 * is in, and wherever the command is missing (the browser mock for
 * screenshots, an older Rust side), it answers with the GitHub build's
 * defaults.
 */

import { invoke } from '@tauri-apps/api/core';
import { platform } from './platform';

export type BuildInfo = {
  /** The Mac App Store build: sandboxed, no setup app, no self-update. */
  store: boolean;
  /**
   * UwURDP finds and installs its own updates. Off: hide the update channel,
   * "check now", "restart to update" and any text about the setup.
   */
  updates: boolean;
  /**
   * The H.264 setting can fetch Cisco's OpenH264. Off: hide the setting and
   * the Cisco notice. (A platform without Cisco's binary still reports
   * `unsupported` through `h264_status`.)
   */
  h264: boolean;
  /** The "Alle Laufwerke" entry of drive redirection means something (Windows, not the store). */
  allDrives: boolean;
  /** RDCMan's list of open files can be offered for a one-click import (Windows only). */
  rdcmanScan: boolean;
};

/** The GitHub build's answer, for wherever the Rust side cannot be asked. */
export function defaultBuildInfo(): BuildInfo {
  const windows = platform() === 'windows';
  return { store: false, updates: true, h264: true, allDrives: windows, rdcmanScan: windows };
}

let cached: BuildInfo | null = null;
let loading: Promise<BuildInfo> | null = null;

/** Asks the Rust side once; later calls get the same promise. Never rejects. */
export function loadBuildInfo(): Promise<BuildInfo> {
  loading ??= invoke<BuildInfo>('build_info')
    .then((info) => ({ ...defaultBuildInfo(), ...info }))
    .catch(() => defaultBuildInfo())
    .then((info) => {
      cached = info;
      return info;
    });
  return loading;
}

/** The answer, once {@link loadBuildInfo} has it; the GitHub build's defaults before. */
export function buildInfo(): BuildInfo {
  return cached ?? defaultBuildInfo();
}

/** Whether this is the Mac App Store build. Shorthand for `buildInfo().store`. */
export function isStoreBuild(): boolean {
  return buildInfo().store;
}
