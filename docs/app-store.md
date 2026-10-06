# Mac App Store (and a look at the Microsoft Store)

UwURDP ships on the Mac in two forms. The **download from GitHub** (the setup
app, the disk image) is what it has always been: not sandboxed, updates itself,
fetches Cisco's OpenH264 when H.264 is switched on. The **Mac App Store
build** is the same client with the store's rules applied. This document is
how that second one is built and how to publish it. None of it has been
through App Review yet, and none of it has run on a real Mac yet (see "Before
the first review").

The last section is an analysis of the Microsoft Store. Nothing is built for
it.

## What is different in the store build

|                          | GitHub (setup, DMG, packages)                                          | Mac App Store                                                                                 |
| ------------------------ | ---------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| Cargo features           | default (`self-update`, `h264-download`)                               | `--no-default-features --features mas`                                                        |
| Config                   | `tauri.conf.json` (+ `tauri.macos.conf.json`)                          | + `tauri.mas.conf.json`                                                                       |
| Installed by             | the setup app (`apps/setup`)                                           | the store; no setup app is built                                                              |
| Updates                  | updater plugin, GitHub feed                                            | none compiled in; the store updates it                                                        |
| H.264 (OpenH264)         | downloaded from Cisco on request                                       | not available: App Review 2.5.2 forbids downloading code                                      |
| Sandbox                  | no                                                                     | yes, `macos/Entitlements.mas.plist`                                                           |
| Shared folders (drives)  | any path                                                               | only folders picked in the panel, kept through security-scoped bookmarks                      |
| "Alle Laufwerke"         | every fixed drive (offered everywhere as before; only Windows has any) | refused (`hosts.rs` leaves it out)                                                            |
| RDCMan import            | RDCMan's list of open files (Windows only)                             | only files picked in the open panel                                                           |
| One copy at a time       | single-instance plugin                                                 | macOS itself (the plugin's socket in `/tmp` is closed to the sandbox, so it's off)            |
| This Mac's name for sync | `scutil --get ComputerName`                                            | `NSHost.localizedName` (runs no other program)                                                |
| App data                 | `~/Library/Application Support/app.uwurdp.desktop`                     | `~/Library/Containers/app.uwurdp.desktop/Data/Library/Application Support/app.uwurdp.desktop` |

Everything else is the same code: connecting, the vault, sync with UwUSync and
UwULock, `uwurdp://` links, the clipboard with files in both directions, audio.

### What the page asks: `build_info`

The page hides what a build doesn't have. It asks once at start
(`apps/desktop/src/lib/build.ts`: `loadBuildInfo()`, then `buildInfo()`):

```ts
type BuildInfo = {
  store: boolean; // the Mac App Store build
  updates: boolean; // self-update: channel, "check now", "restart to update", setup texts
  h264: boolean; // the H.264 switch can fetch OpenH264
  allDrives: boolean; // "Alle Laufwerke" means something (Windows, not the store)
  rdcmanScan: boolean; // RDCMan's open files can be listed (Windows only)
};
```

The update commands still exist in a build without updates and answer
quietly (`update_status` → `null`, `check_for_updates` / `install_update` →
an error, `set_update_channel` ignored), so a page that asks anyway doesn't
break. `h264_status` reports `unsupported` there.

## Entitlements, and why

| Entitlement                      | Why                                                                                                                                                                             |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `app-sandbox`                    | Required for the store.                                                                                                                                                         |
| `network.client`                 | RDP and RD Gateway connections to the user's hosts; sync with the user's UwUSync or UwULock server.                                                                             |
| `network.server`                 | The loopback WebSocket on `127.0.0.1` that carries frames and input between the app and its own window (`frames.rs`). It takes only the page's connection, with a random token. |
| `files.user-selected.read-write` | Folders shared with a remote desktop (the server may write into them), `.rdp`/`.rdg` imports, `.uwurdp` export and import, files dropped on a desktop.                          |
| `files.bookmarks.app-scope`      | Security-scoped bookmarks, so a shared folder still works after a restart.                                                                                                      |

Deliberately absent:

- **Microphone** (`device.audio-input`, `NSMicrophoneUsageDescription`): remote
  audio is only played (RDPSND over cpal); nothing is recorded or redirected.
- **Downloads folder**: files copied on the remote desktop are fetched into the
  app's temporary folder (inside the container) and put on the Mac's
  clipboard from there.
- **Keychain access groups**: the device key that keeps the vault open lives in
  the login keychain (`keyring`'s macOS backend, the file-based keychain),
  where a sandboxed app reaches the items it created without an entitlement.
- Everything that runs or controls other programs.

The two entitlements that name the team (`com.apple.application-identifier`,
`com.apple.developer.team-identifier`) are added by `scripts/build-mas.mjs` at
signing time, so nothing team-specific is committed.

`Info.plist` (merged into every macOS build, GitHub's too) adds
`ITSAppUsesNonExemptEncryption` (below) and `NSLocalNetworkUsageDescription`:
macOS 15 asks before an app reaches the local network, where most RDP hosts
are. Tauri writes `CFBundleURLTypes` for `uwurdp://` from the deep-link
plugin's config, and `LSApplicationCategoryType` from `bundle.category`
(Utility → `public.app-category.utilities`).

`macos/PrivacyInfo.xcprivacy` declares no tracking and no collected data, and
the required-reason APIs: file timestamps (3B52.1 shared folders the user
picked, C617.1 the app's own temporary folders), disk space (85F4.1: a shared
folder's free space shown on the remote desktop), system boot time (35F9.1,
Rust's `Instant`), user defaults (CA92.1, AppKit and WebKit).

## Shared folders in the sandbox

- **Picking** a folder ("Ordner freigeben", `pick_shared_folder`) stores a
  security-scoped bookmark for it in `bookmarks.json` next to the host list
  (`sandbox_access.rs`, at most 256, oldest dropped). A folder inside one that
  has a bookmark uses that one.
- **Connecting** resolves the bookmark of every folder the host shares and
  switches its access on for that session; closing the desktop (or the page
  reloading) switches it off. A folder moved in Finder is shared from where it
  is now.
- A folder **without a bookmark** — synced from another device, from an
  imported `.rdp` file, or picked on another Mac — is skipped when connecting,
  with a log line, like a folder that isn't on this computer. Picking it again
  on this Mac fixes it.
- **"Alle Laufwerke"** has no folder to bookmark and is left out in the store
  build; the page hides it (`allDrives`).
- **Dropped files** (onto a desktop, for the server's clipboard) are granted to
  the app by the system for the rest of its run, which outlasts the clipboard
  offer, so they need no bookmark. The engine reads them only when the server
  asks for their contents. To check on a real Mac (below).
- **Files copied in Finder** and pasted on the remote desktop go through the
  general pasteboard, which hands a sandboxed app access to the file URLs it
  reads from it. Also to check on a real Mac.

## Building

On a Mac with Xcode and both Rust targets
(`rustup target add aarch64-apple-darwin x86_64-apple-darwin`):

```sh
pnpm build:mas
```

That builds the desktop app alone (no setup app) as a universal app (Apple
silicon and Intel), checks it — both architectures, bundle ID, category,
encryption flag, privacy manifest, the `uwurdp://` scheme, no update feed and
no OpenH264 address left in the executable — and packs it with
`productbuild` into `target/release/UwURDP-<version>-mas-universal.pkg`.
Without signing identities in the environment the app and the package stay
unsigned; that is what CI does on every pull request touching the store
build.

The version: App Store Connect takes only three numbers, so `0.1.0-beta.12` is
uploaded as `0.1.0`. `MAS_BUILD_NUMBER` becomes `CFBundleVersion` and has to
grow with every upload of the same version (CI uses the run number). Once
UwURDP leaves beta, the store version and the GitHub version are the same.

Signing reads these from the environment (the identities must be in a
keychain `codesign` can use):

| Variable                         | What                                                                                  |
| -------------------------------- | ------------------------------------------------------------------------------------- |
| `APPLE_MAS_APP_IDENTITY`         | `3rd Party Mac Developer Application: <Name> (<TEAMID>)` or `Apple Distribution: …`   |
| `APPLE_MAS_INSTALLER_IDENTITY`   | `3rd Party Mac Developer Installer: <Name> (<TEAMID>)`                                |
| `APPLE_TEAM_ID`                  | the ten-character team ID                                                             |
| `APPLE_MAS_PROVISIONING_PROFILE` | path to the Mac App Store provisioning profile → `Contents/embedded.provisionprofile` |
| `MAS_BUILD_NUMBER`               | build number, required for a signed build                                             |

`node scripts/build-mas.mjs --sign <UwURDP.app>` signs and packs an app that
was built elsewhere (CI's `sign` job does that).

### CI

`.github/workflows/mas.yml` runs on tags (beside `installers.yml`, not inside
it) and by hand. `build` makes the unsigned universal app and package (artifact
`mas-unsigned`). Pull requests that touch the store build get `check` instead:
`node scripts/build-mas.mjs --check`, a debug build for Apple silicon with the
same bundle checks, which `installers.yml`'s macOS check also runs on every push
to main, so its compiled dependencies are cached for the pull requests. `sign`
runs only when these repository secrets exist, on a fresh runner that has built
nothing:

| Secret                            | Content                                                                              |
| --------------------------------- | ------------------------------------------------------------------------------------ |
| `APPLE_MAS_CERTIFICATES_P12`      | base64 of one .p12 with both certificates (application and installer) and their keys |
| `APPLE_MAS_CERTIFICATES_PASSWORD` | its password                                                                         |
| `APPLE_MAS_PROVISIONING_PROFILE`  | base64 of the `.provisionprofile`                                                    |
| `APPLE_MAS_APP_IDENTITY`          | as above                                                                             |
| `APPLE_MAS_INSTALLER_IDENTITY`    | as above                                                                             |
| `APPLE_TEAM_ID`                   | as above                                                                             |

The signed package is the run's `mas-signed` artifact. Uploading stays a
manual step, on purpose.

`ci.yml` also runs clippy on Linux with the store's features
(`cargo clippy -p uwurdp-desktop --no-default-features --features mas`), which
covers everything except the macOS-only code; that builds in `mas.yml`. A
macOS-target clippy from Linux is not possible here: several dependencies
compile C or Objective-C against Apple's SDK in their build scripts.

## Publishing, step by step

1. **Apple Developer Program** membership (99 USD/year), as an individual or as
   MinifyX (an organisation needs a D-U-N-S number). The seller name in the
   store comes from this. The same membership serves UwUNotes and the other
   suite apps.
2. **App ID**: Certificates, Identifiers & Profiles → Identifiers → `+` → App
   IDs → App, platform macOS, explicit bundle ID `app.uwurdp.desktop`. No
   capabilities need ticking.
3. **Certificates** (once per team, shared with UwUNotes): _Mac App
   Distribution_ and _Mac Installer Distribution_, exported with their keys into
   one .p12 for CI.
4. **Provisioning profile**: Profiles → `+` → Distribution → _Mac App Store
   Connect_, App ID `app.uwurdp.desktop`.
5. **App Store Connect record**: Apps → `+` → New App: macOS, name "UwURDP",
   bundle ID `app.uwurdp.desktop`, SKU e.g. `uwurdp-mac`.
6. **App information**: category _Utilities_ (secondary: _Business_ or
   _Developer Tools_), age rating questionnaire (all "None" → 4+), price free.
7. **App Privacy**: privacy policy URL, "Do you collect data?" → **No** (see
   the privacy manifest above; the servers belong to the user, sync is end-to-end
   encrypted).
8. **Export compliance**: see below. Answered once in App Store Connect.
9. **Screenshots** (16:10, e.g. 2880×1800): the host list, a connected desktop
   (a test VM, nothing private), the vault, settings → sync. Taken from the
   store build: no update setting, no H.264 switch.
10. **Build and upload**: run the `Mac App Store` workflow (or `pnpm build:mas`
    with the variables above), then upload the signed `.pkg` with
    **Transporter** or `xcrun altool --upload-app --type macos --file … --apiKey
… --apiIssuer …`.
11. **TestFlight for Mac**: go through "Before the first review" on a real Mac.
12. **Submit for review** with the notes below, and a demo host if Apple asks
    for one.

### Export compliance

`ITSAppUsesNonExemptEncryption` is **true**, and that is the honest answer:
UwURDP brings its own encryption rather than only using Apple's — TLS 1.2/1.3
(rustls) and CredSSP/NTLM for RDP and the gateway, XChaCha20-Poly1305 with an
Argon2id key for the vault, end-to-end encryption for sync. UwUNotes could say
`false` because it encrypts nothing; UwURDP cannot.

All of it is standard, published algorithms, in an app anyone can get for free,
whose source is public under GPL-3.0. That makes it a mass-market item using
standard cryptography (ECCN 5D992.c under License Exception ENC §740.17(b)(1)),
and publicly available encryption source code (§742.15(b)): no CCATS and no
classification request are needed. In App Store Connect (App Information → App
Encryption Documentation, or the per-build question):

- Does the app use encryption? **Yes.**
- Proprietary or non-standard algorithms? **No.**
- Standard algorithms instead of, or in addition to, Apple's? **Yes.**
- Available in France? France wants an import declaration for such apps.
  Either upload it (ANSSI form) or leave France out of the first release.

Apple then issues an `ITSEncryptionExportComplianceCode` once the
documentation is approved; adding it to `Info.plist` stops the per-build
question. Until then each build asks once in App Store Connect. This is the
part of the submission most worth a second look by someone who knows export
law; nothing here is legal advice.

### Review notes (paste into "Notes" for App Review)

> UwURDP is a Remote Desktop (RDP) client. It connects only to servers the
> user adds — their own Windows computers and servers, optionally through an
> RD Gateway. It has no account of its own and no in-app purchases; it
> collects no data. Saved passwords are kept in an encrypted vault on the Mac
> (master password, Argon2id, XChaCha20-Poly1305). Optionally, hosts and the
> vault sync between the user's devices through a server the user runs
> (UwUSync) or their UwULock password manager; that sync is end-to-end
> encrypted. To try it you need an RDP server; we can provide a test host on
> request.
>
> The Mac App Store version differs from our GitHub version: it has no
> self-updater, does not download the optional OpenH264 video codec (H.264
> stays off), and shares only folders the user picks in the open panel with a
> remote desktop (kept across restarts through security-scoped bookmarks).
> `network.server` is used only for a loopback WebSocket on 127.0.0.1 between
> the app and its own window, which carries the remote screen.

### App Review guidelines, point by point

| Guideline                                     | How UwURDP meets it                                                                                |
| --------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| 2.4.5(i) sandboxed, correct entitlements      | Sandbox with network client/server (loopback only), user-selected files and app-scope bookmarks.   |
| 2.4.5(ii) packaged with Xcode tools           | `productbuild` package signed with the installer certificate.                                      |
| 2.4.5(iii) self-contained, no installing code | One executable; the setup app is not part of this build.                                           |
| 2.4.5(iv) no login items without consent      | None.                                                                                              |
| 2.4.5(v) no other programs or scripts         | No `scutil`, no package manager, no setup; links open through `NSWorkspace`.                       |
| 2.4.5(vi) updates only through the store      | Updater plugin, feed and update UI not compiled in.                                                |
| 2.5.2 no downloaded code                      | OpenH264 download not compiled in; `build-mas.mjs` checks Cisco's address is not in the binary.    |
| 4.2 minimum functionality                     | A full RDP client: gateway, clipboard with files, shared folders, audio, vault, sync.              |
| 5.1.1 privacy policy, 5.1.2 data use          | Privacy policy URL; nothing collected (`PrivacyInfo.xcprivacy`).                                   |
| 5.2 intellectual property                     | Own name and artwork; "Remote Desktop" and "RDP" used descriptively only (Microsoft's trademarks). |
| 5.2.1 / GPL                                   | MinifyX owns the copyright and may distribute through the store; the source stays public.          |

Risks to expect in review: a reviewer without an RDP server (offer a test
host), questions about `network.server` (answered in the notes), and the
export compliance answers.

## Before the first review (needs a Mac)

Nothing below could be tried while this was written. On a real Mac, with a
TestFlight or a locally signed build:

- the window comes up, the native title bar and menu bar are there, ⌘Q with an
  open desktop asks first;
- connecting to a host on the local network: macOS 15 shows the local network
  prompt with our text; RD Gateway works;
- the frame WebSocket on 127.0.0.1 works under the sandbox (`network.server`);
- share a folder, connect, read and write it from the server; quit, start
  again, connect: still there (`bookmarks.json` in the container has an
  entry); move the folder in Finder: still shared;
- drop a file on a desktop and paste it on the server — also a minute after
  the drop; copy a file in Finder and paste it on the server; copy a file on
  the server and paste it in Finder;
- the vault stays open across a restart (device key in the login keychain from
  a sandboxed app);
- `uwurdp://connect/<id>` from Safari, cold and while running;
- sync with UwUSync and sign-in to UwULock;
- no update or H.264 setting anywhere, no setup texts.

## What stays open

- Everything in the list above.
- German text for the permission prompt (`NSLocalNetworkUsageDescription` is
  English; a `de.lproj/InfoPlist.strings` would translate it).
- France and the export compliance code (above).
- Should a Mac App Store copy and a GitHub copy run on the same Mac, they keep
  separate host lists (different data folders) and the GitHub copy's
  single-instance socket doesn't see the store copy.
- Liquid Glass icon (macOS 26), as for every suite app.

## Microsoft Store (analysis only)

Nothing is built for the Microsoft Store. This is what it would take.

### Two ways in

1. **Win32 app with the existing installer** (an `.exe`/`.msi` listed in the
   Store, since 2021): the Store links to a versioned, signed installer URL on
   our side and runs it silently. Our setup app would have to support silent
   install (`/S`-style switch, no UI), the installer must be code-signed (an
   OV/EV certificate, which UwURDP doesn't have yet), and the URL must serve a
   fixed file per version. The app keeps updating itself — the Store allows
   that for Win32 listings — so nothing in the app changes. The least work,
   but it needs the signing certificate and a silent mode in `apps/setup`.
2. **MSIX package**: the Store signs it (no own certificate needed for Store
   distribution), installs and updates it. This is the "real" Store app, and
   the one with the most consequences:

| Topic                 | What MSIX means for UwURDP                                                                                                                                                                                                                                           |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Updater               | Off, as on the Mac: the Store updates MSIX packages. The `self-update` feature already does this; a `msstore` feature would be `--no-default-features` plus whatever MSIX needs. The setup app is not shipped.                                                       |
| Package identity      | A Partner Center reservation gives the identity name (e.g. `MinifyX.UwURDP`) and publisher (`CN=<GUID>`), which go into `AppxManifest.xml`. Version must be four numbers (`0.1.12.0`); betas need a mapping like the Mac's.                                          |
| Deep link `uwurdp://` | Declared in the manifest (`windows.protocol` extension) instead of the registry keys the setup writes (`Software\Classes\uwurdp`). The deep-link plugin's runtime registration must stay off (it would write into the virtualized registry).                         |
| Single instance       | The plugin uses a named mutex/window message on Windows, which works inside MSIX; a link opened while running reaches the running copy. To be confirmed.                                                                                                             |
| App data              | `%APPDATA%\app.uwurdp.desktop` is redirected into the package's private folder (file system virtualization). The app works unchanged, but the data is removed with the app and a GitHub copy on the same PC sees different data. DPAPI keeps working (same user).    |
| OpenH264              | Downloading Cisco's DLL is allowed for desktop apps in the Store (policy 10.2.2 is about undisclosed code; Cisco's binary is disclosed and opt-in), but loading a DLL from a writable folder inside MSIX should be tested. Could stay as is, or be off like the Mac. |
| Drives, RDCMan scan   | Full trust desktop app (`runFullTrust`): file system access is unchanged, "all drives" and the RDCMan scan keep working.                                                                                                                                             |
| WebView2              | Present on every Windows 10/11 the Store supports; no bootstrapper needed.                                                                                                                                                                                           |
| ARM64                 | One package per architecture, or a bundle (`.msixbundle`) with x64 and ARM64.                                                                                                                                                                                        |

### What CI would need

- A Windows job building the app with the Store's feature set, then
  `makeappx pack` with a generated `AppxManifest.xml` (identity, protocol,
  visual assets from the `uwu-icons` set: 44, 150, 310 px tiles), for x64 and
  ARM64, and `makeappx bundle`.
- For local testing a self-signed certificate (`signtool`); for the Store no
  signing: Partner Center signs on ingestion.
- Upload through Partner Center or the Microsoft Store submission API
  (`msstore` CLI) with an Entra app registration: client ID, tenant, secret as
  repository secrets.
- Run the Windows App Certification Kit (`appcert.exe`) in the job before
  uploading.

### Cost and account

A Partner Center developer account: free for individuals since 2024 (identity
verification still required), a one-time fee for a company account. The Store
takes no cut from a free app. For the Win32-installer route add a code-signing
certificate (roughly 200–500 USD a year), which the GitHub downloads would
benefit from as well (no SmartScreen warning).

### Recommendation

If the Store matters for Windows, start with the MSIX route behind a feature
like `mas` (`msstore`: no updater, no setup, protocol from the manifest) and
keep the GitHub setup as it is. The Win32 listing only pays off once there is a
code-signing certificate anyway.
