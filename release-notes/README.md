# Release notes

One file per version, named after it: `0.1.0.json`, `0.1.0-beta.1.json`. `pnpm release` refuses to run
without it. The text appears under "Was ist neu?" in UwURDP's update hint and on the GitHub release page.

```json
{
  "de": "- Kurze, verständliche Punkte\n- Was Leute merken, nicht wie es gebaut ist",
  "en": "- Short, plain points\n- What people notice, not how it's built"
}
```

## Releasing a version

1. Set the version in `Cargo.toml` (workspace), the `tauri.conf.json` of `apps/desktop` and `apps/setup`,
   and the `package.json` files.
2. Add `release-notes/<version>.json`.
3. Commit, tag `v<version>` and push both. The tag starts `.github/workflows/installers.yml`, which
   checks the workspace on macOS and Linux and builds their setups — unsigned, since CI holds no key.
4. Run `pnpm release` on Windows, with the tag checked out.

`pnpm release` builds the app, packs it into `UwURDP-windows-x64-setup.exe` (`pnpm build:setup`),
waits for the tag's CI run and downloads the rest: the Windows ARM setup, the universal macOS disk image
and its update program, and for Linux x64 and arm64 the `.deb`, `.rpm` and portable `.tar.gz`, plus
the x64 setup AppImage for copies the old Linux setup installed. It signs every file the updater runs —
each as a copy under the versioned name installed apps check for (`UwURDP-Setup-<version>.exe`,
`UwURDP-<version>-linux-x86_64.deb`, …), while the release carries the same bytes under names without a
version — checks each signature against the key in `tauri.conf.json`, creates the GitHub release with
all of it and a `SHA256SUMS.txt`, and writes the update feeds (Windows, macOS, the Linux setup and the
Linux packages) to the `updates` branch. It waits until everything is online and checks it, then writes
the AUR package `uwurdp-bin` (`scripts/aur.mjs`) to `target/aur/uwurdp-bin` — or commits and pushes
it from the checkout `UWURDP_AUR_DIR` points at. CI's `aur.yml` pushes it too once the secret
`AUR_SSH_PRIVATE_KEY` is set. If CI can't build for some reason, `pnpm release --windows-only`
publishes Windows alone.

It needs the update signing key, either as `TAURI_SIGNING_PRIVATE_KEY` +
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` or as a folder with `uwurdp-update.key` and `PASSWORT.txt` in
`UWURDP_UPDATE_KEY_DIR` (default: `Documents\UwURDP-Update-Schluessel`). The key never goes into this
repository, and the app and setup builds never see it: only the signing step does. Losing it means
installed apps can't take updates any more, so keep a copy somewhere safe.

## Channels

Versions with a suffix (`-beta.1`) go to the Beta feed only; plain versions go to Stable and Beta. An
installed beta starts on the Beta channel, everything else on Stable; Settings → Updates switches.
