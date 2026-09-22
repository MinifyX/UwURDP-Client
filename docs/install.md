# Installing UwURDP

[Deutsch weiter unten](#uwurdp-installieren)

UwURDP runs on **Windows 10 and 11** (x64 and ARM), **macOS 11 or newer**
(Apple silicon and Intel) and **Linux** (x86_64 and arm64). On Windows and
macOS the setup installs for your user only — no admin rights. UwURDP speaks
English or German, following the system; **Settings → Appearance → Language**
switches.

Windows and macOS get UwURDP's own setup with Nyu in it, Linux a package for
your distribution or a portable folder. Download from the
[releases](https://github.com/MinifyX/UwURDP-Client/releases): take the newest
one at the top. Betas are marked **Pre-release**; the newest version without
that mark is the stable one. The file names carry no version, so
`https://github.com/MinifyX/UwURDP-Client/releases/latest/download/<file>`
always gets the newest stable one.

| System                     | File under **Assets**                                               |
| -------------------------- | ------------------------------------------------------------------- |
| Windows 10/11 (x64)        | `UwURDP-windows-x64-setup.exe`                                      |
| Windows 11 on ARM          | `UwURDP-windows-arm64-setup.exe`                                    |
| macOS (Intel & Apple chip) | `UwURDP-macos-universal.dmg`                                        |
| Ubuntu / Debian            | `UwURDP-linux-x64.deb` · ARM: `UwURDP-linux-arm64.deb`              |
| Fedora / openSUSE          | `UwURDP-linux-x64.rpm` · ARM: `UwURDP-linux-arm64.rpm`              |
| Arch Linux                 | AUR: `yay -S uwurdp-bin`                                            |
| Linux, portable            | `UwURDP-linux-x64-portable.tar.gz` · ARM: `…-arm64-portable.tar.gz` |

The `UwURDP-update-…` files next to them are for the in-app updater; you
don't need them.

**Checking the download (optional).** Each release has a `SHA256SUMS.txt`. On
macOS and Linux: `shasum -a 256 -c SHA256SUMS.txt --ignore-missing` in the
download folder. On Windows, in PowerShell:
`Get-FileHash "$env:USERPROFILE\Downloads\UwURDP-windows-x64-setup.exe"` and
compare with the line in the file.

## Windows

Double-click the setup. Windows will most likely show **"Windows protected your
PC"**: the setup isn't signed with a paid code-signing certificate, so
SmartScreen doesn't know it yet. Click **More info**, then **Run anyway**. Your
browser may also say the file is "not commonly downloaded"; keep it anyway (in
Edge: `…` → **Keep** → **Show more** → **Keep anyway**).

- **Install** sets everything up in a few seconds.
- **Options** lets you change the folder (default
  `%LOCALAPPDATA%\Programs\UwURDP`), turn off the desktop shortcut, or leave out
  **UwUKeygen**, the SSH key generator that comes along.
- If Microsoft Edge WebView2 is missing (Windows 11 always has it), the setup
  offers to download and install it.

Uninstall from **Windows Settings → Apps → Installed apps → UwURDP**.

## macOS

Open the `.dmg` and double-click **UwURDP Setup**. UwURDP isn't notarized by
Apple (that needs a paid developer account), so the first time macOS says it
can't check the app. Then:

1. Open **System Settings → Privacy & Security**.
2. Scroll down: next to "UwURDP Setup was blocked", click **Open Anyway** and
   confirm.

(On macOS 14 and older, right-clicking the setup and choosing **Open** works
too.) The setup installs **UwURDP** and **UwUKeygen** into `/Applications`, or
into `~/Applications` if your user may not write to `/Applications`. The apps
it installs start without that question.

To uninstall, run the setup again and choose **Uninstall …** — it asks whether
to keep your hosts and vault. Dragging the apps to the Trash works too, but
leaves the data in `~/Library/Application Support/app.uwurdp.desktop`.

## Linux

**Ubuntu, Debian and relatives:** `sudo apt install ./UwURDP-linux-x64.deb`
(`…-arm64.deb` on ARM). **Fedora, openSUSE:**
`sudo dnf install ./UwURDP-linux-x64.rpm` or
`sudo zypper install ./UwURDP-linux-x64.rpm`. Both install the app
system-wide as package `uwurdp`, with a menu entry, using the system's
WebKitGTK 4.1, and update themselves: UwURDP downloads the next package and
installs it on **Restart now**, asking for the administrator password.
Uninstall with `sudo apt remove uwurdp` or `sudo dnf remove uwurdp`.
UwUKeygen, the key generator, only comes with the Windows and macOS setups.

**Arch Linux:** `yay -S uwurdp-bin` (or any other AUR helper). pacman updates
it, not UwURDP itself.

**Portable:** unpack `UwURDP-linux-x64-portable.tar.gz` anywhere and start
`./UwURDP/uwurdp`. It brings its own WebKit, installs nothing and doesn't
update itself — fetch the newest one to update.

**Installed with the setup AppImage of UwURDP 0.1** (into
`~/.local/share/uwurdp`)? That copy keeps updating itself as before. To move
to a package instead, uninstall it (**Uninstall …** in the setup; keep hosts
and vault) and install the package: both use the same data.

**Remembering the vault on Linux** uses the Secret Service (GNOME Keyring,
KWallet). Without one — a bare window manager — UwURDP keeps its key in a file
only your user can read, which protects less against someone with your disk.

## First steps

- **Add a host** with `+` in the sidebar: address, user, and a password or a
  key.
- **Or bring your hosts along**: the import button next to it reads Termius,
  PuTTY, KiTTY (from the registry, or a portable KiTTY's `Sessions` folder and
  `.reg` exports) and `~/.ssh/config`.
- **Passwords and keys** go into an encrypted vault. The first time you save
  one, you pick a master password. Tick "remember on this device" if your
  account should open the vault on its own.
- On first contact with a server you are shown its host key fingerprint. Trust
  it only if it is the one you expect.
- A click on a host opens a connection in a tab — or shows the tab that is
  already open. Right-click the host for **Open another tab**.
- Nothing opens on its own when UwURDP starts. **Settings → Terminal → Open on
  start** can open a local shell or chosen hosts instead.
- **Several computers?** Settings → Sync connects a
  [UwUSSH server](https://github.com/MinifyX/UwUSSH-Server) of your own, and
  keeps hosts, keys and passwords the same everywhere, end-to-end encrypted.

## Updates

UwURDP updates itself: about 20 seconds after it starts, and every six hours,
it looks for a newer version, downloads it quietly (signed and checked) and
offers a restart. **Settings → Updates** switches between the Beta and Stable
channels. Stable only gets versions without a beta mark.

A newer setup can also simply be run over an installed UwURDP, a newer package
installed over the old one. Hosts, the vault and settings stay. The portable
folder doesn't update itself, and the Arch package updates with pacman.

## Where your data lives

| What                                | Windows                              | macOS                                              | Linux                               |
| ----------------------------------- | ------------------------------------ | -------------------------------------------------- | ----------------------------------- |
| Hosts, groups, host keys, the vault | `%APPDATA%\app.uwurdp.desktop\`      | `~/Library/Application Support/app.uwurdp.desktop` | `~/.local/share/app.uwurdp.desktop` |
| App settings (look, terminal, …)    | `%LOCALAPPDATA%\app.uwurdp.desktop\` | `~/Library/WebKit/app.uwurdp.desktop`              | `~/.local/share/app.uwurdp.desktop` |
| The program                         | `%LOCALAPPDATA%\Programs\UwURDP\`    | `/Applications/UwURDP.app`                         | `/usr/bin/uwurdp-desktop`           |

Passwords and private keys are only stored encrypted. To move to another
computer without a sync server: **Settings → Import & Export** writes everything
into one `.uwurdp` file, sealed with a password of its own, which UwURDP on the
other computer reads back in.

## If something goes wrong

- **"WebView2 couldn't be installed"** (Windows): install the Evergreen WebView2
  Runtime from [Microsoft](https://developer.microsoft.com/microsoft-edge/webview2/)
  and run the setup again.
- **An antivirus program blocks the setup**: that is the same missing
  certificate as SmartScreen's warning. The source of every release is in this
  repository, and the checksums tell you the file is the published one.
- **macOS says the app is damaged**: that happens when the quarantine mark
  survives on the installed app, which the setup avoids. Running
  `xattr -dr com.apple.quarantine /Applications/UwURDP.app` clears it.
- **Windows on ARM** has its own setup (`UwURDP-windows-arm64-setup.exe`); the
  x64 one runs there too, emulated and slower.
- **A package update fails** (no `pkexec`, or the password prompt was
  cancelled): download the newest `.deb` / `.rpm` and install it as above.
- Something else? [Open an issue](https://github.com/MinifyX/UwURDP-Client/issues)
  — no promises on how fast, see the README.

Building it yourself instead: [Development](../README.md#development).

---

# UwURDP installieren

UwURDP läuft unter **Windows 10 und 11** (x64 und ARM), **macOS 11 oder
neuer** (Apple-Chip und Intel) und **Linux** (x86_64 und arm64). Unter Windows
und macOS installiert das Setup nur für deinen Benutzer — ohne Adminrechte.
UwURDP spricht Deutsch oder Englisch, je nach System; **Einstellungen →
Darstellung → Sprache** schaltet um.

Windows und macOS bekommen UwURDPs eigenes Setup mit Nyu, Linux ein Paket für
deine Distribution oder einen portablen Ordner. Lade von den
[Releases](https://github.com/MinifyX/UwURDP-Client/releases) herunter: das
neueste ganz oben. Betas sind als **Pre-release** markiert; die neueste
Version ohne diese Markierung ist die stabile. Die Dateinamen enthalten keine
Version, `https://github.com/MinifyX/UwURDP-Client/releases/latest/download/<Datei>`
holt also immer die neueste stabile.

| System                     | Datei unter **Assets**                                              |
| -------------------------- | ------------------------------------------------------------------- |
| Windows 10/11 (x64)        | `UwURDP-windows-x64-setup.exe`                                      |
| Windows 11 auf ARM         | `UwURDP-windows-arm64-setup.exe`                                    |
| macOS (Intel & Apple-Chip) | `UwURDP-macos-universal.dmg`                                        |
| Ubuntu / Debian            | `UwURDP-linux-x64.deb` · ARM: `UwURDP-linux-arm64.deb`              |
| Fedora / openSUSE          | `UwURDP-linux-x64.rpm` · ARM: `UwURDP-linux-arm64.rpm`              |
| Arch Linux                 | AUR: `yay -S uwurdp-bin`                                            |
| Linux, portabel            | `UwURDP-linux-x64-portable.tar.gz` · ARM: `…-arm64-portable.tar.gz` |

Die `UwURDP-update-…`-Dateien daneben sind für den Updater in der App; du
brauchst sie nicht.

**Download prüfen (optional).** Jedes Release hat eine `SHA256SUMS.txt`. Unter
macOS und Linux im Download-Ordner: `shasum -a 256 -c SHA256SUMS.txt
--ignore-missing`. Unter Windows in PowerShell:
`Get-FileHash "$env:USERPROFILE\Downloads\UwURDP-windows-x64-setup.exe"` und
mit der Zeile in der Datei vergleichen.

## Windows

Doppelklick auf das Setup. Windows zeigt sehr wahrscheinlich **„Der Computer
wurde durch Windows geschützt“**: Das Setup ist nicht mit einem
kostenpflichtigen Code-Signing-Zertifikat signiert, deshalb kennt SmartScreen es
noch nicht. Klick auf **Weitere Informationen**, dann auf **Trotzdem
ausführen**. Der Browser meldet vielleicht, die Datei werde „nicht häufig
heruntergeladen“; behalte sie trotzdem (in Edge: `…` → **Beibehalten** → **Mehr
anzeigen** → **Trotzdem beibehalten**).

- **Installieren** richtet alles in ein paar Sekunden ein.
- Unter **Optionen** änderst du den Ordner (Standard
  `%LOCALAPPDATA%\Programs\UwURDP`), schaltest die Desktop-Verknüpfung ab oder
  lässt **UwUKeygen** weg, den SSH-Schlüssel-Generator, der mitkommt.
- Fehlt Microsoft Edge WebView2 (Windows 11 hat es immer), bietet das Setup an,
  es herunterzuladen und zu installieren.

Deinstallieren über **Windows-Einstellungen → Apps → Installierte Apps →
UwURDP**.

## macOS

Die `.dmg` öffnen und **UwURDP Setup** doppelklicken. UwURDP ist nicht bei
Apple notarisiert (das braucht einen kostenpflichtigen Entwickler-Account),
deshalb sagt macOS beim ersten Mal, es könne die App nicht prüfen. Dann:

1. **Systemeinstellungen → Datenschutz & Sicherheit** öffnen.
2. Nach unten scrollen: neben „UwURDP Setup wurde blockiert“ auf **Trotzdem
   öffnen** klicken und bestätigen.

(Unter macOS 14 und älter geht auch Rechtsklick auf das Setup → **Öffnen**.) Das
Setup installiert **UwURDP** und **UwUKeygen** nach `/Applications`, oder nach
`~/Applications`, wenn dein Benutzer nicht in `/Applications` schreiben darf.
Die installierten Apps starten ohne diese Rückfrage.

Deinstallieren: das Setup noch einmal starten und **Deinstallieren …** wählen —
es fragt, ob Hosts und Tresor bleiben sollen. Die Apps in den Papierkorb ziehen
geht auch, lässt aber die Daten in
`~/Library/Application Support/app.uwurdp.desktop` liegen.

## Linux

**Ubuntu, Debian und Verwandte:** `sudo apt install ./UwURDP-linux-x64.deb`
(`…-arm64.deb` auf ARM). **Fedora, openSUSE:**
`sudo dnf install ./UwURDP-linux-x64.rpm` oder
`sudo zypper install ./UwURDP-linux-x64.rpm`. Beide installieren die App
systemweit als Paket `uwurdp`, mit Eintrag im Anwendungsmenü, nutzen das
WebKitGTK 4.1 des Systems und aktualisieren sich selbst: UwURDP lädt das
nächste Paket und installiert es bei **Jetzt neu starten**, nach Eingabe des
Administrator-Passworts. Deinstallieren mit `sudo apt remove uwurdp` bzw.
`sudo dnf remove uwurdp`. UwUKeygen, der Schlüssel-Generator, kommt nur mit
den Setups für Windows und macOS.

**Arch Linux:** `yay -S uwurdp-bin` (oder ein anderer AUR-Helfer).
Aktualisiert wird es von pacman, nicht von UwURDP selbst.

**Portabel:** `UwURDP-linux-x64-portable.tar.gz` irgendwo entpacken und
`./UwURDP/uwurdp` starten. Bringt sein eigenes WebKit mit, installiert nichts
und aktualisiert sich nicht — zum Aktualisieren die neueste holen.

**Mit dem Setup-AppImage von UwURDP 0.1 installiert** (nach
`~/.local/share/uwurdp`)? Diese Kopie aktualisiert sich weiter wie bisher. Wer
lieber ein Paket hätte: sie deinstallieren (**Deinstallieren …** im Setup;
Hosts und Tresor behalten) und das Paket installieren — beide nutzen dieselben
Daten.

**Tresor merken unter Linux** nutzt den Secret Service (GNOME Keyring, KWallet).
Ohne einen — etwa unter einem reinen Fenstermanager — legt UwURDP seinen
Schlüssel in eine Datei, die nur dein Benutzer lesen kann; das schützt weniger
gegen jemanden mit deiner Festplatte.

## Erste Schritte

- **Host anlegen** mit `+` in der Seitenleiste: Adresse, Benutzer und ein
  Passwort oder ein Key.
- **Oder Hosts mitbringen**: Der Import-Knopf daneben liest Termius, PuTTY,
  KiTTY (aus der Registry oder aus dem `Sessions`-Ordner eines portablen KiTTY
  und aus `.reg`-Exporten) und `~/.ssh/config`.
- **Passwörter und Keys** landen in einem verschlüsselten Tresor. Beim ersten
  Speichern legst du ein Master-Passwort fest. Mit „Auf diesem Gerät merken“
  öffnet dein Benutzerkonto den Tresor von selbst.
- Beim ersten Kontakt mit einem Server siehst du den Fingerprint seines
  Host-Keys. Vertrau ihm nur, wenn es der erwartete ist.
- Ein Klick auf einen Host öffnet eine Verbindung in einem Tab — oder zeigt den
  Tab, der schon offen ist. Rechtsklick auf den Host → **Weiteren Tab öffnen**.
- Beim Start öffnet UwURDP nichts von selbst. **Einstellungen → Terminal → Beim
  Start öffnen** kann stattdessen eine lokale Shell oder bestimmte Hosts öffnen.
- **Mehrere Rechner?** Einstellungen → Sync verbindet einen eigenen
  [UwUSSH-Server](https://github.com/MinifyX/UwUSSH-Server) und hält Hosts, Keys
  und Passwörter überall gleich, Ende-zu-Ende-verschlüsselt.

## Updates

UwURDP aktualisiert sich selbst: etwa 20 Sekunden nach dem Start und danach
alle sechs Stunden sucht es nach einer neuen Version, lädt sie still herunter
(signiert und geprüft) und bietet einen Neustart an. **Einstellungen → Updates**
wechselt zwischen den Kanälen Beta und Stabil. Stabil bekommt nur Versionen
ohne Beta-Markierung.

Ein neueres Setup kann auch einfach über ein installiertes UwURDP laufen, ein
neueres Paket über das alte installiert werden. Hosts, Tresor und Einstellungen
bleiben. Der portable Ordner aktualisiert sich nicht selbst, das Arch-Paket mit
pacman.

## Wo deine Daten liegen

| Was                               | Windows                              | macOS                                              | Linux                               |
| --------------------------------- | ------------------------------------ | -------------------------------------------------- | ----------------------------------- |
| Hosts, Gruppen, Host-Keys, Tresor | `%APPDATA%\app.uwurdp.desktop\`      | `~/Library/Application Support/app.uwurdp.desktop` | `~/.local/share/app.uwurdp.desktop` |
| App-Einstellungen (Aussehen, …)   | `%LOCALAPPDATA%\app.uwurdp.desktop\` | `~/Library/WebKit/app.uwurdp.desktop`              | `~/.local/share/app.uwurdp.desktop` |
| Das Programm                      | `%LOCALAPPDATA%\Programs\UwURDP\`    | `/Applications/UwURDP.app`                         | `/usr/bin/uwurdp-desktop`           |

Passwörter und private Keys werden nur verschlüsselt gespeichert. Für einen
Umzug ohne Sync-Server: **Einstellungen → Import & Export** schreibt alles in
eine `.uwurdp`-Datei, versiegelt mit einem eigenen Passwort, die UwURDP auf dem
anderen Rechner wieder einliest.

## Wenn etwas nicht klappt

- **„WebView2 couldn't be installed“** (Windows): Installiere die Evergreen
  WebView2 Runtime von
  [Microsoft](https://developer.microsoft.com/microsoft-edge/webview2/) und
  starte das Setup noch einmal.
- **Ein Virenscanner blockiert das Setup**: Das ist dasselbe fehlende Zertifikat
  wie bei SmartScreen. Der Quellcode jedes Releases liegt in diesem Repository,
  und die Prüfsummen zeigen dir, dass die Datei die veröffentlichte ist.
- **macOS sagt, die App sei beschädigt**: Das passiert, wenn die
  Quarantäne-Markierung an der installierten App hängen bleibt, was das Setup
  vermeidet. `xattr -dr com.apple.quarantine /Applications/UwURDP.app` entfernt
  sie.
- **Windows auf ARM** hat ein eigenes Setup
  (`UwURDP-windows-arm64-setup.exe`); das x64-Setup läuft dort auch, emuliert
  und langsamer.
- **Ein Paket-Update klappt nicht** (kein `pkexec`, oder die Passwortabfrage
  abgebrochen): die neueste `.deb` / `.rpm` herunterladen und wie oben
  installieren.
- Etwas anderes? [Issue aufmachen](https://github.com/MinifyX/UwURDP-Client/issues)
  — ohne Versprechen, wie schnell, siehe README.
