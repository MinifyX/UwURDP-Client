# Roadmap

My wish list, without dates. The order is deliberate: the biggest risk goes
first, not the nicest feature.

## M0 · Foundation

Done. The fork from UwUSSH, and the question everything else hung on: can a
Tauri app show a remote desktop smoothly, with the pixels crossing the IPC
boundary into a web page?

- **The RDP engine** on IronRDP 0.17: TCP, X.224, TLS with rustls, the
  certificate check before anything secret is sent, CredSSP with NTLM, the
  active session. See [architecture](architecture.md#connecting).
- **The frame path**: the engine keeps the desktop image, merges what changed,
  and sends straight RGBA rectangles over a loopback WebSocket at most every
  16 ms, with at most two unacknowledged. Nothing is ever dropped — a slow page
  gets coarser updates, not missing ones. UwUSSH's terminal spike had already
  shown that flow control is not optional, so it was built in from the first
  line. See [architecture](architecture.md#the-frame-path).
- **Input** by physical key position, so the server's layout applies, plus
  mouse, wheel, and releasing everything when the window loses focus.
- **The store, vault and sync** carried over from UwUSSH, with one migration
  (V7) for what an RDP host needs: display settings, a domain on every login,
  a group login, a gateway and its login, a comment.
- **`dev_rdpd`**, a toy RDP server, and seven end-to-end tests against an
  in-process `ironrdp-server`.

## M1 · Daily driver

The point where I can stop opening RDCMan.

Done, in 0.1.0-beta.1:

- **RDCMan import**: `.rdg` 2.2 to 2.93 with nested groups, credential
  profiles, inherited logins and settings, DPAPI passwords, comments; the
  files RDCMan had open, listed directly. **`.rdp` import**, several at once.
- **Logins like RDCMan**: a host's own, or its group's, set with a right-click.
  No login asks on connect, prefilled, with "save for this host".
- **Certificates like SSH host keys**: SHA-256 fingerprint and the Windows
  thumbprint, asked on first contact, blocked when changed, synced.
- **Tabs and the overview**: each desktop in a tab, several to one host,
  an overview of every open session or of one group, **Connect all** and
  **Disconnect all**.
- **Display**: the desktop takes the tab's size and follows the window, or a fixed size, or full
  screen with an mstsc-like connection bar; scale down or scroll.
- **Clipboard text** both ways, **sound** played locally.
- **Disconnect keeps the tab** with the last picture; a dropped connection
  reconnects once on its own. An unreachable host fails fast, before any
  login dialog.
- **Export and import** of UwURDP's own `.uwurdp` file, sealed with a password
  when it carries passwords.
- **Sync** through a UwUSync server of your own, with a recovery kit, pairing
  by three words and revoking by master password.
- **0.1.0-beta.7: UwULock as the other way to sync.** Settings → Sync offers
  UwUSync as before, or signing in with a UwULock account: the hosts live in
  the account's `rdp` space, changes arrive over UwULock's realtime channel,
  and a device on UwUSync moves over in one click, checked before it switches.
  See [architecture](architecture.md#through-uwulock).
- **0.1.0-beta.8**: the UwULock sync reads with limits, checks every entry it
  moved, trusts the system's certificate authorities, and tells a server with
  app sync switched off apart from a broken one.
- **0.1.0-beta.9: a sharp picture, the whole clipboard, drives.**
  - With H.264, **AVC444** like mstsc: full colour resolution, so text stays
    sharp; the canvas sits on whole device pixels and is never resampled at
    1:1. See [architecture](architecture.md#the-graphics-pipeline).
  - **The whole clipboard.** Text that arrives every time, HTML and pictures
    both ways, and **files through the clipboard**: copied or dropped on the
    desktop, they paste on the server; the server's land on the local
    clipboard. See [architecture](architecture.md#clipboard-and-sound).
  - **Drive redirection**, off by default: local folders, or every fixed
    drive, as `\\tsclient\<name>` under "This PC", per host or handed down
    by the group, synced, imported from RDCMan and `.rdp`. The file system
    behind it is UwURDP's own, confined to the shared folder. See
    [architecture](architecture.md#drive-redirection).
  - **macOS** gets the system's traffic lights instead of Windows buttons.
- **The installer with Nyu** and signed updates on Windows (x64 and ARM),
  macOS and Linux.

Next, roughly in this order:

- **RD Gateway.** The settings are already stored, imported from RDCMan and
  synced; connecting through a gateway says "not supported yet". IronRDP's
  gateway crate (`ironrdp-mstsgu` 0.0.1) doesn't verify the gateway's
  certificate while sending the password in a Basic auth header, and only does
  Basic auth. Sending a password to an unchecked server is not something I'll
  ship, so this waits for either IronRDP or a pinned-certificate path of my
  own.
- **Console / admin session** (`mstsc /admin`). The setting is stored and
  imported, but IronRDP 0.17 always sends an empty cluster data block, so it
  isn't requested yet.
- **Kerberos**, for domains where NTLM is switched off. Needs a KDC client
  without dragging a second TLS stack into the app.
- **Several monitors.**
- **Printers and smart cards**, now that the RDPDR channel is there.
- **RDCMan smart groups**, which the import skips today.
- **Display settings per group**, like RDCMan's inherited display settings,
  instead of per host only.
- **Hyper-V console** (VMConnect-style, over port 2179).
- Testing the macOS and Linux builds by hand, not just in CI.

## Later

- Android and iOS, since Tauri 2 does mobile and IronRDP runs there — a
  desktop on a phone is a niche, but a quick look at a server from the couch
  is a real one.

## Open questions

1. **Gateway: wait or build?** Waiting for IronRDP is cleaner; a small
   gateway client with the certificate pinned like everything else is faster.
2. **Nested groups.** RDCMan's trees are flattened to `Parent / Child` on
   import. Real nesting would make inheritance deeper than one level — is that
   worth the complexity, or is one level the honest amount?
3. **One vault for UwUSSH and UwURDP?** Today each app has its own account on
   the server. Sharing logins between an SSH and an RDP client would be nice
   for Windows servers that speak both, but mixes two apps' records.
