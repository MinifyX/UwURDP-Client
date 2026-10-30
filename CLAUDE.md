# UwURDP-Client

Tauri-RDP-Client (RDCMan-Dichte) auf IronRDP mit eigenem Sync (UwUSync/UwULock). Version siehe `package.json`.

## Layout
- `crates/uwurdp-core` RDP-Engine (Verbindung, Frames, Clipboard, Config) · `-proto` · `-import` (RDCMan u. a.) · `-store` · `-sync` (UwUSync/UwULock) · `-vault` (Schlüsselableitung, Record-Verschlüsselung)
- `apps/desktop` Haupt-App (Vite/React + `src-tauri`) · `apps/setup` Windows-Setup-UI
- `vendor/` gepatchte Fremd-Crates (ironrdp-graphics, ironrdp-pdu), `brand/`, `packaging/aur`, `release-notes/`

## Befehle
- `pnpm test` läuft nur `apps/desktop` (`node --test test/*.test.mjs`); `pnpm lint` = Prettier + `scripts/check-i18n.mjs`.
- `node apps/desktop/e2e/run.mjs`: End-to-End lokal mit Spielzeug-RDP-Server (`crates/uwurdp-core/examples/dev_rdpd.rs`); braucht Desktop-Session, nicht in CI.
- `scripts/lock-live.sh`: Sync-Tests gegen echten UwULock-/UwUSync-Server (`UWULOCK_REPO=…`), `#[ignore]`, nicht in CI.

## Fallen
- `vendor/` nie von Hand umbauen, nicht linten/testen (in `.prettierignore`). Patches gelten für IronRDP-Rev `9b151c4c` bzw. ironrdp-pdu 0.9.0 (`[patch]` in `Cargo.toml`); bei IronRDP-Update `vendor/README.md` prüfen, ob die Kopie entfallen kann.
- `uwulock-core` ist per git-`rev` auf UwULock-Client `client-0.3` gepinnt (`Cargo.toml`); Pin bewusst anheben.
- UI-Texte deutsch in `t()`/`N_()`, Englisch in `apps/desktop/src/i18n/en/*.json`; danach `node scripts/check-i18n.mjs`.
- `@uwusuite/design` kommt als Release-Tarball-URL (`apps/desktop/package.json`); UI-Änderungen zuerst in UwUSuite-Design.
- `apps/*/src-tauri/gen/schemas/` ist generiert; `target/`, `wt/` nicht durchsuchen.

## Plan/Stand
- `KONZEPT.md`, `docs/roadmap.md`, `docs/architecture.md`; Reviews in `docs/security-review-<YYYY-MM>.md`.

## Release
- `pnpm release` (`scripts/release.mjs`, braucht lokalen Update-Signing-Key). Version in `Cargo.toml` (workspace), beiden `tauri.conf.json` und den `package.json` setzen.
- `release-notes/unreleased.json` (de/en) in `<version>.json` umbenennen, mergen, Tag `v<version>` auf Merge-Commit. Ohne die Datei bricht der Release ab.
- Weitere Kanäle: `build:mas` (Mac App Store, `docs/app-store.md`), `scripts/aur.mjs` (AUR).

## Security
- Kritisch: `crates/uwurdp-vault` (Krypto), `crates/uwurdp-sync` (Sync/Login), `crates/uwurdp-core` (CredSSP/TLS, Zertifikatsprüfung, Clipboard), `crates/uwurdp-proto`, `crates/uwurdp-import` (Parser).
- Vor jedem Feature-Release den globalen Subagent `security-reviewer` über den Bereich seit dem letzten Tag laufen lassen; Ergebnis nach `docs/security-review-<YYYY-MM>.md`.
