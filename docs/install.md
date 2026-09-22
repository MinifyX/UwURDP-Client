# Installing UwURDP

[Deutsch weiter unten](#uwurdp-installieren)

UwURDP runs on **Windows 10 and 11** (x64 and ARM), **macOS 11 or newer**
(Apple silicon and Intel) and **Linux** (x86_64 and arm64). On Windows and
macOS the setup installs for your user only — no admin rights. UwURDP speaks
English or German, following the system; **Settings → Appearance → Language**
switches.

It is a first beta. I use it every day on Windows; the macOS and Linux builds
come out of CI and haven't been tried by hand yet, so expect more rough edges
there.

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
| Linux, portable            | `UwURDP-linux-x64-portable.tar.gz` · ARM: `…-arm64-portable.tar.gz` |
| Arch Linux                 | planned: an AUR package `uwurdp-bin`; until then the portable one   |

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
  `%LOCALAPPDATA%\Programs\UwURDP`) or turn off the desktop shortcut.
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
too.) The setup installs **UwURDP** into `/Applications`, or into
`~/Applications` if your user may not write to `/Applications`. The installed
app starts without that question.

To uninstall, run the setup again and choose **Uninstall …** — it asks whether
to keep your hosts and vault. Dragging the app to the Trash works too, but
leaves the data in `~/Library/Application Support/app.uwurdp.desktop`.

## Linux

**Ubuntu, Debian and relatives:** `sudo apt install ./UwURDP-linux-x64.deb`
(`…-arm64.deb` on ARM). **Fedora, openSUSE:**
`sudo dnf install ./UwURDP-linux-x64.rpm` or
`sudo zypper install ./UwURDP-linux-x64.rpm`. Both install the app
system-wide as package `uwurdp`, with a menu entry, using the system's
WebKitGTK 4.1 and ALSA (`libasound2`) for sound, and update themselves: UwURDP
downloads the next package and installs it on **Restart now**, asking for the
administrator password. Uninstall with `sudo apt remove uwurdp` or
`sudo dnf remove uwurdp`.

**Portable:** unpack `UwURDP-linux-x64-portable.tar.gz` anywhere and start
`./UwURDP/uwurdp`. It brings its own WebKit, installs nothing and doesn't
update itself — fetch the newest one to update. Sound needs the system's ALSA
library (`libasound2`, on Arch `alsa-lib`), which nearly every desktop has.

**Arch Linux:** an AUR package `uwurdp-bin` is planned. Until it exists, the
portable folder works.

**Remembering the vault on Linux** uses the Secret Service (GNOME Keyring,
KWallet). Without one — a bare window manager — UwURDP keeps its key in a file
only your user can read, which protects less against someone with your disk.

## First steps

- **Bring your servers along**: the import button in the sidebar reads
  RDCMan `.rdg` files — the ones RDCMan had open are listed right away — and
  mstsc `.rdp` files. Saved passwords come along when you're on the same
  Windows account that saved them.
- **Or add a host** with `+`: address, and a login of its own or none, so it
  takes its group's.
- **A login for a whole group**: right-click the group → **Group login…**.
  Every host in it without its own login uses that one.
- **Passwords** go into an encrypted vault. The first time you save one, you
  pick a master password. Tick "remember on this device" if your account
  should open the vault on its own.
- On first contact with a server you are shown its certificate's fingerprint,
  and the thumbprint Windows shows for it. Trust it only if it is the one you
  expect.
- A click on a host opens its desktop in a tab — or shows the tab that is
  already open. Right-click the host for **Open another tab**.
- Inside a desktop the keyboard belongs to the server. **Ctrl+Alt+End** sends
  Ctrl+Alt+Del, **Ctrl+Alt+Break** toggles full screen, **Ctrl+Alt+Home** takes
  the keyboard back, **Ctrl+Alt+PgUp/PgDn** switch tabs. The rest are under
  Settings → Sessions.
