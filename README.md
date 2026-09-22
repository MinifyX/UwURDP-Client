<p align="center">
  <img src="brand/uwurdp-app-icon.svg" width="112" alt="UwURDP logo" />
</p>

<h1 align="center">UwURDP</h1>

<p align="center">
  The remote desktop client I build for myself, because every other one annoyed me. (◕‿◕✿)<br/>
  RDP · RDCMan import · Vault · Sync · Windows, macOS and Linux, first beta
</p>

<p align="center">
  <a href="https://github.com/MinifyX/UwURDP-Client/releases"><b>Download for Windows, macOS and Linux</b></a>
  ·
  <a href="docs/install.md"><b>How to install</b></a>
  ·
  <a href="docs/install.md#uwurdp-installieren">Anleitung auf Deutsch</a>
</p>

---

## Why this exists

Every remote desktop client I tried annoyed me in one way or another.
Remote Desktop Connection Manager does exactly what I want — a tree of
servers, a login per group, thumbnails of everything that's open — but it looks
like 2008, lives in one `.rdg` file on one machine, and Microsoft only barely
keeps it alive. mstsc is fine for one server and useless for sixty. The
modern ones want an account, a subscription, or both, and sync your server
list, passwords included, through somebody else's cloud. So I started building
my own, the way I want it, with a sync server that runs on my own box.

- **Just for fun.** No company, no team, no schedule, no promises. I work on it
  when I have time and feel like it, so don't expect steady development, and
  don't be surprised by long breaks.
- **Written with AI.** Almost all of the code is written with Claude, because
  I'm honestly not a great programmer. Not your thing? No hard feelings, just
  pick something else.
- **Use it, fork it, do what you want with it.** The license only asks one
  thing: if you pass on a changed version, its source stays open too.
- **No support.** Issues and pull requests are okay, but I might answer late or
  not at all, and I mostly build what I need myself.

