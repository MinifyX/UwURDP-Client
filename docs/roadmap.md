# Roadmap

My wish list, without dates. The order is deliberate: the biggest risk goes
first, not the nicest feature.

## M0 · Foundation

Tauri shell, `xterm.js`, `russh` connect, password and key auth, the SQLite
schema, a host list.

**The throughput spike is done** (2026-09-16, [results](m0-spike.md)). The IPC
channel carries a real terminal: 41–46 MiB/s with no UI frame above 7 ms, which
is xterm.js' own parse speed. The WebSocket fallback is dropped. End-to-end flow
control turned out to be mandatory — without it xterm.js discarded about 9 MiB
of a 64 MiB flood — and is built in.

**The rest of M0 is done too.** SSH sessions run on that data path, with
password and key login — OpenSSH, PEM and PuTTY `.ppk` keys, encrypted ones
included. Hosts, identities and trusted host keys live in SQLite. Host keys are
checked on first contact and blocked when they change, which this roadmap had
in M1; connecting without that check would have been worse than not connecting
at all. An end-to-end run drives the real app against a real SSH server and
caught four bugs no other test could see — see
[architecture](architecture.md#how-it-is-tested).

## M1 · Daily driver

The point where I can stop using anything else.

- **Termius import is done.** Termius has no export, so UwURDP reads its local
  Electron database directly — hosts, groups, logins, keys with their
  passphrases, the host keys Termius already trusts, snippets — and writes them
  into the vault. It found 14 hosts, 16 logins, 2 keys and 138 known-host keys
  on the first real run, and skips nothing it can read. See
  [architecture](architecture.md#import).
- **PuTTY, KiTTY and `ssh_config` import are done.** PuTTY and KiTTY sessions
  come straight out of the registry (`.ppk` paths and faux-folder groups and
  all); `~/.ssh/config` is read with its `Include` directives followed. All keep
  keys as files and type their passwords, so those imports hold no secrets and
  need no vault. Import is source-agnostic: pick a source, preview it, write it.
  Since 0.1.0-beta.8 also from a folder: a portable KiTTY's `Sessions`, or
  `.reg` exports of PuTTY and KiTTY.
- Still to import: the host keys PuTTY already trusts, whose registry format is
  PuTTY's own and wants a real dump to verify before it is promised. And
  ProxyJump: `ssh_config` records the jump host, but linking the chain is the
  ProxyJump feature below, not the import.
- **Tabs are done.** Every connection gets its own tab, several to the same
  server too (a click shows an open one, the context menu opens another), each
  with its own terminal and its own login; closing one leaves the others alone. Keyboard: Ctrl+Shift+T/W/D, Ctrl+Tab, Ctrl+Shift+1…9. See
  [architecture](architecture.md#tabs).
- **Settings are done**: theme and animations, terminal font size, cursor,
  scrollback, copy and paste keys, locking the vault, the update channel.
- **Workspaces and drag and drop are done** (0.1.0-beta.2): Private and
  Business in one sidebar, groups as records of their own, and hosts and groups
  sorted by dragging, within a workspace and across.
- **Keyword highlighting is done**: errors, warnings, success words, addresses,
  and rules of your own, as xterm.js decorations that never touch the bytes.
  Ctrl+mouse wheel sets the text size.
- **The password helper is done**: when a prompt like sudo's shows up in a
  session that logged in with a password, a pill offers to type it (or
  Ctrl+Shift+P). It asks every time; nothing is typed on its own.
- **System detection is done**: after login, one small probe (`uname`,
  `/etc/os-release`) and the SSH banner tell Ubuntu from Windows from Cisco,
  and the host list shows it as an icon.
- **Export and import of UwURDP's own file are done**: everything in one
  `.uwurdp` file, sealed with a password when it carries secrets.
- Still to do: splits, agent auth (Pageant, `\\.\pipe\openssh-ssh-agent`,
  1Password), ProxyJump chains
- Snippets, broadcast input, themes, the command palette
- **Connecting with vault credentials is done.** An imported host logs in with
  the password or key sealed in the vault — key material passed to the engine in
  memory, no file on disk — and when the vault is locked the app asks for the
  master password and reconnects. The host key is still checked before any
  secret is sent.
- **First Nyu scenes are done**: vault, connecting, files, keys, goodbye, the
  laser pad in UwUKeygen. Still to do: the playful/neutral string split

## M2 · Vault and sync

- **The vault's local crypto is done, early** — the Termius import had to put
  its secrets somewhere. Master password → Argon2id → wrapped vault key,
  XChaCha20-Poly1305 per record, created and unlocked from the app.
- **Passwords and keys per host are done** (0.1.0-beta.2): the host form keeps a
  password or a vault key, a typed password can be saved on the way in, and
  keys are generated, imported, exported and assigned from Settings → Vault &
  Keys.
- **Unlock with the Windows account is done**: the vault key, sealed with DPAPI
  for the signed-in user, opens the vault at start — one master password once,
  then never again on this machine, until you turn it off. Still to do here:
  recovery kit, Windows Hello and biometric unlock, auto-lock.
- **The sync foundation is done**, before there is a server to talk to: a
  record travels as an envelope whose header is sealed with its payload, its
  version is the server's sequence number, the outbox is a column in the
  database rather than a queue in memory, and hosts point at their group by id
  so a rename is one record. One pass pushes, pulls and merges; conflicts
  resolve per record by the hybrid logical clock, with a delete beating a
  concurrent edit. `MemoryServer` in `uwurdp-sync` holds the server's rules as
  running code, and sixteen tests drive two real devices against it — including
  a server that flips a tombstone flag or replays an old version, which gets
  nowhere. See [architecture](architecture.md#sync).
- **The server's first half is done**, in
  [UwUSSH-Server](https://github.com/MinifyX/UwUSSH-Server): accounts, devices,
  records with the version check, an event stream, rate limits, a command line,
  a Docker image and nightly backups — plus its own TLS certificate, whose
  fingerprint a device pins like an SSH host key, and the post box two devices
  pair through. Eighty-one tests, seventeen of them over real HTTP.
- **The client half is done too**, bar the interface: the account key (so a
  stolen server database is worth nothing), the transport with the server's key
  pinned like an SSH host key, a keypair per device, and pairing by three
  spoken words over SPAKE2 — with the secret handed over only after the other
  side has proved it derived the same key. The server repository's
  `tests/client.rs` runs both halves against each other.
- **Settings → Sync is done** (0.1.0-beta.8): connect a server with its setup
  code, the recovery kit shown once and closable only when saved, adding a
  device with a code read out or pasted, the device list with revoking by
  master password and the honest sentence about what a revoked device still
  knows, what the last pass did, and leaving again. A worker thread keeps the
  device in step. An end-to-end phase drives two app instances against a real
  server. See [architecture](architecture.md#settings--sync).
  Since 0.1.0-beta.9 a server that refuses the connect leaves the vault as it
  was, a vault an earlier refusal stranded gets a new master password on a
  device that remembered it, and every paired device shows the kit again for
  the master password.
- **The server is ready to release**: `install.sh` sets it up with one
  question, `update.sh` updates it with a backup first and the old version back
  if the new one does not come up, and CI runs both on a real Docker before an
  image is published. Reviewed by someone who had not written it; every finding
  fixed, three of them in the protocol (revoking another device takes the
  master password, the enrolment token travels in a body, pairing sides are
  bound).
- **0.1.1, the first release without "beta"**, after a security review of
  client and server: each device publishes a sealed manifest, so a server that
  serves old versions or holds records back is caught (and synced host keys are
  not trusted meanwhile); a floor for Argon2 costs a server hands out; adding a
  device takes the master password; pairing messages sealed per direction;
  imports only trust host keys for hosts they add; files private to the user
  on Unix. The server got connection deadlines and caps, a disk cap, a stricter
  `update.sh` and a token limit. Still open from the review: rotating the vault
  key when a device is revoked.
- **0.1.2**: a tab in the background keeps its terminal size (it was fitted to
  about ten columns while hidden, and the server wrapped its output to that),
  scrollbars in the app's colours, and a setup for Windows on ARM, built
  natively in CI and updated through its own feed entry (`windows-aarch64`).
- Still to come: a way to push everything again after the server was restored
  from a backup

## M3 · SFTP and tunnels

- **The file browser is done, early** (0.1.0-beta.2): this computer on the
  left, the server on the right, over SFTP with the host's login — or an SMB
  share on the same host. Drag and drop between the panes and from Explorer,
  transfers with progress and cancel, new folder, rename, delete, permissions.
  **Root** reconnects the SFTP side through `sudo` and starts at `/`. See
  [architecture](architecture.md#files).
- **UwUKeygen is done**: PuTTYgen with Nyu, RSA 2048 by default and everything
  else under Advanced, in the host form and as its own app the installer adds.
- Open a remote file in the local editor and write it back
- Port forwarding manager: local, remote, dynamic, with autostart per host

## M4 · Polish

- **The installer and the updater are done, early**, for the first beta
  (0.1.0-beta.1): UwURDP's own Windows setup, the same as UwUMail's, and
  signed automatic updates in a Stable and a Beta channel. Releases and feeds
  live in this repository, so there is no `UwURDP-Releases`. See
  [architecture](architecture.md#installer-and-updates).
- **Linux and macOS builds are done** (0.1.0-beta.8): the same setup with Nyu
  for macOS on Apple silicon and Intel and for Linux, with automatic updates
  on all three, built by CI and signed where the key is. Plus a `.deb`.
- **Windows on ARM** (0.1.2), then **one release format** for all three
  UwUSuite apps: download names without a version, one universal macOS disk
  image, `.deb` and `.rpm` for x64 and arm64 that update themselves, the AUR
  package `uwurdp-bin`, and a portable Linux folder.
- Onboarding, accessibility pass, the full Nyu scene set

## M5 · Homelab

Where the distance to the commercial clients actually opens up.

- **Tailscale / Headscale import** — tailnet devices as a host list, MagicDNS
  names, live reachability. Probably the strongest single feature for this
  audience.
- **Proxmox import** — nodes, LXC and VMs via the API, groups mirroring the
  cluster
- **Netbox import** — inventory as the source of truth
- Local shell tabs (PowerShell, WSL, cmd) and serial console
- Session recording with asciinema export, persistent searchable scrollback
- **UwURDP as an SSH agent** — other programs use the vault's keys through a
  named pipe or unix socket, with a confirmation prompt per signature
- SSH CA certificates; FIDO2 (`ed25519-sk`) — needs a look at `russh` support
  first

## M6 · Mobile and teams

- Android and iOS, since Tauri 2 does mobile and `russh` runs there
- Shared vaults with per-host ACLs

## Open questions

Things I haven't decided, roughly in the order they'll bite:

1. ~~**PPK parser**~~ — settled: russh reads `.ppk` v2 and v3 natively,
   encrypted or not, so there is nothing to write.
2. **What can the Termius import actually reach?** Verify against a real export
   before promising it in the README. If it's CSV only, that belongs in the
   README honestly.
3. ~~**Server default database**~~ — settled: SQLite, one volume, one backup.
   Postgres would be a second driver for a benefit nobody with three devices
   feels.
4. **Sync `known_hosts`?** — probably yes, on by default; it's the most common
   friction point when switching devices. The merge rule for two devices that
   trusted different keys for one machine is already in place.
5. **File-based sync as a third option** — would Syncthing or Nextcloud as a
   transport, next to "local only" and "own server", be the shortest path for
   most homelabs?
6. **Shared vaults** — leave them out of v1 entirely, or keep the data model
   ready? `vault_id` already exists, so keeping the door open costs nothing.