- **Ctrl+Shift+O** opens the overview: every open desktop as a live thumbnail.
- **Several computers?** Settings → Sync connects a
  [UwUSSH server](https://github.com/MinifyX/UwUSSH-Server) of your own and
  keeps hosts, logins, passwords and trusted certificates the same everywhere,
  end-to-end encrypted. Already running one for UwUSSH? Give UwURDP its own
  account: `docker compose exec uwussh uwussh-server invite` prints a new
  setup code.

## Updates

UwURDP updates itself: about 20 seconds after it starts, and every six hours,
it looks for a newer version, downloads it quietly (signed and checked) and
offers a restart. **Settings → Updates** switches between the Beta and Stable
channels. Stable only gets versions without a beta mark; as long as there are
only betas, stay on Beta.

A newer setup can also simply be run over an installed UwURDP, a newer package
installed over the old one. Hosts, the vault and settings stay. The portable
folder doesn't update itself.

## Where your data lives

| What                                   | Windows                              | macOS                                              | Linux                               |
| -------------------------------------- | ------------------------------------ | -------------------------------------------------- | ----------------------------------- |
| Hosts, groups, certificates, the vault | `%APPDATA%\app.uwurdp.desktop\`      | `~/Library/Application Support/app.uwurdp.desktop` | `~/.local/share/app.uwurdp.desktop` |
| App settings (look, sessions, …)       | `%LOCALAPPDATA%\app.uwurdp.desktop\` | `~/Library/WebKit/app.uwurdp.desktop`              | `~/.local/share/app.uwurdp.desktop` |
| The program                            | `%LOCALAPPDATA%\Programs\UwURDP\`    | `/Applications/UwURDP.app`                         | `/usr/bin/uwurdp-desktop`           |

The database is `uwurdp.db` in the first folder. Passwords are only stored
encrypted. To move to another computer without a sync server: **Settings →
Import & Export** writes everything into one `.uwurdp` file, sealed with a
password of its own when it carries passwords, which UwURDP on the other
computer reads back in.

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
- **"RD Gateway is not supported yet" when connecting**: the host goes
  through an RD Gateway, which UwURDP can't do yet. The gateway settings are kept for when
  it can.
- **The imported passwords are missing**: RDCMan and mstsc encrypt them for
  one Windows account. Import on that account, or type them once — they stay
  in the vault after that.
- **The login is rejected on a domain that has NTLM switched off**: UwURDP
  only speaks NTLM so far, not Kerberos.
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

Es ist eine erste Beta. Ich nutze sie jeden Tag unter Windows; die Versionen
für macOS und Linux baut die CI, von Hand ausprobiert hat sie noch niemand —
dort also mit mehr Ecken und Kanten rechnen.

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
| Linux, portabel            | `UwURDP-linux-x64-portable.tar.gz` · ARM: `…-arm64-portable.tar.gz` |
| Arch Linux                 | geplant: ein AUR-Paket `uwurdp-bin`; bis dahin die portable Version |

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
  `%LOCALAPPDATA%\Programs\UwURDP`) oder schaltest die Desktop-Verknüpfung ab.
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
Setup installiert **UwURDP** nach `/Applications`, oder nach `~/Applications`,
wenn dein Benutzer nicht in `/Applications` schreiben darf. Die installierte
App startet ohne diese Rückfrage.

Deinstallieren: das Setup noch einmal starten und **Deinstallieren …** wählen —
es fragt, ob Hosts und Tresor bleiben sollen. Die App in den Papierkorb ziehen
geht auch, lässt aber die Daten in
`~/Library/Application Support/app.uwurdp.desktop` liegen.

## Linux

**Ubuntu, Debian und Verwandte:** `sudo apt install ./UwURDP-linux-x64.deb`
(`…-arm64.deb` auf ARM). **Fedora, openSUSE:**
`sudo dnf install ./UwURDP-linux-x64.rpm` oder
`sudo zypper install ./UwURDP-linux-x64.rpm`. Beide installieren die App
systemweit als Paket `uwurdp`, mit Eintrag im Anwendungsmenü, nutzen das
WebKitGTK 4.1 des Systems und ALSA (`libasound2`) für den Ton, und
aktualisieren sich selbst: UwURDP lädt das nächste Paket und installiert es bei
**Jetzt neu starten**, nach Eingabe des Administrator-Passworts.
Deinstallieren mit `sudo apt remove uwurdp` bzw. `sudo dnf remove uwurdp`.

**Portabel:** `UwURDP-linux-x64-portable.tar.gz` irgendwo entpacken und
`./UwURDP/uwurdp` starten. Bringt sein eigenes WebKit mit, installiert nichts
und aktualisiert sich nicht — zum Aktualisieren die neueste holen. Für den Ton
braucht es die ALSA-Bibliothek des Systems (`libasound2`, unter Arch
`alsa-lib`), die fast jeder Desktop hat.

**Arch Linux:** Ein AUR-Paket `uwurdp-bin` ist geplant. Bis es das gibt, geht
die portable Version.

**Tresor merken unter Linux** nutzt den Secret Service (GNOME Keyring, KWallet).
Ohne einen — etwa unter einem reinen Fenstermanager — legt UwURDP seinen
Schlüssel in eine Datei, die nur dein Benutzer lesen kann; das schützt weniger
gegen jemanden mit deiner Festplatte.

## Erste Schritte

- **Server mitbringen**: Der Import-Knopf in der Seitenleiste liest
  RDCMan-`.rdg`-Dateien — die, die RDCMan offen hatte, stehen gleich da — und
  mstsc-`.rdp`-Dateien. Gespeicherte Passwörter kommen mit, wenn du am selben
  Windows-Konto sitzt, das sie gespeichert hat.
- **Oder Host anlegen** mit `+`: Adresse, dazu eine eigene Anmeldung oder keine
  — dann gilt die der Gruppe.
- **Eine Anmeldung für die ganze Gruppe**: Rechtsklick auf die Gruppe →
  **Anmeldung der Gruppe…**. Jeder Host darin ohne eigene Anmeldung nutzt
  diese.
- **Passwörter** landen in einem verschlüsselten Tresor. Beim ersten Speichern
  legst du ein Master-Passwort fest. Mit „Auf diesem Gerät merken“ öffnet dein
  Benutzerkonto den Tresor von selbst.
- Beim ersten Kontakt mit einem Server siehst du den Fingerprint seines
  Zertifikats und den Fingerabdruck, den Windows dafür anzeigt. Vertrau ihm nur,
  wenn es der erwartete ist.
- Ein Klick auf einen Host öffnet seinen Desktop in einem Tab — oder zeigt den
  Tab, der schon offen ist. Rechtsklick auf den Host → **Weiteren Tab öffnen**.
- Im Desktop gehört die Tastatur dem Server. **Strg+Alt+Ende** schickt
  Strg+Alt+Entf, **Strg+Alt+Pause** schaltet Vollbild um, **Strg+Alt+Pos1**
  holt die Tastatur zurück, **Strg+Alt+Bild↑/Bild↓** wechselt den Tab. Die
  übrigen stehen unter Einstellungen → Sitzungen.
- **Strg+Umschalt+O** öffnet die Übersicht: jeder offene Desktop als
  Live-Vorschau.
- **Mehrere Rechner?** Einstellungen → Sync verbindet einen eigenen
  [UwUSSH-Server](https://github.com/MinifyX/UwUSSH-Server) und hält Hosts,
  Anmeldungen, Passwörter und vertraute Zertifikate überall gleich,
  Ende-zu-Ende-verschlüsselt. Läuft schon einer für UwUSSH? Gib UwURDP ein
  eigenes Konto: `docker compose exec uwussh uwussh-server invite` gibt einen
  neuen Einrichtungscode aus.

## Updates

UwURDP aktualisiert sich selbst: etwa 20 Sekunden nach dem Start und danach
alle sechs Stunden sucht es nach einer neuen Version, lädt sie still herunter
(signiert und geprüft) und bietet einen Neustart an. **Einstellungen → Updates**
wechselt zwischen den Kanälen Beta und Stabil. Stabil bekommt nur Versionen
ohne Beta-Markierung; solange es nur Betas gibt, bleib bei Beta.

Ein neueres Setup kann auch einfach über ein installiertes UwURDP laufen, ein
neueres Paket über das alte installiert werden. Hosts, Tresor und Einstellungen
bleiben. Der portable Ordner aktualisiert sich nicht selbst.

## Wo deine Daten liegen

| Was                                     | Windows                              | macOS                                              | Linux                               |
| --------------------------------------- | ------------------------------------ | -------------------------------------------------- | ----------------------------------- |
| Hosts, Gruppen, Zertifikate, Tresor     | `%APPDATA%\app.uwurdp.desktop\`      | `~/Library/Application Support/app.uwurdp.desktop` | `~/.local/share/app.uwurdp.desktop` |
| App-Einstellungen (Aussehen, Sitzungen) | `%LOCALAPPDATA%\app.uwurdp.desktop\` | `~/Library/WebKit/app.uwurdp.desktop`              | `~/.local/share/app.uwurdp.desktop` |
| Das Programm                            | `%LOCALAPPDATA%\Programs\UwURDP\`    | `/Applications/UwURDP.app`                         | `/usr/bin/uwurdp-desktop`           |

Die Datenbank ist `uwurdp.db` im ersten Ordner. Passwörter werden nur
verschlüsselt gespeichert. Für einen Umzug ohne Sync-Server: **Einstellungen →
Import & Export** schreibt alles in eine `.uwurdp`-Datei, mit einem eigenen
Passwort versiegelt, wenn Passwörter drin sind, die UwURDP auf dem anderen
Rechner wieder einliest.

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
- **„RD Gateway is not supported yet“ beim Verbinden**: Der Host geht über ein
  RD-Gateway, und das kann UwURDP noch nicht. Die Gateway-Einstellungen bleiben
  für später gespeichert.
- **Die importierten Passwörter fehlen**: RDCMan und mstsc verschlüsseln sie
  für ein Windows-Konto. Am selben Konto importieren, oder sie einmal eintippen
  — danach liegen sie im Tresor.
- **Die Anmeldung wird in einer Domäne abgelehnt, in der NTLM abgeschaltet
  ist**: UwURDP spricht bisher nur NTLM, kein Kerberos.
- Etwas anderes? [Issue aufmachen](https://github.com/MinifyX/UwURDP-Client/issues)
  — ohne Versprechen, wie schnell, siehe README.
