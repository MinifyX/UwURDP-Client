# Design

Clean, bright, soft — with a wink. Same design system as
[UwUSSH](https://github.com/MinifyX/UwUSSH-Client) and
[UwUMail](https://github.com/MinifyX/UwUMail-Client), same cat, one confident
bubblegum pink. The remote desktop is the one place that belongs to someone
else: UwURDP frames it and otherwise keeps out of its way.

## Color

Tokens come from UwUMail unchanged, including the `--uwu-*` naming, and live in
`apps/desktop/src/styles/tokens.css`. Components never use raw hex values.

| Token              | Light     | Dark      | Use                                            |
| ------------------ | --------- | --------- | ---------------------------------------------- |
| `--uwu-canvas`     | `#f8f4f6` | `#141016` | App background                                 |
| `--uwu-surface`    | `#ffffff` | `#1c171f` | Host tree, panels, cards                       |
| `--uwu-elevated`   | `#fcf8fa` | `#241e28` | Hover rows, popovers                           |
| `--uwu-ink`        | `#1c1420` | `#f8f2f6` | Primary text                                   |
| `--uwu-muted`      | `#716672` | `#b3a8b3` | Secondary text                                 |
| `--uwu-hairline`   | `#f2e8ee` | `#2c2430` | Dividers                                       |
| `--uwu-border`     | `#e9dde4` | `#3a3040` | Control borders                                |
| `--uwu-pink`       | `#ff4d8d` | `#ff7fac` | **Brand.** Status dots, selection, focus, logo |
| `--uwu-pink-solid` | `#e11d74` | `#ff7fac` | Filled buttons with text                       |
| `--uwu-pink-tint`  | `#ffe4ef` | `#3a1a2a` | Selected host, active row                      |

**Why two pinks?** White text on `#ff4d8d` reaches only 3.1:1. Filled buttons
therefore use `#e11d74` (4.5:1, WCAG AA). The brighter brand pink stays for
everything that is not small text on a pink fill.

Connection state uses semantic color, separate from the brand: online is mint,
disconnected is muted grey, and a certificate problem is amber — never pink,
because pink means "selected" everywhere else.

## The desktop is always dark

Whatever the app's theme, the area a remote desktop sits in is dark: the
letterbox around a scaled desktop, the space a smaller desktop leaves, the
dimmed last picture of a disconnected session, full screen. A light frame
around a Windows desktop glares, and the desktop's own colours are the
server's business — UwURDP never tints, filters or rounds them.

The tiles in the overview follow the same rule: dark tiles with the state of
the session, the host name and status on the card underneath.

## Type

- **Manrope** (variable, bundled, no network) for the interface.
- **JetBrains Mono** for fingerprints, addresses and anything to compare
  character by character.
- Sizes: 12 caption · 13 meta · 14 body/list · 16 panel body · 18 section ·
  22 title. Weights 400, 500, 600 for titles and host names, 700 only for the
  wordmark.

## Shape and space

- Radius: 10px controls, 16px cards and panes, 999px pills and badges. The
  desktop itself is never rounded.
- Spacing on a 4px grid.
- Shadows only for floating layers: menus, dialogs, the connection bar, toasts.

## Layout

```
┌────────────┬──────────────────────────────────────────────┐
│ Sidebar    │ Tab bar  [Overview] [dc-01] [sql-02] [+]     │
│            ├──────────────────────────────────────────────┤
│ Search     │                                              │
│ ▸ Kunde A  │          remote desktop (canvas)             │
│   ● dc-01  │                                              │
│   ○ fs-01  │                                              │
│ ▸ Homelab  │                                              │
└────────────┴──────────────────────────────────────────────┘
```

- Custom title bar, no OS chrome edge, with its own minimize, maximize/restore
  and close buttons at Windows' own size; close turns brand pink on hover, as in
  the installer.
- The sidebar has two workspaces, **Private** and **Business** (renamable), as
  pills with a count each. Groups fold; hosts and groups move by dragging.
  Right-click a group for **Group login…**, **Connect all**, **Disconnect
  all** and its overview.
- **Tabs** sit above the desktop, one per session. The active tab has a pink
  top edge, a status dot says connecting (pulsing pink), online (mint) or
  disconnected (grey), and a second tab to the same host gets a small number.
- The **overview** is a tab of its own: a grid of tiles, one per session. A click
  switches to the session; a host that isn't connected shows a connect button
  in its place. With nothing open, Nyu naps.
- **Full screen** hides everything but the desktop and an mstsc-like
  connection bar at the top edge: host name, Ctrl+Alt+Del, leave full screen,
  disconnect. Outside full screen the same actions, plus fit/scroll, sit in a
  quiet toolbar above the desktop.
- **Disconnect is a state, not a modal.** The tab keeps the last picture,
  dimmed, with the reason and a reconnect button over it. A modal over a
  desktop is a UX bug, not a safety feature.
- Keyboard-first outside the desktop: everything reachable without the mouse,
  visible focus rings. Inside the desktop, the keyboard belongs to the server.

## Nyu, the mascot

Nyu is the same cat as in UwUMail and UwUSSH — this time her hull is a
**monitor on a stand**. Ears poking out above the bezel, the screen is the
face: UwU eyes, `w` mouth, blush.

- **Sticker style**, unchanged. Plum outlines `#4B1D3F`, pink body `#FF6FA6`,
  light screen `#FFB8D3`, pastel props, a white die-cut edge. The colors are
  fixed artwork and stay the same in dark mode; the white edge keeps the
  outlines readable on dark backgrounds.
- **App icon** (website, GitHub, macOS Dock). Built like UwUMail's and
  UwUSSH's: Nyu as a pink monitor on a stand with cat ears, slightly tilted,
  the night-blue screen showing the UwU face and a yellow mouse pointer. The
  heart top left, a small star left, a big star bottom right.
- **The tile.** Every UwU app's icon for the website and GitHub sits on
  UwUMail's pastel pink tile (`#FFF3F8` to `#FFD3E5`), never another colour.
  Each one gets sparkles and a heart, arranged differently around it.
- **Taskbar icon.** On the Windows taskbar, in the setup and in Linux menus
  Nyu stands alone: upright, no tile, white die-cut edge. The monitor on its
  stand shows a window from another computer with the pointer reaching into
  it, so it reads as remote desktop, not as a terminal; the ears and blush
  say Nyu (`brand/uwurdp-taskbar-icon.svg`). At 16 and 24 px a simplified cut
  takes over (`uwurdp-taskbar-icon-small.svg`). `node scripts/icons.mjs`
  regenerates all desktop icons from these three.
- **The face.** Wherever Nyu has one, it's UwU: two U eyes and a **round `w`**
  (two soft arcs, never a zigzag).
- **Sources** in `brand/` (icon, symbol, mono symbol) and
  `apps/desktop/src/components/nyu/` (React).

**The installer** (`apps/setup`) is UwUSSH's setup with the monitor cat: the
same pink gradient window, Nyu waving hello, busy while installing, cheering
when it's done, and waving goodbye on uninstall. Its scenes share
`components/nyu/` with the app.

**Scenes** (`NyuScene`, 320 × 220), for the moments an RDP client actually has:

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

**Motion.** Nyu blinks in scenes and twitches her ears on hover. Settings →
Appearance → Animations (System / On / Off) resolves to
`<html data-motion="full|reduced">`; with `reduced`, all animation collapses to
1 ms and Nyu holds still.

## Tone of voice

Warm and a little playful: kaomoji now and then, small jokes in empty states,
soft animation. The interface speaks German and English (following the
system, switchable under Settings → Appearance → Language); German strings are
the source and use "du".

Rules:

1. **Information first.** The joke never replaces what happened or what to do.
2. **Short.** One kaomoji at most, never in buttons that act on data.
3. **Kind.** Never mock the user; the app laughs at itself.
4. **Security is never playful.** A changed certificate, a failed vault
   unlock, a rejected login: no kaomoji, no Nyu. A cute face next to a possible
   man-in-the-middle warning destroys exactly what the warning is for.

Rule 4 is not negotiable:

```
⚠  The certificate of dc-01.corp.example changed.

  known        SHA256:nThbg6kX…UmcQ2p4   since 2026-09-14
  now          SHA256:7Pq1Zx0v…Kd9Lm3s
  thumbprint   3F 9A 11 C2 … 7E 04

This can be a renewed certificate — or a man in the middle.

              [ Trust new certificate ]  [ Cancel ]   ← focus
```

RDP certificates renew on their own every few months, so this dialog will be
seen for harmless reasons. That is exactly why it stays plain: two buttons,
focus on the safe one, the thumbprint Windows shows so it can be compared with
the server itself.