UwURDP is a fork of my SSH client [UwUSSH](https://github.com/MinifyX/UwUSSH-Client):
same vault, same sync, same installer, same cat — with an RDP engine where the
terminal used to be. It syncs through the same
[UwUSSH server](https://github.com/MinifyX/UwUSSH-Server).

## What it is

UwURDP is an open-source RDP client for people who look after more Windows
machines than they can remember, from more than one computer. Think RDCMan's
density with a calmer face and a sync server that is yours.

- **Groups and logins like RDCMan.** Each host has its own login, or takes its
  group's — "inherit from parent", one level deep. Change a password once, and
  forty servers use the new one.
- **Everything open at a glance.** Each desktop gets a tab. The overview shows
  all open sessions as live thumbnails, or one whole group with a connect
  button on the servers that aren't open yet. **Connect all** does what it
  says.
- **It imports your old setup.** RDCMan `.rdg` files — groups, logins,
  inherited settings, and the passwords too, if you're on the Windows account
  that saved them — and mstsc `.rdp` files. Nobody retypes 80 servers.
- **Your hosts, your passwords, your server.** Passwords live in an encrypted
  vault. Sync is end-to-end encrypted: the server relays ciphertext it cannot
  read.
- **Certificates checked like SSH host keys.** First contact shows the
  fingerprint and asks; a changed certificate blocks the connection with a
  plain warning. Nothing is sent to a server before its certificate checks
  out.
- **Private by default.** No telemetry, no account with me, no subscription.
  Passwords never reach the web page the interface runs in.
- **Playful.** Nyu, the cat, now lives in a monitor. Security warnings are
  never playful.

> **Status: first beta.** [0.1.0-beta.1](https://github.com/MinifyX/UwURDP-Client/releases)
> is out for Windows, macOS and Linux, with the installer with Nyu in it and
> signed automatic updates. I use it every day on Windows; the macOS and Linux
> builds come out of CI and haven't been tried by hand yet. It is a beta:
> expect rough edges.
>
> **What works.**
>
> - RDP with TLS and NLA (CredSSP with NTLM), on [IronRDP](https://github.com/Devolutions/IronRDP).
>   The desktop follows the tab's size, or stays at a fixed size, or goes full
>   screen with an mstsc-like connection bar; a desktop that doesn't fit
>   scales down or scrolls.
> - The keyboard works by key position, so the server's keyboard layout
>   applies, just like in mstsc. Ctrl+Alt+End sends Ctrl+Alt+Del.
> - Clipboard text both ways, and sound from the server played here.
> - Tabs, several to the same host too; a dropped connection keeps its tab with
>   the last picture and reconnects once on its own.
> - Two workspaces, **Private and Business**, groups sorted by drag and drop,
>   and a group login set with a right-click.
> - Import from RDCMan (`.rdg` 2.2 to 2.93, including credential profiles and
>   the files RDCMan had open) and from `.rdp` files, and an export of
>   everything into one `.uwurdp` file, sealed with a password when it carries
>   passwords.
> - **Sync** through a [UwUSSH server](https://github.com/MinifyX/UwUSSH-Server)
>   of your own: hosts, logins, passwords and trusted certificates end-to-end
>   encrypted, a recovery kit shown once, a new device paired by three words.
>
> **What doesn't, yet.** RD Gateway (the settings are kept and imported, but
> connecting through one says "not supported yet"), the console/admin session,
> Kerberos, drive, printer and smart card redirection, several monitors,
> copying files through the clipboard. The [roadmap](docs/roadmap.md) has the
> order.

## Install

Windows 10 or 11 (x64 and ARM), macOS 11 or newer (Apple silicon and Intel),
Linux (x86_64 and arm64).

1. Open the [releases](https://github.com/MinifyX/UwURDP-Client/releases) and
   download the file for your system from the newest one:
   `UwURDP-windows-x64-setup.exe` (`UwURDP-windows-arm64-setup.exe` on ARM),
   `UwURDP-macos-universal.dmg`, or on Linux `UwURDP-linux-x64.deb` /
   `.rpm` (`…-arm64…` on ARM), or the `…-portable.tar.gz` to just unpack and
   run. An Arch package (`uwurdp-bin`) is planned.
2. Run it. Neither Windows nor macOS knows the setup, because it isn't signed
   with a paid certificate: on Windows **More info → Run anyway**, on macOS
   **System Settings → Privacy & Security → Open Anyway**.
3. Click **Install**. No admin prompt: it installs for your user only and keeps
   itself up to date. The Linux packages update themselves too, asking for
   the administrator password; the portable folder doesn't.

The [install guide](docs/install.md) has the details: checking the download,
updates, uninstalling, where your data lives, and what to do when something
goes wrong. [Auf Deutsch](docs/install.md#uwurdp-installieren).

## The sync server

UwURDP doesn't have a server of its own. It speaks the wire protocol of
[UwUSSH-Server](https://github.com/MinifyX/UwUSSH-Server) unchanged: one Rust
binary, one Docker image, one SQLite file, its own TLS certificate whose
fingerprint each device pins. Already running one for UwUSSH? Give UwURDP an
account of its own on it:

```bash
docker compose exec uwussh uwussh-server invite
```

That prints a new `uwu1_…` setup code. Paste it into **Settings → Sync →
Connect a server**, and UwURDP shows the recovery kit once. Another device
joins with the code **Add a device** shows — three words over SPAKE2, the
master password typed on each device and never sent.

The server only ever holds ciphertext, and without the account key from the
recovery kit even its copy of the wrapped vault key is worth nothing. And if
you'd rather not run a server at all, "local only" is a first-class choice,
not a downgrade.

## Project layout

| Path                   | What lives there                                                          |
| ---------------------- | ------------------------------------------------------------------------- |
| `apps/desktop`         | The Tauri 2 app (React UI + Rust shell)                                   |
| `apps/desktop/e2e`     | End-to-end run of the real app against a toy RDP server and a sync server |
| `apps/setup`           | The installer, updater and uninstaller, for all three systems             |
| `crates/uwurdp-core`   | Session engine: IronRDP, certificates, frames, input, clipboard, sound    |
| `crates/uwurdp-import` | RDCMan `.rdg` and `RDCMan.settings`, mstsc `.rdp`, DPAPI                  |
| `crates/uwurdp-store`  | SQLite: hosts, groups, logins, trusted certificates, export files         |
| `crates/uwurdp-vault`  | Key derivation, record encryption                                         |
| `crates/uwurdp-sync`   | Sync client: clocks, outbox, merging, pairing                             |
| `crates/uwurdp-proto`  | Shared types between client and server                                    |
| `brand/`               | Nyu: the UwURDP icon, symbol, mono symbol                                 |
| `docs/`                | Vision, architecture, design, roadmap, install guide                      |
| `release-notes/`       | What's new, per version                                                   |
| `scripts/`             | Building the setup, releasing                                             |
| `packaging/aur`        | The Arch package, for when it exists                                      |

## Development

Requirements:

- Node.js 24 and pnpm 11 (`corepack enable`)
- Rust stable (via [rustup](https://rustup.rs))
- Platform prerequisites for Tauri: see
  [tauri.app/start/prerequisites](https://tauri.app/start/prerequisites/)
  (Windows: Visual Studio C++ Build Tools and WebView2; Linux:
  `libwebkit2gtk-4.1-dev` and friends, plus `libdbus-1-dev` and
  `libasound2-dev` for sound)

```bash
pnpm install
pnpm tauri dev
```

No Windows server at hand? A toy RDP server for trying things out —
`127.0.0.1:3390`, user `uwu`, password `nyu`, NLA, a fresh self-signed
certificate on every start. It paints a pink gradient, a square follows the
mouse, clicks leave dots and keys shift the colours:

```bash
cargo run -p uwurdp-core --example dev_rdpd
```

Checks:

```bash
pnpm typecheck && pnpm lint
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
node apps/desktop/e2e/run.mjs     # end to end, Windows (phase E needs ../UwUSSH-Server built)
```

The installer, with the app packed inside:

```bash
pnpm build:setup                  # target/installers/, for the system it runs on
```

Releasing is `pnpm release`: it builds and signs the Windows setup here, takes
the macOS and Linux setups CI built for the tag, signs those here too, and
publishes all of them. [release-notes/README.md](release-notes/README.md) has
the steps.

## Documentation

- [Install guide](docs/install.md) — installing, updating, uninstalling, in English and German
- [Konzept](KONZEPT.md) — the concept, in German
- [Vision](docs/vision.md) — what I want UwURDP to be and what it will never do
- [Architecture](docs/architecture.md) — how the pieces fit together
- [Design](docs/design.md) — colors, type, Nyu, tone of voice
- [Roadmap](docs/roadmap.md) — my wish list, without dates

## License

UwURDP is free software under the [GNU GPL v3.0](LICENSE): use it, change it,
fork it, share it. If you pass on a changed version, its source has to stay
open too.
