# Security review, September 2026

The first review of UwURDP on its own, dated 2026-09-23, against 0.1.0-beta.5
(`613f280`). It covered the parts that are RDP's: connection setup and
certificate pinning, everything a server sends (graphics pipeline and codecs,
bitmaps, pointer, desktop size, clipboard, audio), the loopback frame socket,
the OpenH264 download, the `.rdg`, `.rdp` and `.uwurdp` importers, the Tauri
command surface and content security policy, the setup and CI.

The sync client, the vault, `device.rs` and the updater are UwUSSH's code after
a rename. They were reviewed there, in UwUSSH's
`docs/security-review-2026-09.md`, and the fixes from that review carry over:
DLLs load from System32 only, an export's certificates are taken only for the
hosts it adds and only when the fingerprint matches, and an export's key
derivation costs are capped.

## The trust boundary

The same as UwUSSH's. The page inside the window is **inside** it: it only ever
renders UwURDP's own code, under a strict content security policy
(`script-src 'self'`, no frames, objects or base URIs), with frozen prototypes
and no `innerHTML`. What a server sends — pixels, pointer shapes, clipboard
text, error strings — reaches the page as canvas data or React text, never as
markup.

A server is outside until the user trusted its certificate, and even then it
may be hostile: it can send anything and must not be able to do more than end
its own session. An imported file is outside too: it names whatever addresses
it likes.

## Fixed

| Severity | Where    | What                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| -------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Medium   | Importer | A `.rdg` could point a server of its own choosing at one of the user's Local-scope RDCMan profiles by name, and the importer stored that profile's password — opened with the user's DPAPI key — as the login of the file's host. A DPAPI blob in a `.rdg` or in an `.rdp`'s `password 51` went the same way. The preview showed counts only, so the user never saw which addresses got which password; connecting and trusting the new certificate then handed the password to that server (CredSSP delegates it). Every password now carries where it came from. One that DPAPI opened or that comes from the user's own profile is written only when the user ticks a box under the list of hosts, gateways and groups that would get it, with their addresses; without the tick those hosts get the username alone. Fixed in `7d01bf0`. |

The review suggested dropping Local-profile passwords for files picked in the
dialog and keeping them for files RDCMan itself lists. That was not taken: the
files RDCMan lists are often team files on a share, just as easy to change, and
a DPAPI blob in the file would have stayed unasked. One rule for every source
is simpler and closes both. Nothing stored changes, and a user importing their
own file ticks one box more.

## Shared with UwUSSH

Three Medium findings of UwUSSH's round apply here unchanged, because the code
is the same. Their fixes are ported with their tests, same logic, UwURDP's
names:

| Severity | Where       | What                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| -------- | ----------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Medium   | Sync engine | The manifest check only ran after a pull that reached the end; a server that always said "there is more" kept it from ever running, and certificates trusted on other devices stayed trusted. An incomplete pull is now written down as something kept back until a complete one clears it, a paired device distrusts other devices' certificates until the pairing's manifest was checked, and the report says whether the pass was complete. (`41da1d2`) |
| Medium   | Updater     | The Linux setup, an AppImage run with `APPIMAGE_EXTRACT_AND_RUN`, unpacked into a folder in `/tmp` whose name anyone can work out from the public release, and ran what it found there. It now unpacks into a fresh 0700 folder in UwURDP's local data folder and hands the `TMPDIR` from before back to the UwURDP it starts again. Protects updates applied by this version on. (`c60de51`)                                                              |
| Medium   | Device seal | A keychain, Secret Service or DPAPI that did not answer counted as "this no longer opens": the pairing's account key and the remembered vault key were deleted for good, and on macOS and Linux a new seal key was made on the way. Only a blob that fails its seal against every key there is (`InvalidData`) is forgotten now; anything else is an error and everything stays. (`9e738f8`)                                                               |

## Not fixed: Low and Info

Listed, not changed in this round.

- **L1, Low — sizes a server sends reach allocations without a cap of the
  app's own.** The desktop size on (re)activation, a `WireToSurface1`
  rectangle before it is clipped to its surface, and an H.264 picture size can
  ask for gigabytes; a failed allocation ends the whole app, not only the
  session. Only a server whose certificate the user trusted can do it, and it
  gains nothing beyond that. Whether IronRDP caps any of it first needs a check
  at runtime.
- **L2, Low — the setup's WebView data folder on Linux has a fixed name in
  `/tmp`.** Another local user can create it first. The setup page stores
  nothing sensitive; the result is a setup that fails or writes WebKit's files
  where that user wants them.
- **L3, Low — clipboard redirection is on by default and works while the
  window is in the background.** A server learns of every text copied anywhere
  during a session and can ask for it, and it can replace the local clipboard.
  mstsc behaves the same.
- **L4, Low — the frame socket takes connections without a handshake timeout
  or limit.** The token check itself is sound; any local process can still hold
  many idle connections open until no new desktop connects. Denial of service
  only.
- **L5, Low — an install into a folder the user picks keeps the inherited
  access list.** Under `C:\` that lets other users of the machine replace
  `UwURDP.exe`. The default folder, in the user's profile, is private.
- **I1, Info — an imported RD Gateway without its own login would get the
  host's.** Harmless while gateways are refused at connect; to change before
  they are implemented.
- **I2, Info — `hex_to_bytes` slices a string by byte index.** A `password 51`
  with a multi-byte character panics inside the import worker, which surfaces
  as an import error.
- **I3, Info — an unchecked addition** when placing a surface on the output
  wraps in release builds; the result is only drawn in the wrong place.
- **I4, Info — the user name goes out before the certificate check**, in the
  connection request cookie. mstsc does the same, and it is documented.

## Checked and fine

- **The pin comes first.** The certificate is checked after the TLS upgrade and
  before CredSSP or the client info; an unknown or changed one drops the
  connection. With NLA on, plain TLS is not offered.
- **TLS still verifies.** The handshake signature is checked against the
  presented certificate, so a replayed public certificate does not pass; the
  CredSSP public key comes from the same certificate; no session resumption.
- **Trust decisions.** Only a fingerprint the server presented in the last
  attempt for that address and port can be trusted, for ten minutes; replacing
  a trusted certificate needs the "changed" dialog's confirmation.
- **Imported settings.** Alternate shells and working folders are never taken;
  drive, printer and device redirection and RemoteApp only become text in the
  comment; NLA is not taken from files.
- **XML.** No DTDs, so no entities; import files are capped at 16 MiB.
- **OpenH264.** Downloaded over HTTPS with a pinned SHA-256 per platform,
  checked before it is moved into place and again when it loads, by full path.
- **Command surface.** Commands take ids, tokens and enums, never paths; picked
  files stay in Rust behind tokens; messages on the frame socket are capped.
- **Graphics pipeline.** Surfaces and the cache have byte budgets, every write
  clips, codec errors are counted rather than fatal.
- **CI.** Every action pinned to a commit, no stored credentials in checkouts,
  read-only permissions, no `pull_request_target`.

## Cleanups in the same pass

- The `.rdg` importer decoded passwords in two copies of the same code, and
  both importers had their own `split_host_port`; one of each now.
- Four `windows-sys` features the app no longer uses (UwUSSH's file sharing,
  access lists and clipboard) are gone.
- Comments inherited from UwUSSH that talked about OpenSSH keys now describe
  certificates; the setup's content security policy says `frame-src 'none'`
  like the app's.
