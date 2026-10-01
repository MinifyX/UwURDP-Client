/**
 * `uwurdp://connect/<host-id>` links (see `src-tauri/src/deep_link.rs`).
 *
 * Rust reads the link and keeps only the host's id; nothing else from a link
 * reaches the page. What happens with the id is decided here: the vault is
 * opened if it is locked, the host is looked up, and a host this device
 * doesn't know yet gets one sync pass to arrive. The connection itself is the
 * same as a double click on the host.
 *
 * No runtime imports: `pnpm test` runs this file in Node as it is.
 */

/** A link the app was opened with, as Rust hands it over. */
export type DeepLink = { kind: 'connect'; id: string } | { kind: 'invalid' };

/** How the sync pass for a link went. */
export type LinkSync = { kind: 'done' } | { kind: 'off' } | { kind: 'failed'; message: string };

export type LinkOutcome<H> =
  | { kind: 'connect'; host: H }
  /** The vault stayed locked. */
  | { kind: 'locked' }
  /** Not here, also after `sync` (which says whether there was a pass). */
  | { kind: 'unknown'; sync: LinkSync }
  | { kind: 'invalid' };

export type LinkSteps<H> = {
  /** Is the vault there and locked? */
  vaultLocked: () => Promise<boolean>;
  /** Ask for the master password; false when the person didn't open it. */
  unlock: () => Promise<boolean>;
  hosts: () => Promise<H[]>;
  /** One sync pass, finished. */
  sync: () => Promise<LinkSync>;
  /** The host isn't here; a sync pass starts. */
  onSyncing?: () => void;
};

/** What to do with a link: which host to connect, or why none. */
export async function resolveLink<H extends { id: string }>(
  link: DeepLink,
  steps: LinkSteps<H>,
): Promise<LinkOutcome<H>> {
  if (link.kind !== 'connect') return { kind: 'invalid' };
  const id = link.id.toLowerCase();
  if ((await steps.vaultLocked()) && !(await steps.unlock())) return { kind: 'locked' };

  const find = async () => (await steps.hosts()).find((host) => host.id.toLowerCase() === id);
  const here = await find();
  if (here) return { kind: 'connect', host: here };

  steps.onSyncing?.();
  const sync = await steps.sync();
  if (sync.kind === 'done') {
    const synced = await find();
    if (synced) return { kind: 'connect', host: synced };
  }
  return { kind: 'unknown', sync };
}
