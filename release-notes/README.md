# Release notes

One file per version, named after it: `0.1.0.json`, `0.1.0-beta.1.json`. `pnpm release` refuses to run
without it. Changes not released yet collect in `unreleased.json`, in the same form;
releasing renames it to the version's file. The text appears under "Was ist neu?" in UwURDP's update hint and on the GitHub release page.

```json
{
  "de": "- Kurze, verständliche Punkte\n- Was Leute merken, nicht wie es gebaut ist",
  "en": "- Short, plain points\n- What people notice, not how it's built"
}
```

## Releasing a version

1. Set the version in `Cargo.toml` (workspace), the `tauri.conf.json` of `apps/desktop` and `apps/setup`,
   and the `package.json` files.
2. Rename `release-notes/unreleased.json` to `release-notes/<version>.json` (or add it), and write the
   first line of each language as the release's headline.
3. Commit, tag `v<version>` and push both. The tag starts `.github/workflows/installers.yml`, which
   checks the workspace on macOS and Linux and builds every setup — unsigned, since CI holds no key.
4. Run `pnpm release` on the machine with the signing key (any system), with the tag checked out.

`pnpm release` waits for the tag's CI run and downloads what it built: the Windows setups for x64
(`UwURDP-windows-x64-setup.exe`) and ARM, the universal macOS disk image and its update program, and
for Linux x64 and arm64 the `.deb`, `.rpm` and portable `.tar.gz`, plus the x64 setup AppImage for
copies the old Linux setup installed. With `--build-windows` it builds the Windows x64 setup on this
machine instead (`pnpm build:setup`, on Windows only), with `--no-build` it takes the one already in
`target/installers`. It signs every file the updater runs —
each as a copy under the versioned name installed apps check for (`UwURDP-Setup-<version>.exe`,
`UwURDP-<version>-linux-x86_64.deb`, …), while the release carries the same bytes under names without a
version — checks each signature against the key in `tauri.conf.json`, creates the GitHub release with
all of it and a `SHA256SUMS.txt`, and writes the update feeds (Windows, macOS, the Linux setup and the
Linux packages) to the `updates` branch. It waits until everything is online and checks it, then writes
the AUR package `uwurdp-bin` (`scripts/aur.mjs`) to `target/aur/uwurdp-bin` — or commits and pushes
it from the checkout `UWURDP_AUR_DIR` points at. CI's `aur.yml` pushes it too once the secret
`AUR_SSH_PRIVATE_KEY` is set. If CI can't build for some reason, `pnpm release --windows-only`
builds Windows x64 on this machine (on Windows) and publishes it alone.

`pnpm release --dry-run` does everything up to publishing — waits for CI, downloads, signs, checks the
signatures, builds the checksums, feeds and AUR package — and stops, publishing nothing. It runs on any
commit with a green Installers run, no tag needed: push the commit to a `ci/…` branch, which starts
that run.

It needs the update signing key, either as `TAURI_SIGNING_PRIVATE_KEY` +
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` or as a folder with `uwurdp-update.key` and `PASSWORT.txt` in
`UWURDP_UPDATE_KEY_DIR` (default: `Documents\UwURDP-Update-Schluessel`). The key never goes into this
repository, and the app and setup builds never see it: only the signing step does. Losing it means
installed apps can't take updates any more, so keep a copy somewhere safe.

## Channels

Versions with a suffix (`-beta.1`) go to the Beta feed only; plain versions go to Stable and Beta. An
installed beta starts on the Beta channel, everything else on Stable; Settings → Updates switches.
