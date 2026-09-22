<p align="center">
  <img src="brand/uwurdp-app-icon.svg" width="112" alt="UwURDP logo" />
</p>

<h1 align="center">UwURDP</h1>

<p align="center">
  The SSH client I build for myself, because every other one annoyed me. (◕‿◕✿)<br/>
  SSH · SFTP · Vault · Sync · Windows, macOS and Linux, beta
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

Every SSH client I tried annoyed me in one way or another. The old ones are
powerful but look like 2004 and forget everything the moment you switch
machines. The pretty ones sync your whole host list, keys included, through
somebody else's cloud, usually behind a subscription. So I started building my
own, the way I want it, with a sync server that runs on my own box.

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

This is the sibling of [UwUMail](https://github.com/MinifyX/UwUMail-Client), and
it shares its design system, its tooling and its cat.

## What it is

UwURDP is an open-source SSH client for people who have more hosts than they
can remember and more than one machine to reach them from. This is where it is
headed; the status below says what already works today.

- **Your hosts, your keys, your server.** The sync server is
  [self-hosted](#the-sync-server) and gets only ciphertext. Host names, keys and
  passwords are encrypted on your machine before they ever leave it, so the
  server can relay them without being able to read them.
- **Keys and logins live in the vault.** Generate or import a key, add the
  passphrase once, assign it to as many hosts as you like. Unlock the vault and
  everything is there, on every device, without copying key files around.
- **It imports your old setup.** PuTTY and KiTTY sessions straight from the
  registry, `~/.ssh/config`, Termius, WinSCP, mRemoteNG and MobaXterm exports.
  Nobody retypes 80 hosts.
- **Calm, dense UI.** A quiet host tree, tabs and splits, a command palette on
  `Ctrl+K`, and an inspector you can fold away when you just want a terminal.
- **Everything a session needs.** Agent auth, ProxyJump chains, local, remote
  and dynamic port forwarding, an SFTP browser, snippets and broadcast input.
- **Private by default.** No telemetry, no account with me, no subscription.
  The vault auto-locks, and key material never leaves the Rust core.
- **Playful.** Nyu, the terminal cat, keeps you company. Prefer it plain?
  Settings → Tone → Neutral. Security warnings are never playful, in either
  tone.

> **Status: beta.** [Betas are out for Windows, macOS and Linux](https://github.com/MinifyX/UwURDP-Client/releases),
> each with the same installer with Nyu in it, and signed automatic updates.
> Splits, port forwarding, agent login and ProxyJump are still to come. It is a
> beta: expect rough edges.
>
> **What works.** SSH with a password or a key — OpenSSH, PEM and PuTTY `.ppk` —
> with host keys checked on first contact and every time after, in tabs, on a
> terminal path measured at 41–46 MiB/s ([the spike](docs/m0-spike.md)).
>
> - Hosts live in two workspaces, **Private and Business**, in groups you sort by
>   drag and drop, each with a little icon for the system the server runs
>   (Ubuntu, Debian, Fedora, Windows, Cisco and friends, detected on connect).
> - Passwords and keys can live in an **encrypted vault**. Unlock it once, or let
>   your user account open it on its own. When `sudo` asks for the password
>   in the terminal, one click types it.
> - A **file browser**: your computer on the left, the server on the right, over
>   SFTP or an SMB share, with drag and drop both ways and a root mode through
>   `sudo` that starts at `/`.
> - **UwUKeygen**, a PuTTYgen with Nyu: RSA, Ed25519, ECDSA, OpenSSH, PuTTY and PEM
>   output, randomness from chasing a laser pointer. Built into the host form, and
>   as its own small app the installer can add.
> - Keyword highlighting in the terminal, Ctrl+mouse wheel for the text size,
>   and an export of everything into one file, sealed with a password when it
>   carries secrets, that UwURDP reads back in.
> - Imports from Termius (its local database, since Termius has no export —
>   [how](docs/architecture.md#termius-which-has-no-export)), PuTTY and KiTTY
>   from the registry or from a portable KiTTY's folder and `.reg` exports, and
>   `~/.ssh/config` with its `Include`s.
> - **Sync** through a [UwUSSH server](https://github.com/MinifyX/UwUSSH-Server)
>   of your own: hosts, keys and passwords end-to-end encrypted, a recovery kit
>   shown once, a new device paired by three words, and revoking one with the
>   master password.
>
> Splits, agent login and ProxyJump come next; the [roadmap](docs/roadmap.md) has
> the order.

## Install

Windows 10 or 11 (x64 and ARM), macOS 11 or newer (Apple silicon and Intel),
Linux (x86_64 and arm64).

1. Open the [releases](https://github.com/MinifyX/UwURDP-Client/releases) and
   download the file for your system from the newest one:
   `UwURDP-windows-x64-setup.exe` (`UwURDP-windows-arm64-setup.exe` on ARM),
   `UwURDP-macos-universal.dmg`, or on Linux `UwURDP-linux-x64.deb` /
   `.rpm` (`…-arm64…` on ARM), `yay -S uwurdp-bin` on Arch, or the
   `…-portable.tar.gz` to just unpack and run.
2. Run it. Neither Windows nor macOS knows the setup, because it isn't signed
   with a paid certificate: on Windows **More info → Run anyway**, on macOS
   **System Settings → Privacy & Security → Open Anyway**.
3. Click **Install**. No admin prompt: it installs for your user only, brings
   UwUKeygen along unless you untick it under Options, and keeps itself up to
   date. The Linux packages update themselves too, asking for the
   administrator password; the Arch package updates with pacman, the portable
   folder not at all.

The [install guide](docs/install.md) has the details: checking the download,
updates, uninstalling, where your data lives, and what to do when something
goes wrong. [Auf Deutsch](docs/install.md#uwurdp-installieren).

## The sync server

The server is a separate repo, [UwUSSH-Server](https://github.com/MinifyX/UwUSSH-Server):
one Rust binary, one Docker image, one SQLite file, its own TLS certificate
whose fingerprint each device pins like an SSH host key. It installs with one
command and prints a setup code; paste that into **Settings → Sync → Connect a
server**, and UwURDP shows the recovery kit once. Another device joins with
the code **Add a device** shows — three words over SPAKE2, the master password
typed on each device and never sent.

The server only ever holds ciphertext, and without the account key from the
recovery kit even its copy of the wrapped vault key is worth nothing. And if
you'd rather not run a server at all, "local only" is a first-class choice,
not a downgrade.

## Project layout

| Path                   | What lives there                                                        |
| ---------------------- | ----------------------------------------------------------------------- |
| `apps/desktop`         | The Tauri 2 app (React UI + Rust shell)                                 |
| `apps/keygen`          | UwUKeygen, the standalone key generator                                 |
| `apps/setup`           | The installer, updater and uninstaller, for all three systems           |
| `apps/desktop/e2e`     | End-to-end run against a real SSH server                                |
| `crates/uwurdp-core`   | Session engine: SSH, SFTP, local shells, flow control, system detection |
| `crates/uwurdp-store`  | SQLite: hosts, groups, keys, trusted host keys, export files            |
| `crates/uwurdp-vault`  | Key derivation, record encryption                                       |
| `crates/uwurdp-keygen` | Key generation: RSA, Ed25519, ECDSA; OpenSSH, PuTTY and PEM output      |
| `crates/uwurdp-sync`   | Sync client: clocks, outbox, merging                                    |
| `crates/uwurdp-import` | PuTTY, KiTTY, `ssh_config`, Termius                                     |
| `crates/uwurdp-proto`  | Shared types between client and server                                  |
| `brand/`               | Nyu: the UwURDP and UwUKeygen icons, symbol, mono symbol                |
| `docs/`                | Vision, architecture, design, roadmap                                   |
| `release-notes/`       | What's new, per version                                                 |
| `scripts/`             | Building the setup, releasing                                           |

## Development

Requirements:

- Node.js 24 and pnpm 11 (`corepack enable`)
- Rust stable (via [rustup](https://rustup.rs))
- Platform prerequisites for Tauri: see
  [tauri.app/start/prerequisites](https://tauri.app/start/prerequisites/)
  (Windows: Visual Studio C++ Build Tools and WebView2; Linux:
  `libwebkit2gtk-4.1-dev` and friends, plus `libdbus-1-dev`)

```bash
pnpm install
pnpm tauri dev
```

No server at hand? A toy SSH server for trying things out — `127.0.0.1:2222`,
user `uwu`, password `nyu`:

```bash
cargo run -p uwurdp-core --example dev_sshd
```

Checks:

```bash
pnpm typecheck && pnpm lint
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
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
- [Konzept](KONZEPT.md) — the full concept, in German
- [Vision](docs/vision.md) — what I want UwURDP to be and what it will never do
- [Architecture](docs/architecture.md) — how the pieces fit together
- [M0 spike](docs/m0-spike.md) — the throughput measurement everything else waits on
- [Design](docs/design.md) — colors, type, Nyu, tone of voice
- [Roadmap](docs/roadmap.md) — my wish list, without dates
- [Security review](docs/security-review-2026-09.md) — what was checked before each beta, and fixed

## License

UwURDP is free software under the [GNU GPL v3.0](LICENSE): use it, change it,
fork it, share it. If you pass on a changed version, its source has to stay
open too.
