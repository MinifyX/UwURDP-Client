# Architecture

How the pieces fit together. The German [KONZEPT.md](../KONZEPT.md) has the
short version with the reasons.

UwURDP is a fork of [UwUSSH](https://github.com/MinifyX/UwUSSH-Client). The
store, the vault, sync, the installer and the updater came over almost
unchanged; the terminal went, and an RDP engine took its place. Where this
document says "as in UwUSSH", UwUSSH's architecture document has the long
story.

## The stack

| Layer     | Choice                                  | Why                                                                                          |
| --------- | --------------------------------------- | -------------------------------------------------------------------------------------------- |
| Shell     | Tauri 2                                 | Same as UwUSSH and UwUMail. Small, WebView2 on Windows, WebKit elsewhere.                    |
| UI        | React + TypeScript, Node 24, pnpm 11    | UwUSSH's stack, so tokens, components and Nyu carry over.                                    |
| RDP       | IronRDP 0.17 (Devolutions)              | Pure Rust, async, maintained, and the only RDP stack that isn't FreeRDP behind an FFI layer. |
| TLS       | `rustls` with _ring_                    | No OpenSSL, no second TLS stack; the certificate check is ours.                              |
| Clipboard | `arboard`                               | Text on Windows, macOS and X11/Wayland from one crate.                                       |
| Sound     | `cpal`                                  | Plays the server's PCM on WASAPI, CoreAudio and ALSA.                                        |
| Store     | `rusqlite` with WAL                     | One file, offline-first, trivial to back up.                                                 |
| Crypto    | `argon2`, `chacha20poly1305`, `zeroize` | Established RustCrypto crates. Nothing home-made.                                            |
| Sync      | the UwUSync server, unchanged           | One server for both apps; UwURDP speaks its protocol as it is.                               |

## The pieces

```
apps/desktop        Tauri app: React UI, Rust commands, the e2e run
apps/setup          installer, updater, uninstaller (Windows, macOS, Linux)
crates/uwurdp-core    RDP engine on IronRDP — knows nothing about Tauri
crates/uwurdp-import  RDCMan .rdg + RDCMan.settings, mstsc .rdp, DPAPI
crates/uwurdp-store   SQLite: hosts, groups, logins, certificates, export
crates/uwurdp-vault   Argon2id, key wrapping, record encryption
crates/uwurdp-sync    sync client: outbox, merging, pairing, pinning
crates/uwurdp-proto   payloads and wire types shared with the server
```

`uwurdp-core` has no idea there is a web page. The app implements its
`FrameSink` over a loopback WebSocket; the tests implement it over a `Vec`.

## The secret boundary

```
┌─ WebView ─────────────┐ │ ┌─ Rust ────────────────────────────────┐
│ Host tree, tabs       │ │ │ SessionManager → IronRDP → RDP host   │
│ Overview, dialogs     │ │ │ Vault          → passwords            │
│ <canvas> per desktop  │ │ │ SyncEngine     → outbox, merging      │
│                       │ │ │ Store          → SQLite               │
│ no stored passwords   │ │ │                                       │
└───────────────────────┘ │ └───────────────────────────────────────┘
        ↓ connect, resize (commands); input, ack (the desktop's socket)
        ↑ desktop pixels, pointer, closed (the desktop's socket, binary)
```

Passwords from the vault never cross that line. The page asks Rust to connect
host X; Rust resolves the login, unseals the password, and hands it to
CredSSP. What does cross goes the other way: a password you type into the
login dialog, on its way to Rust. Imported passwords are the same — the import
preview the page sees is built from a type that cannot be serialized with its
password in it.

The page is still inside the trust boundary: it can type into every open
desktop. What protects it is that it only ever runs UwURDP's own code, under a
strict content security policy and a frozen prototype.

## Connecting

The order matters for security, so it is fixed in `uwurdp-core::connect`:

1. A **TCP probe** from the app first (five seconds). A host that doesn't
   answer fails here, before anyone is asked for a password.
2. TCP connect, then **X.224** negotiation: CredSSP when NLA is on (the
   default), TLS only when it's off for an old server.
3. **TLS** (rustls with _ring_), then the **certificate check**. An unknown or
   changed certificate ends the connection here: the only thing the server has
   seen so far is the user name in the routing cookie.
4. **CredSSP with NTLM.** UwURDP drives CredSSP itself rather than letting
   IronRDP finish the connection, so an error during authentication is reliably
   reported as a rejected login. Kerberos isn't offered: IronRDP's Kerberos
   path needs an HTTP client for KDC proxies that would pull a second TLS stack
   into the app. NTLM works against every Windows host that allows it, domain
   members reached by IP included.
5. The rest of the RDP connection sequence: capabilities, licensing, channels.

Then one task per session owns the connection.

Two choices in the capabilities are deliberate:

- **RemoteFX is the only codec advertised.** IronRDP's defaults also list
  QOI whenever any crate in the build turns on its feature (the test server
  does), and advertising a codec the client can't decode means a black
  screen. Windows servers speak RemoteFX anyway.
- **No bulk compression.** IronRDP can't carry the decompressor across a
  deactivation-reactivation, which is every resize, and a fresh one falls out
  of step with the server's history.

## The frame path

This is the part where Tauri could have hurt: a desktop is megabytes of pixels,
and they have to cross the IPC boundary into a web page.

```
server ──TLS──▶ session task ──▶ desktop image (RGBA) ──▶ dirty region
                     ▲                                        │ ≤ 1 per 16 ms,
                     │ input, resize, clipboard               │ < 2 unacked
                     │                                        ▼
page ──socket───▶ SessionManager            WebSocket 127.0.0.1 (binary) ──▶ page
                                                   putImageData on a <canvas>
```

The session task decodes server updates into one RGBA image of the whole
desktop and records which rectangles changed. RemoteFX alone reports one
rectangle per 64×64 tile, so a scrolling window yields hundreds a second;
sending each on its own would cost one IPC crossing and one `putImageData`
each. Instead the dirty region merges rectangles that overlap or nearly touch
(when the union doesn't waste much area) and collapses into its bounding box
when it gets busy. At most every 16 ms the task sends the _current_ pixels of
what is dirty.

### The socket

Tauri's IPC is fine for commands and wrong for pixels. A channel message over
1 KiB is an `eval` on the main thread plus a fetch through the webview's
custom protocol, and input or acks as synchronous commands queue on that same
main thread; a full-HD frame is 8 MB. So each desktop gets a **WebSocket on
127.0.0.1** instead (`src-tauri/src/frames.rs`):

- The app listens on a random port and makes a random 256-bit token at start;
  the page gets both through the `frame_socket` command and the CSP allows
  `ws://127.0.0.1:*` and nothing else.
- Before `connect_host` the page opens a socket with the token and the
  attempt's name. The app registers it and answers `ready`; only then does the
  page connect, and `connect_host` takes that socket for the session.
- App → page: the messages below as binary frames, then a close frame when
  the session is over. Page → app: `[1]` acknowledges one `BITMAPS` message,
  a text message is a JSON array of input events — both in order, and neither
  touches Tauri's main thread.

### Messages

One message per send, little-endian, the first byte says what it is:

| Kind | Message            | Payload                                                         |
| ---- | ------------------ | --------------------------------------------------------------- |
| 1    | `BITMAPS`          | u16 count; per rectangle u16 x, y, w, h, then w·h·4 bytes RGBA  |
| 2    | `DESKTOP_SIZE`     | u16 width, height — after connecting and every resize           |
| 3    | `POINTER_BITMAP`   | u16 hot_x, hot_y, w, h, then RGBA                               |
| 4    | `POINTER_DEFAULT`  | —                                                               |
| 5    | `POINTER_HIDDEN`   | —                                                               |
| 6    | `POINTER_POSITION` | u16 x, y                                                        |
| 7    | `CLOSED`           | JSON `{"reason":"logoff\|disconnect\|server\|error","message"}` |

Pixels are straight RGBA, row-major, without padding — exactly what
`ImageData` wants, so the page copies nothing. One `BITMAPS` message is capped
at 32 MiB (a full 4K frame fits); anything bigger goes as horizontal strips.

### Flow control

UwUSSH learned this the hard way: sending returns once a message is queued,
not once the page has handled it, so without acknowledgements a slow page
silently piles up work. UwURDP builds the same
lesson in from the start:

- The page acknowledges every `BITMAPS` message after drawing it.
- The session sends a new one only while **fewer than two** wait for an
  acknowledgement. Everything else (pointer, size, closed) goes out at once.
- **Nothing is dropped.** While the session waits, changes keep accumulating in
  the dirty region, and the next message carries the current pixels of all of
  them. A slow page doesn't lose updates, it gets coarser ones.

## Input

- **Keys go by physical position.** The page maps `KeyboardEvent.code` to PS/2
  set-1 scancodes, so the server applies its own keyboard layout — the same
  as mstsc. A German server with a German layout gets `ä` from the `ä` key,
  whatever the local layout says. Characters without a scancode fall back to
  Unicode events.
- **Mouse** moves, buttons and the wheel (120 per notch, horizontal too).
- IronRDP's input database tracks what is held, and **losing focus releases
  everything** the server believes is down — otherwise alt-tabbing away leaves
  a stuck Alt on the remote side.
- **Shortcuts.** Inside a desktop every key belongs to the server, Ctrl+Tab and
  Ctrl+Shift+Esc included. Only the combinations mstsc and RDCMan reserve stay
  with the app: Ctrl+Alt+End sends Ctrl+Alt+Del, Ctrl+Alt+Break toggles full
  screen, Ctrl+Alt+Home takes the keyboard back, Ctrl+Alt+PgUp/PgDn switch
  tabs. Outside a desktop the usual ones work too: Ctrl+Shift+O overview,
  Ctrl+Shift+W/D close and duplicate a tab, Ctrl+Shift+1…9, Ctrl+, settings.

## Display

- **Fit** (the default): the desktop takes the tab's size when it connects
  and follows **the window** through the DisplayControl virtual channel, like
  mstsc's dynamic resolution. Every resize is a deactivation-reactivation; the
  page gets `DESKTOP_SIZE` and a full frame. So only the window changing size
  (or full screen) asks for one: switching tabs, the overview, a notice bar or
  the sidebar never do, and a tab that connects in the background is sized
  for the area it will be shown in. In the first seconds after connecting the
  desktop may still fit itself once, when the notice or dialog of connecting
  goes away. Servers without DisplayControl keep their size and the page
  scales.
- **Fixed** width × height, and **full screen**, with an mstsc-like connection
  bar at the top.
- A desktop that doesn't fit the tab is **scaled down** (smart sizing, the
  default) or **scrolled**.

## Clipboard and sound

- **Clipboard:** plain text both ways over CLIPRDR. IronRDP's callbacks run in
  the middle of processing a PDU, so they never touch the OS clipboard
  themselves; a small worker thread owns it through `arboard`, notices local
  changes once a second and when the window gets focus, and answers the
  server's requests. A broken clipboard is logged and swallowed, never takes
  the session down. Files through the clipboard aren't supported yet.
- **Sound:** RDPSND, played locally through `cpal` as PCM. Opus is off. The
  backend is ours (`uwurdp-core/src/audio.rs`), not `ironrdp-rdpsnd-native`'s:
  that one never starts its stream, so on Windows nothing played and every
  wave piled up in memory until the app ran out of it. Ours starts the stream
  and keeps at most half a second of sound waiting; older sound is dropped,
  since the server doesn't wait for playback. "Leave it on the server" and
  "off" both tell the server not to redirect sound — IronRDP can't send the
  flag for the first one yet.

## Failures and logs

- Every session runs in its own task. A panic in one (a decoder tripping over
  what a server sent, say) ends that session with "internal error" and leaves
  the others running; the release build unwinds instead of aborting for this.
- The app logs to `uwurdp.log` in its log folder
  (`%LOCALAPPDATA%app.uwurdp.desktoplogs` on Windows,
  `~/Library/Logs/app.uwurdp.desktop` on macOS,
  `~/.local/share/app.uwurdp.desktop/logs` on Linux), the previous run's as
  `uwurdp.old.log`, at most 20 MB per run. Panics land there with their
  location. `UWURDP_LOG` takes a `tracing` filter (default
  `uwurdp=debug,warn`).

## Certificates

An RDP server's certificate is almost always self-signed, so checking it
against a CA would mean either always failing or always clicking "accept".
UwURDP treats it the way SSH treats host keys: **trust on first use**.

- The fingerprint is the SHA-256 of the leaf certificate's DER, shown as
  `SHA256:…` like `ssh-keygen` prints it — plus the **SHA-1 thumbprint**
  Windows shows in `certlm.msc` and the RDP listener's settings, so you can
  compare it with the server without converting anything.
- **First contact asks.** A **changed certificate blocks** with a plain
  warning, old and new fingerprint side by side, focus on the safe button.
- The check runs **before any credential is sent**. The TLS handshake
  signatures are verified; chain and host name aren't, because the pin replaces
  them.
- Trusted certificates live in the `known_hosts` table inherited from UwUSSH,
  with algorithm `x509`, and **sync** like everything else. While a server
  can't prove it holds nothing back (see manifests below), certificates that
  came from sync aren't trusted, and UwURDP asks again.

## Logins and inheritance

A login is its own record — user, domain, and a password sealed in the vault —
because one login serves forty hosts, and a password change should be one edit.

- A host has **its own login**, or **inherits its group's** — RDCMan's
  "inherit from parent", one level deep. Right-click a group → **Group
  login…** sets it.
- **No login at all?** The connect dialog asks, prefilled with what is known,
  with "save for this host".
- A **login the server rejects** brings the login dialog back, saying so.
- A **locked vault** asks for the master password first and then connects.

RDCMan's deeper inheritance (a login inherited through three levels of groups)
is resolved at import time, so every host arrives with the login it would have
used in RDCMan.

## Tabs, overview, reconnect

- Each desktop gets its own **tab**, several to one host too (a click shows an
  open one, the context menu or Ctrl+Shift+D opens another).
- The **overview** (Ctrl+Shift+O) shows every open session as a tile with
  its state — or one group, including its hosts that aren't connected, each
  with a connect button. No live pictures: redrawing them cost every session
  time and they said little the name doesn't.
  **Connect all** and **Disconnect all** work on a group.
- **Disconnecting keeps the tab**, with the last picture dimmed and a
  reconnect button. A connection that **drops** reconnects once on its own;
  if that fails, the tab stays and waits for you.

## Data model

The SQLite schema is UwUSSH's V1–V6 plus one migration of its own, **V7**:

```sql
ALTER TABLE identities  ADD COLUMN domain TEXT NOT NULL DEFAULT '';
ALTER TABLE hosts       ADD COLUMN rdp TEXT;             -- RdpSettings as JSON, NULL = defaults
ALTER TABLE hosts       ADD COLUMN comment TEXT NOT NULL DEFAULT '';
ALTER TABLE hosts       ADD COLUMN gateway_identity_id TEXT REFERENCES identities (id);
ALTER TABLE host_groups ADD COLUMN identity_id TEXT REFERENCES identities (id);
```

The sync payloads grow the same way:

- **HostPayload** gains `rdp: RdpSettings` — `display` (`fit`, `fixed`,
  `fullscreen`), `width`, `height`, `smartSizing`, `colorDepth`, `audio`
  (`local`, `remote`, `off`), `clipboard`, `admin`, `nla`, `wallpaper`, and
  `gateway { address, port, useHostLogin, bypassLocal }` — plus `comment` and
  `gateway_identity_id`.
- **GroupPayload** gains `identity_id`, the group login.
- **IdentityPayload** gains `domain`.

Every field has a default, and modes are strings rather than enums, so a record
from an older or newer build never fails: an unknown display mode shows as
`fit`. **Unknown fields survive round trips** — a build that doesn't know a
field keeps it and writes it back.

Some tables still carry UwUSSH's shape (keys, snippets); RDP doesn't use them,
and they stay so the protocol stays the same.

**What stays local** and never syncs: where this device stands with its server,
the vault key sealed for this device ("remember on this device"), when a host
was last connected, and the app's own settings (look, what
opens on start), which live in the web view's storage.

