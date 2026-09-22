# UwURDP — Konzept

> RDP-Client mit RDCMan-Dichte, Termius-Ruhe und einem Sync-Server, der dir gehört.

|                 |                                                                             |
| --------------- | --------------------------------------------------------------------------- |
| **Stand**       | 2026-09-22 · 0.1.0-beta.1                                                   |
| **Basis**       | Fork von UwUSSH: Tauri 2 + React + SQLite, RDP über IronRDP                 |
| **Bundle-ID**   | `app.uwurdp.desktop`                                                        |
| **Repo**        | [MinifyX/UwURDP-Client](https://github.com/MinifyX/UwURDP-Client) · GPL-3.0 |
| **Sync-Server** | [UwUSync-Server](https://github.com/MinifyX/UwUSync-Server), unverändert    |
| **Maskottchen** | Nyu, diesmal als Monitor auf einem Standfuß                                 |
| **Plattformen** | Windows zuerst, macOS und Linux aus der CI, später vielleicht mobil         |

---

## 1. Ziel

Ein RDP-Client, mit dem ich RDCMan nicht mehr öffnen muss.

Der Markt ist dreigeteilt:

- **mstsc** — solide, eingebaut, aber für genau einen Server gedacht.
- **Remote Desktop Connection Manager** — genau das richtige Arbeitsmodell
  (Gruppen, geerbte Anmeldungen, Vorschaubilder), aber UI von 2008, eine
  `.rdg`-Datei auf einem Rechner, Passwörter nur für ein Windows-Konto
  lesbar, und Microsoft pflegt es mit dem kleinstmöglichen Aufwand.
- **Royal TS, Devolutions & Co.** — teils gut, aber Konto, Abo oder Lizenz pro
  Platz, und der Sync läuft über fremde Server.

UwURDP besetzt die Lücke: **RDCMans Arbeitsweise, ein ruhiges Gesicht, Sync
auf dem eigenen Server — Zero-Knowledge verschlüsselt.**

**Versprechen in einem Satz:** _Deine Server, deine Passwörter, dein
Sync-Server._

Nebenbei: Spaßprojekt, fast komplett mit Claude geschrieben, kein Support,
keine Termine. Wer es nutzen oder forken will: gern, GPL-3.0.

### Nicht-Ziele

- Kein Alles-Client. SSH macht [UwUSSH](https://github.com/MinifyX/UwUSSH-Client),
  VNC/SPICE/Seriell machen andere.
- Kein Team-Produkt, keine geteilten Tresore in v1.
- Keine Telemetrie, keine Konten bei mir, kein Abo.

## 2. Zielgruppe

Admins und Homelabber mit 20–200 Windows-Servern, mehr als einem Rechner und
einer RDCMan-Datei, die über Jahre gewachsen ist. Also: ich.

## 3. Was es kann (0.1.0-beta.1)

**Verbinden**

- RDP mit TLS und NLA (CredSSP mit NTLM) auf IronRDP.
- Der Desktop folgt der Tab-Größe (DisplayControl), alternativ feste Größe
  oder Vollbild mit mstsc-ähnlicher Verbindungsleiste; zu große Desktops
  werden verkleinert (Smart Sizing) oder gescrollt.
- Tastatur nach physischer Position (PS/2-Scancodes aus `KeyboardEvent.code`),
  damit das Tastaturlayout des Servers gilt — wie bei mstsc. Unicode als
  Rückfall, Maus, Mausrad, beim Fokusverlust wird alles losgelassen.
- Strg+Alt+Ende = Strg+Alt+Entf, Strg+Alt+Pause = Vollbild, Strg+Alt+Pos1 =
  Tastatur zurück an die App, Strg+Alt+Bild↑/↓ = Tabs. Alles andere gehört im
  Desktop dem Server.
- Zwischenablage (Text) in beide Richtungen, Ton vom Server lokal abgespielt.

**Arbeiten wie in RDCMan**

- Bereiche Privat und Business, Gruppen per Drag & Drop.
- Jeder Host hat eine eigene Anmeldung oder erbt die seiner Gruppe
  (Rechtsklick → **Anmeldung der Gruppe…**).
- Jeder Desktop in einem eigenen Tab, auch mehrere zum selben Host.
- **Übersicht**: alle offenen Sitzungen als Kacheln mit ihrem Zustand, ohne
  Live-Bild, oder eine Gruppe inklusive nicht verbundener Hosts mit
  Verbinden-Knopf. **Alle verbinden** / **Alle trennen** pro Gruppe.
- Trennen behält den Tab mit dem letzten Bild, abgedunkelt. Bricht die
  Verbindung ab, verbindet UwURDP einmal selbst neu.

**Nicht (noch nicht)**: RD-Gateway, Konsolensitzung (`/admin`), Kerberos,
Laufwerks-/Drucker-/Smartcard-Umleitung, mehrere Monitore, Dateien über die
Zwischenablage, Hyper-V-Konsole. macOS und Linux baut die CI, von Hand
getestet sind sie noch nicht.

## 4. Architektur

```
┌─ WebView ─────────────────────────┐
│  Host-Baum, Tabs, Übersicht       │
│  <canvas> pro Desktop             │
└──────────────┬────────────────────┘
               │ Tauri IPC
               │  ↓ Befehle (connect, input, resize, ack)
               │  ↑ Channel (Binärnachrichten: Pixel, Zeiger, Ende)
┌──────────────┴────────────────────────────────────────┐
│  Rust                                                 │
│  SessionManager ──► IronRDP ──► RDP-Host              │
│  Vault   (Argon2id, XChaCha20-Poly1305, zeroize)      │
│  Sync    (HLC, Outbox, Manifeste)                     │
│  Store   (SQLite, WAL)                                │
└──────────────┬────────────────────────────────────────┘
               │ HTTPS (nur Chiffrat)
┌──────────────┴────────────────────────────────────────┐
│  UwUSync-Server (selbst gehostet, unverändert)        │
└───────────────────────────────────────────────────────┘
```

**Verbindungsaufbau** (`uwurdp-core`): TCP-Probe (ein Host, der nicht
antwortet, scheitert vor dem Anmeldedialog) → TCP → X.224 → TLS (rustls mit
_ring_) → **Zertifikatsprüfung** → CredSSP/NTLM → aktive Sitzung. Beworben
wird nur RemoteFX; Bulk-Kompression ist aus, weil IronRDP den Dekompressor
beim Resize verliert.

**Bildpfad.** Die Engine hält das Desktop-Bild, sammelt geänderte Rechtecke,
fasst sie zusammen und schickt RGBA-Rechtecke als Binärnachricht über einen
Tauri-Channel — höchstens alle 16 ms und höchstens zwei unquittiert. Die Seite
zeichnet mit `putImageData` und quittiert. **Pixel gehen nie verloren**: Solange
die Engine wartet, sammeln sich Änderungen weiter, die nächste Nachricht trägt
den aktuellen Stand. Eine langsame Seite bekommt gröbere Updates, keine
fehlenden. Die Lektion stammt aus UwUSSHs Terminal-Spike: ohne
End-to-End-Rückmeldung staut sich alles unbemerkt in der WebView.

**Harte Regel:** Passwörter aus dem Tresor verlassen Rust nie. Die WebView
bekommt Pixel und Metadaten; Anmeldung passiert komplett in Rust.

## 5. Datenmodell

UwUSSHs Schema V1–V6 plus **V7**:

| Tabelle       | Neu in V7                                      |
| ------------- | ---------------------------------------------- |
| `identities`  | `domain`                                       |
| `hosts`       | `rdp` (JSON), `comment`, `gateway_identity_id` |
| `host_groups` | `identity_id` — die Anmeldung der Gruppe       |

`RdpSettings` im Host: `display` (`fit` / `fixed` / `fullscreen`), `width`,
`height`, `smartSizing`, `colorDepth`, `audio` (`local` / `remote` / `off`),
`clipboard`, `admin`, `nla`, `wallpaper`, `gateway { address, port,
useHostLogin, bypassLocal }`. Alle Felder haben Defaults, Modi sind Strings
statt Enums, und **unbekannte Felder überleben den Round-Trip** — ein älterer
Build zerstört nichts, was ein neuerer geschrieben hat.

Lokal bleiben: Sync-Stand des Geräts, der fürs Gerät versiegelte Tresor-Key,
letzte Verbindung, App-Einstellungen.

## 6. Sync

Kein eigener Server. UwURDP spricht das Protokoll des
[UwUSync-Servers](https://github.com/MinifyX/UwUSync-Server) unverändert — der
Server sieht nur IDs, Sequenznummern und versiegelte Blobs und muss deshalb
nie wissen, was ein RDP-Host ist.

- **Eigenes Konto auf demselben Server**: `docker compose exec uwusync
uwusync-server invite` gibt einen neuen `uwu1_`-Einrichtungscode aus →
  Einstellungen → Sync → Server verbinden.
- **Zero-Knowledge**: Records werden vor dem Versand mit XChaCha20-Poly1305
  versiegelt. Der Tresor-Key ist mit Master-Passwort _und_ Account-Key
  umschlossen; der Account-Key steht nur auf gekoppelten Geräten und im
  Recovery-Kit.
- **Koppeln** per SPAKE2 mit kurzem Code (ID plus drei Wörter), das
  Master-Passwort wird auf jedem Gerät getippt und nie gesendet.
- **Manifeste**: Jedes Gerät veröffentlicht versiegelt, was es hat — ein
  Server, der Records zurückhält oder alte Versionen ausliefert, fällt auf.
- **Widerrufen** eines Geräts nur mit Master-Passwort.
- Synct: Hosts, Gruppen, Anmeldungen, Passwörter, vertraute Zertifikate.

## 7. Sicherheit

- **Zertifikate wie SSH-Host-Keys (TOFU).** RDP-Zertifikate sind fast immer
  selbst signiert; eine CA-Prüfung wäre entweder immer rot oder immer
  weggeklickt. Angezeigt wird der SHA-256 des Leaf-Zertifikats (`SHA256:…`)
  plus der SHA-1-Fingerabdruck, den Windows zeigt. Erster Kontakt fragt, eine
  Änderung blockiert mit einer nüchternen Warnung. Geprüft wird **bevor
  irgendein Credential gesendet wird**. Vertraute Zertifikate syncen
  (`known_hosts`, Algorithmus `x509`).
- **Tresor**: Master-Passwort → Argon2id → Master-Key → umschlossener
  Tresor-Key → AEAD pro Record. „Auf diesem Gerät merken“ über DPAPI
  (Windows), Keychain (macOS) oder Secret Service (Linux).
- **Kein Gateway ohne Zertifikatsprüfung.** `ironrdp-mstsgu` 0.0.1 prüft das
  Gateway-Zertifikat nicht und schickt das Passwort per Basic-Auth. Deshalb
  lieber „noch nicht unterstützt“ als ein Passwort an einen ungeprüften Server.
- **Sicherheit ist nie verspielt.** Keine Kaomoji, kein Nyu in Warnungen.
- WebView unter strikter CSP und mit eingefrorenem Prototyp; sie läuft nur
  UwURDPs eigenen Code.

## 8. Import

Priorität 1: Ein RDP-Client, der leer startet, wird wieder geschlossen.

- **RDCMan `.rdg`** 2.2 bis 2.93: verschachtelte Gruppen werden zu
  `Eltern / Kind`, Anmeldungen inklusive Credential-Profilen (File- und
  Local-Scope aus `RDCMan.settings`), Vererbung aufgelöst, Anzeige, Farbtiefe,
  Konsole, Ton, Zwischenablage, Gateway und Kommentare. Smart Groups werden
  übersprungen; Einstellungen ohne eigenes Feld landen im Kommentar des Hosts.
  Passwörter per DPAPI entschlüsselt — nur am selben Windows-Konto;
  zertifikatsverschlüsselte Dateien behalten Benutzer, verlieren Passwörter.
- Die Dateien, die RDCMan offen hatte, stehen **direkt in der Liste**.
- **mstsc `.rdp`**: UTF-16 und UTF-8, mehrere auf einmal, `password 51` per
  DPAPI.
- **Eigener Export `.uwurdp`**: JSON ohne Passwörter, sonst als Ganzes mit
  eigenem Passwort versiegelt (Argon2id + XChaCha20-Poly1305). Zertifikate nur
  für Hosts, die die Datei mitbringt.

Import-Daten haben eine eigene Form, die sich nicht mit Passwort
serialisieren lässt — die Vorschau in der WebView sieht nie ein Secret.

## 9. Roadmap-Überblick

| Meilenstein        | Stand                                                                                                                                                                                                                                                 |
| ------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **M0 · Fundament** | fertig — Fork, RDP-Engine, Bildpfad mit Flow-Control, Input, Schema V7, `dev_rdpd`                                                                                                                                                                    |
| **M1 · Alltag**    | erste Beta draußen — Import, Anmeldungen, Zertifikate, Tabs, Übersicht, Sync, Installer. Als Nächstes: RD-Gateway, Konsolensitzung, Kerberos, mehrere Monitore, Laufwerke, Dateien über die Zwischenablage, Smart Groups, Anzeige pro Gruppe, Hyper-V |
| **Später**         | mobil (Tauri 2 kann es, IronRDP läuft dort)                                                                                                                                                                                                           |

Details: [`docs/roadmap.md`](docs/roadmap.md).

## 10. Offene Fragen

1. **Gateway: warten oder selbst bauen?** Auf IronRDP warten ist sauberer,
   ein kleiner eigener Gateway-Client mit gepinntem Zertifikat schneller.
2. **Verschachtelte Gruppen.** Heute eine Ebene, RDCMan-Bäume werden
   flachgeklopft. Echte Verschachtelung hieße tiefere Vererbung — lohnt das?
3. **Ein Tresor für UwUSSH und UwURDP?** Windows-Server mit SSH und RDP
   hätten gern dieselbe Anmeldung. Dafür müssten zwei Apps ihre Records in
   einem Konto teilen.
4. **Kerberos ohne zweiten TLS-Stack** — eigener KDC-Client, oder warten, bis
   IronRDP ohne reqwest auskommt?
5. **Anzeige-Einstellungen pro Gruppe** wie in RDCMan, oder reicht pro Host
   plus Vorlage?
