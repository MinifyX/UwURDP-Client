# Design

UwURDP looks like every UwU app because it is built from the suite's design
package, [@uwusuite/design](https://github.com/MinifyX/UwUSuite-Design): its
tokens, UwU Sans and the font picker, light, dark and high contrast, the
motion rules, the icons (Lucide through `Icon` and `ICONS`), the components
(Button, IconButton, Dialog, Switch, Segmented, SettingRow, StatusDot,
TitleBar, Wordmark, …) and Nyu. The rules for all of that live there, in its
`docs/` (color, typography, icons, components, window, motion, nyu, tone), and
the way an app moves onto it in its
[docs/migration.md](https://github.com/MinifyX/UwUSuite-Design/blob/main/docs/migration.md).

This page is only about what is special about UwURDP. The remote desktop is
the one place that belongs to someone else: UwURDP frames it and otherwise
keeps out of its way.

## Where things are

- `apps/desktop/src/styles/index.css` imports Tailwind, the package's
  `tailwind.css` and `font-picker.css`, then the app's own sheets into
  Tailwind's `components` layer (a utility class always wins over them):
  `app.css` (shell, certificates, import, settings, update hint),
  `sidebar.css` (host tree, workspaces, groups, dragging), `workspace.css`
  (tab bar, toolbar, notices, overlays), `forms.css` (the host form and the
  dialogs' fields), `features.css` (sync, drives, startup hosts) and
  `rdp.css` (the desktop, full screen, the overview).
- Theme, contrast and motion: Settings → Darstellung, through the package's
  `useAppearance()` (`lib/appearance.ts`); `/boot.js` (the package's
  `bootScript()`, emitted by `vite.config.ts`) puts them on `<html>` before
  the first paint. UwURDP is dark until the person picks something.
- The font: Settings → Darstellung → Schrift, the package's choices and
  `applyUiFont()`, per device. A stored font that is no longer offered falls
  back to UwU Sans.
- What a build can do (GitHub download or Mac App Store): `lib/build.ts`
  hides updates, H.264, "Alle Laufwerke" and the RDCMan scan where the build
  has none (docs/app-store.md).

## What is UwURDP's own

- **The desktop is always dark and pixel-exact.** Whatever the app's theme,
  the area a remote desktop sits in uses the package's `stage-*` tokens: the
  letterbox around a scaled desktop, the dimmed last picture of a
  disconnected session, full screen and its connection bar, the overview
  tiles. The canvas itself is never tinted, filtered, rounded or scaled by
  CSS at 1:1 (`image-rendering: pixelated`); only "Einpassen" scales it,
  smoothly.
- **Tabs** (`TabBar.tsx`) sit above the desktop, one per session: pink top
  edge on the active one, a status dot (connecting pulses pink, online mint,
  disconnected grey), a small number on a second tab to the same host. The
  overview is a tab of its own.
- **Host tree** (`HostList.tsx`): two workspaces, Privat and Business
  (renamable), groups that fold, hosts and groups moved by dragging, a
  context menu (`ContextMenu.tsx`, built like the package's `Menu`).
- **Disconnect is a state, not a modal.** The tab keeps the last picture,
  dimmed, with the reason and a reconnect button over it.
- **Dialogs** are the package's `Dialog` (`components/Modal.tsx`): a click
  beside a dialog doesn't close it, focus starts on the safe choice
  (`data-autofocus`, never on a `data-secondary` button). Native checkboxes
  in the suite's colours where a list is picked from, `Toggle`/`Switch` for
  on/off settings, native selects in the control look.
- **Keyboard**: inside the desktop every key belongs to the server, except
  mstsc's Ctrl+Alt combinations (Ctrl on a Mac too) and, on a Mac, ⌘ keys,
  which go to the menu bar.
- **Windows and Linux**: the package's `TitleBar` with the Wordmark and the
  settings button; a double-click maximizes once (`useTauriWindow`).
- **macOS** (package `docs/macos.md`): the system's title bar with the
  traffic lights (`tauri.macos.conf.json`), the title bar's actions in the
  menu bar (`setMacMenu()` in `App.tsx`): Einstellungen … on ⌘,, Ablage with
  Host hinzufügen (⌘N), Import, Export and **Tab schließen** (⌘W while a tab
  is open), Darstellung → Übersicht (⌘⇧O), a **Verbindung** menu (Strg+Alt+Entf,
  Einpassen, Vollbild, weiterer Tab ⌘⇧D, Trennen/Neu verbinden). With no tab
  open ⌘W hides the window and a click on the Dock icon brings it back. ⌘Q,
  the Dock and logging out go through the `uwu-macos` quit guard, which still
  asks while sessions are open. Tooltips write shortcuts the platform's way
  (`lib/shortcuts.ts`, `withShortcut()`).
- **App icons** come from `brand/` through the package's tool:
  `pnpm --filter @uwurdp/desktop icons` (`uwu-icons`), which also sets the
  Dock icon into Apple's grid.

## Nyu, the monitor cat

Nyu's hull is a **monitor on a stand** here: ears above the bezel, the screen
is her face. The ears, the face, the sticker edge and the palette are the
package's (`NyuEars`, `NyuFace`, `Sticker`, `NYU`); the monitor shell is
`components/nyu/Nyu.tsx`, and the package's catalogue draws the same cat
(`shell="monitor"`). The installer (`apps/setup`) uses the same cat.

**Scenes** (`NyuScene`, 320 × 220, the app's own):

| Scene      | When                                                |
| ---------- | --------------------------------------------------- |
| Pick       | No tab open — pick a host                           |
| Connecting | A tab waiting for its connection                    |
| Load error | A connection that failed before the desktop came up |
| Sleepy     | The overview with nothing open                      |
| Done       | Import or export finished                           |
| Vault      | Creating or unlocking the vault — Nyu on the safe   |
| Welcome    | Settings → Sync before a server is connected        |
| Keys       | The recovery kit                                    |
| Goodbye    | Closing with sessions still open                    |

## Tone of voice

The suite's tone (package `docs/tone.md`), with one rule that is not
negotiable here: **security is never playful.** A changed certificate, a
failed vault unlock, a rejected login: no kaomoji, no Nyu. RDP certificates
renew on their own every few months, so the certificate dialog will be seen
for harmless reasons — that is exactly why it stays plain: two buttons, focus
on the safe one, the thumbprint Windows shows so it can be compared with the
server itself.