## Vault

As in UwUSSH: master password → Argon2id → master key, which wraps a random
vault key; every secret is its own record under XChaCha20-Poly1305 with the
record's id and type as associated data. "Remember on this device" seals the
vault key with **DPAPI** for the Windows account, or with a key of UwURDP's
own in the **Keychain** (macOS) or the **Secret Service** (GNOME Keyring,
KWallet); without a Secret Service, in a file only the user can read.

## Sync and the shared server

UwURDP has no server of its own. It speaks the wire protocol of
[UwUSync-Server](https://github.com/MinifyX/UwUSync-Server) unchanged — the
server only sees ids, sequence numbers and sealed blobs, so it never needed to
learn what an RDP host is.

- **Use a separate account on the same server.** `docker compose exec uwusync
uwusync-server invite` prints a new `uwu1_…` setup code; **Settings → Sync →
  Connect a server** takes it. Sharing one account between UwUSSH and UwURDP
  would mix two apps' records in one vault.
- **Zero knowledge.** Records are sealed with the vault key before they leave.
  The vault key is wrapped with the master password _and_ an account key that
  exists only on paired devices and in the recovery kit, so the server's copy
  is worth nothing on its own.
- **Pairing** a new device uses SPAKE2 over a short code (an id and three
  words); the secret goes over only after the other side proved it derived the
  same key. The master password is typed on each device and never sent.
- **Manifests:** each device publishes a sealed list of what it holds, so a
  server that serves old versions or holds records back is caught.
- **Conflicts** resolve per record by hybrid logical clock, a delete beating a
  concurrent edit. The outbox is a column in the database, not a queue in
  memory.
- **Revoking** a device takes the master password.

## Import

`uwurdp-import` turns every source into one `ImportBundle`, so the preview, the
duplicate check and the writer never learn which tool the data came from.
Credentials are deduplicated: a profile forty hosts share arrives once.

### RDCMan

`.rdg` files from version 2.2 through 2.93:

- **Groups** nested any depth are flattened to `Parent / Child`, since UwURDP
  groups are one level.
- **Logon credentials**, including credential profiles of File scope (in the
  `.rdg`) and Local scope (in `RDCMan.settings`), with RDCMan's inheritance
  resolved down the tree.
- **Settings:** display size, colour depth, console session, sound,
  clipboard, gateway, and comments. Settings UwURDP has no field for are
  appended to the host's comment rather than lost. Smart groups are skipped.
- **Passwords** are DPAPI-decrypted, which only works on the Windows account
  that saved them. Files encrypted with a certificate keep their users and
  drop the passwords.
- The files RDCMan had open are **listed directly**, read from
  `RDCMan.settings`, so there is nothing to browse for.

### mstsc `.rdp`

UTF-16 or UTF-8, several at once. `password 51:b:…` is DPAPI too, with the
same one-account rule.

### UwURDP's own export

Everything — hosts, groups, logins, trusted certificates — in one `.uwurdp`
file. Without passwords it's plain JSON, readable and diffable. With passwords
the whole file is sealed under a password of its own (Argon2id,
XChaCha20-Poly1305), not the master password, because the file may go to
another device or another person. Reading it back adds nothing twice, and it
only trusts certificates for hosts the file brings.

## Installer and updates

As in UwUSSH, `apps/setup` is one app for all three systems: the installer with
Nyu, the updater and the uninstaller.

- **Windows:** a per-user install into `%LOCALAPPDATA%\Programs\UwURDP`, no
  admin prompt. Offers WebView2 if it's missing.
- **macOS:** one universal disk image; the setup installs into `/Applications`
  (or `~/Applications`) and clears the quarantine mark on what it installed.
- **Linux:** `.deb` and `.rpm` for x64 and arm64 that update themselves
  through `pkexec`, and a portable folder that doesn't. They need ALSA
  (`libasound2`) for sound. An AUR package is prepared in `packaging/aur` and
  will come once it exists.
- **Updates** are checked 20 seconds after start and every six hours, in a
  Stable or a Beta channel, downloaded quietly and verified against the update
  key in `tauri.conf.json` before anything runs. Feeds live on this
  repository's `updates` branch.
- None of the setups is code-signed with a paid certificate, so SmartScreen and
  Gatekeeper warn once. The update signature is a separate key, and that one
  is checked every time.

`pnpm release` builds and signs; [release-notes/README.md](../release-notes/README.md)
has the steps.

## How it is tested

About 300 Rust tests:

| Crate / area   | Tests | What they cover                                                           |
| -------------- | ----- | ------------------------------------------------------------------------- |
| `uwurdp-store` | 80    | Schema migrations, logins and inheritance, export files, sync plumbing    |
| `uwurdp-sync`  | 51    | Two real devices against an in-memory server, a lying server, pairing     |
| `uwurdp-core`  | 64+7  | Dirty regions, frame encoding, input, flow control; 7 end-to-end sessions |
| `uwurdp-vault` | 35    | Key derivation, wrapping, record encryption                               |
| `uwurdp-proto` | 27    | Payload round trips, unknown fields, clocks, merging                      |
| import         | 11+   | RDCMan versions, credential profiles, inheritance, `.rdp` encodings       |
| desktop, setup | —     | Commands, updater feeds, install paths                                    |

The seven end-to-end tests in `uwurdp-core` run real sessions against an
in-process `ironrdp-server`: certificate pinning, NLA, frames, input, resize,
disconnect.

Then `node apps/desktop/e2e/run.mjs` drives **the real app** over WebView2's
DevTools protocol:

- **Phase A**, against `dev_rdpd`: RDCMan import, the certificate dialog,
  login, the vault, drawing, mouse, keyboard, resize, the overview,
  disconnect and reconnect, an unreachable host, the host form, export.
- **Phase E**: sync between two app instances through a real UwUSync-Server.

`dev_rdpd` is the toy server for all of it:

```bash
cargo run -p uwurdp-core --example dev_rdpd
```

`127.0.0.1:3390`, user `uwu`, password `nyu`, NLA, a fresh self-signed
certificate each start (its fingerprint is printed, to compare with the trust
dialog). It paints a pink gradient, a square follows the mouse, clicks draw
dots and keys shift the hue — so a test can see that input arrived.

The macOS and Linux builds come out of CI, which runs the Rust checks on both,
but nobody has clicked through them by hand yet.
