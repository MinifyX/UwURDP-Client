# Architecture

How the pieces fit together. The German [KONZEPT.md](../KONZEPT.md) has the
long version.

## The stack

| Layer       | Choice                                  | Why                                                                                              |
| ----------- | --------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Shell       | Tauri 2                                 | Same as UwUMail. ~12 MB instead of Electron's 150, WebView2 on Windows, mobile support built in. |
| UI          | React + TypeScript, Node 24, pnpm 11    | UwUMail's stack, so tokens, components and Nyu carry over.                                       |
| Terminal    | `xterm.js` + WebGL addon                | What VS Code and Termius use. Canvas fallback.                                                   |
| SSH         | `russh`, `russh-sftp`                   | Pure Rust, async, no libssh2 FFI pain. Reads OpenSSH, PEM and PuTTY `.ppk` keys itself.          |
| Local shell | `portable-pty`                          | ConPTY on Windows, PTY elsewhere.                                                                |
| Store       | `rusqlite` with WAL                     | One file, offline-first, trivial to back up.                                                     |
| Crypto      | `argon2`, `chacha20poly1305`, `zeroize` | Established RustCrypto crates. Nothing home-made.                                                |
| Server      | Rust + `axum`, SQLite                   | One language across the stack, one Docker image.                                                 |

## The secret boundary

```
┌─ WebView ─────────────┐ │ ┌─ Rust Core ───────────────────────┐
│ Host tree, tabs       │ │ │ SessionManager → russh → SSH host  │
│ Inspector, SFTP       │ │ │ VaultService   → keys, passwords   │
│ xterm.js (WebGL)      │ │ │ SyncEngine     → outbox, merging   │
│                       │ │ │ Store          → SQLite            │
│ no access to secrets  │ │ │                                    │
└───────────────────────┘ │ └────────────────────────────────────┘
        ↓ connect, resize (JSON command)
        ↑ PTY bytes (raw channel, batched every 8 ms)
```

Private keys and passwords from the vault never cross that line. Authentication
happens entirely in Rust, and the WebView gets terminal bytes and metadata. What
does cross goes the other way: a password you type into a dialog, on its way to
Rust. The one private key the page ever sees is one UwUKeygen has just
generated, and only when you ask to see or copy it; a key already in the vault
leaves only into a file, written by Rust.
That boundary keeps secrets out of the page's memory; it does not make the page
untrusted. The page can open a local shell and type into it, so it sits inside
the trust boundary, and what protects it is that it only ever runs UwURDP's own
code under a strict content security policy. See the
[security review](security-review-2026-09.md#the-trust-boundary).

## Terminal throughput — measured, settled

This was the one risk that could have invalidated the design: terminal output
crossing the IPC boundary. It has been measured, and the answer is in
[the M0 spike](m0-spike.md). Short version: **the IPC channel stays, the
WebSocket fallback is gone, and end-to-end flow control is mandatory.**

The data path, as built:

```
source (SSH socket, PTY reader, synthetic)
  → bounded queue          backpressure to the source when full
  → batcher                8 ms frames, 256 KiB cap, pauses on the renderer
  → tauri::ipc::Channel    raw bytes, no JSON
  → xterm.js write()       acknowledges parsed bytes back to the batcher
```

With flow control, 64 MiB of coloured log output reaches the screen at
41–46 MiB/s with no UI frame above 7 ms. That rate is xterm.js' own parse
speed — the channel delivers faster — so no transport change could raise it.
Local shells on Windows are capped far lower by ConPTY (~1.7 MiB/s), which SSH
sessions never go through.

### Why flow control is not optional

`Channel::send` returns once Tauri has queued a frame, not once the webview has
parsed it. Without acknowledgements, backpressure stops at the IPC boundary, the
webview queues everything — and xterm.js throws away writes once more than
50 MB are pending. Measured: about 9 MiB lost from a 64 MiB flood, twice, with a
perfectly smooth UI that gave no hint of it.

So the renderer acknowledges from xterm.js' write callback, the batcher pauses
at 512 KiB outstanding and resumes at 128 KiB, and acknowledgements go out every
64 KiB. That last size must stay below the resume threshold, or a paused stream
deadlocks on an unacknowledged remainder; `uwurdp-core` asserts it at compile
time. It is the scheme VS Code's terminal uses, for the same reason.

### Bytes are never dropped

An early draft of the concept said frames would be dropped on overflow. That was
wrong: dropping bytes before xterm.js cuts escape sequences in half. The engine
never drops; it only ever makes the source wait.

## Data model

Every syncable entity shares one header:

```
id          UUIDv7   time-sortable, no coordination needed
updated_at  HLC      hybrid logical clock (wall_ms, counter, device)
deleted     bool     tombstone, 90 day TTL
vault_id    UUID     which vault this belongs to
server_seq  u64      the version the server last confirmed, 0 for never
dirty       bool     changed here and not pushed yet
sync_extra  JSON     fields a newer build wrote that this one does not know
rev         u64      local bookkeeping, handy in tests and logs
```

Entities: `Host`, `Group`, `Identity`, `Key`, `Secret`, `Snippet`,
`PortForward`, `KnownHost`, `TerminalProfile` — the last two of those have no
table yet. `SessionLog` stays local and does not sync by default, and so do the
columns that only mean something here: a key's file path, when this device last
connected, and the system a connection found.

Hosts also carry a **workspace** (`private` or `business`) and a position.
Workspaces are a view — two lists in one sidebar, like UwUMail's accounts — not
separate vaults; everything shares one vault and one database. Groups are
records of their own (`host_groups`: workspace, name, position), so an empty
group survives and groups keep the order they were dragged into, and a host
points at its group **by id**. That last part is what keeps renaming a group one
record instead of one per host — which, on two devices at once, would have been
one conflict per host. Names stay the handle the interface uses; they are
resolved to ids on the way in.

`Host.jump_host_id` is a self-reference, which gives ProxyJump chains for free:
`laptop → bastion → db-01` is a linked list the SessionManager resolves
recursively, with a depth limit of 8 and cycle detection.

## Vault

```
Master password
   └─ Argon2id (m=64 MiB, t=3, p=4, per-vault salt)
        ├─ bytes  0..32  →  master key        never leaves the device
        └─ bytes 32..64  →  auth secret → HKDF → server login hash

Vault key (32 random bytes)
   └─ wrapped with the master key → wrapped_vault_key
```

Two consequences justify this shape: changing the password only rewraps one key
instead of re-encrypting every record, and the server only ever learns the login
hash. It is the Bitwarden model, which is well understood and has been looked at
by many more eyes than mine.

Records are encrypted with `XChaCha20-Poly1305`, a fresh nonce each, and
`record_id || entity_type || vault_id` as associated data — so a malicious
server can neither swap blobs between records nor reinterpret their type.

### Opening with the Windows account

Typing the master password at every start is what people turn vaults off for.
So the vault can be remembered on this device: the vault key is sealed with
DPAPI (`CryptProtectData` for the signed-in user, with UwURDP's own entropy)
and kept in `device_unlock`, next to a check value sealed with the vault key.
At start the app unseals it, opens the vault with it and verifies the check; a
key that no longer fits — another vault, a restored database — is dropped
quietly and the app asks as before.

This is a trade-off, made on purpose and switchable in Settings → Vault & Keys:
anything that runs as the same Windows user can ask DPAPI for the same key, so
with remembering on, the vault protects against a copied database and another
account, not against malware already running as you. That matches the trust
boundary the app already has — such malware could type into your local shell —
and it is one checkbox — "remember on this device", ticked by default in the vault dialog, off again in Settings with one click.

macOS and Linux have no DPAPI. There a random key of UwURDP's own lives in the
Keychain or the Secret Service (GNOME Keyring, KWallet), made on first use and
read back once to be sure it was kept, and seals with XChaCha20-Poly1305. Where
no Secret Service answers, the key falls back to a file only this user can
read, and the install guide says what that costs. Every seal carries a label
for its purpose — the remembered vault key, or what a paired device keeps — so
a blob made for one never opens as the other (see `device.rs`).

### Secrets per host

A host keeps a password or a vault key of its own. Identities (user, key,
password) are separate records, and an import lets several hosts share one, as
Termius does. Changing one host's login therefore copies a shared identity
first, and a password or identity goes away only when no host uses it any more
— so saving a password for one imported host never hands it to another. Saving the host form with a
password seals it on the way (`PasswordChange`: keep, set, forget), a password
typed into the connect dialog can be kept with one checkbox once the login
succeeded, and forgetting overwrites the sealed blob and leaves a tombstone.
Keys are vault records with a label and a public key that is readable while
the vault is locked, so a host form can show which key it uses without
unlocking anything.

## Sync

Offline-first: everything lands in SQLite first, the server is a relay.

### The envelope

A record travels as an envelope — id, kind, vault, clock, tombstone flag — plus
a payload sealed with the vault key. The server sees only the header, and
**the header is sealed along with the payload** (associated data:
`label ‖ id ‖ kind ‖ vault_id ‖ hlc ‖ deleted`).

That is not a detail. Without it a server could set `deleted`, and since a
tombstone beats a concurrent edit, one flipped bit would remove a host from
every device; or it could rewrite the clock and make an old version look like
the newest one. Both now fail the authentication tag. A tombstone therefore
seals an empty payload rather than nothing at all.

### The outbox is the database

A local write sets `dirty = 1` in the same transaction that writes the row, and
`server_seq` records the version the server confirmed. A queue beside the
database would lose edits in a crash; a column cannot. Finding what to push is a
partial index over exactly the waiting rows.

`sync_extra` is the other half of that: fields a **newer** build wrote are kept
verbatim, so editing a host on an older device does not quietly drop what it has
never heard of.

### One pass

Push first, then pull — and the pull is not optional even when nothing comes
back, because the cursor only moves on a pull. A pass that ended with a push
would leave the device believing it had not seen its own writes. Pulling last
also brings those writes back once, checked against what is here, which is a
cheap way to notice a server that stored something else.

A conflict therefore shows up on the way out: the server refuses the record and
returns the version it holds, the client merges that and starts the round again
— at most three times, then a banner.

### Conflicts

Last-writer-wins **per record**, decided by the HLC, and a **delete beats a
concurrent edit**: a host you removed coming back — a bastion you
decommissioned, say — is worse than redoing a rename.

Per record rather than per field, because two devices practically never edit the
same host at the same moment, and a field-wise merge needs a common ancestor per
field, which costs a second copy of every record for a case that does not
happen. Snippet bodies, where text really can collide, are the one place that
may earn a three-way merge later.

The rule itself lives in `uwurdp-proto`, next to the clock, because the store
applies it inside the transaction that writes the record and the engine decides
what to push — and the server must never disagree with either.

Two cases have a rule of their own:

- **Trusted host keys** share one slot per `address:port`. If two devices
  trusted different keys without seeing each other's, the newer record takes
  the slot, on every device, so they end up agreeing; a differing fingerprint is
  counted so the interface can say so.
- **A key referenced by file path** stays local. A path means nothing on
  another machine.

### Records that arrive out of order

A host can arrive before the login it points at, and foreign keys would refuse
it. So a missing reference gets a **placeholder**: a live row with the oldest
possible clock, no name, never pushed — which the real record replaces the
moment it turns up. Until then the host is listed without a login, and a group
with no name yet is not shown as a group at all.

### Manifests: noticing what the server keeps back

The seal stops a server from forging or altering a record, but not from
**leaving one out**: it could hand a newly joined device the old version of a
host key that was replaced after a reinstall, never mention the new one, and
the device would trust the old key — a man in the middle waiting for the first
connection. Nothing in a single record says there should have been more.

So every device publishes one **manifest** (`EntityKind::Manifest`, sealed like
any record) under an id derived from the vault and the device's clock id — a
version-8 UUID from SHA-256, so each device has exactly one and it can be named
before it arrives. It lists id, version and tombstone flag of every record the
device holds **that the server has confirmed** (`dirty = 0`, a `server_seq`, no
placeholder), in a compact binary form of 34 bytes per record. A pass that got
everything out and pulled to the end rewrites it when that list changed and
pushes it **on its own**, after the records it lists; a server that does not
know the kind yet refuses that request alone, and the records still sync.

After a pull that reached the end, every manifest is held against what is here:
each listed version must be here or something newer — a later edit, or a
tombstone, which beats every edit anyway. A listed delete of something never
had is fine, and so is a host key that lost its `address:port` slot to a newer
one (it is recorded in `superseded`, since it leaves no tombstone). Kinds this
build keeps no table for are not checked. Before the alarm goes up the first
time, everything is pulled once more from cursor 0, so only what is still
missing then counts. What is found is written down (`manifest_violations`),
shown in Settings → Sync and once as a banner, and while it involves host keys
**a host key another device decided on is not trusted**: `known_host` answers
as if the host were new, the next connection asks, and a key the user accepts
there is this device's own and trusted again. Manifests themselves go through
the usual conflict rule, so a server cannot roll an existing device back to an
older manifest either.

A device that joins gets a **floor** inside the pairing handshake, which the
server cannot touch: the id and version of the manifest the offering device had
last published. Until that manifest, at least that new, is here, the check
fails — so a server cannot hide the one manifest that would give it away.

The limits, honestly:

- A device with **no floor and no earlier state** — joined by hand, or before
  the offering device ever published — can be rolled back **consistently**: a
  server that serves an old snapshot of every record and every manifest
  together is indistinguishable from a vault that simply is that old.
- A manifest lists at most 7,000 records, host keys first, then keys, secrets,
  logins, hosts, groups and snippets; a bigger vault publishes a partial one
  and the rest goes unchecked.
- The check covers versions the listing device had confirmed. An edit made
  after its last manifest is covered once that device publishes again.
- Manifests of devices that are gone stay on the server. They are harmless as
  long as the server keeps tombstones; once it forgets a tombstone, a new
  device would miss a record that stale manifest still lists — which is why a
  server that knows manifests drops a device's manifest when it is revoked.

### Joining an account

A second device that already has hosts of its own has its own vault id and its
own vault key. `adopt_vault` takes over the account's: every secret is opened
with the old key and sealed again with the new one — the ids stay, but a sealed
record is bound to its vault — the vault id is rewritten everywhere, the key
this device had sealed for itself is dropped, and everything is marked waiting,
because to the account it is all new. What was already there then goes through
the same duplicate detection as an import.

### Settings → Sync

The interface is `sync.rs` in the app, a thin layer over `uwurdp_sync::flow`,
which puts the steps in their order once: **Connect a server** takes the
setup code the server prints, the master password (checked against the vault
that is here, or a new one) and a device name, makes the account and shows the
recovery kit — the account key in its printed form, with the server and its
fingerprint — closable only after "I've written it down". The vault here is
wrapped under the account key only once the server has taken the account and
this device has kept the key: a refusal leaves it as it was. (Up to
0.1.0-beta.8 it was wrapped first, and a refused connect left a vault needing
a key nobody had; the vault dialog now offers a device that still opens such a
vault on its own a new master password.) **Recovery kit** shows the kit again
on any paired device, for the master password — every one of them keeps the
key anyway, and a kit seen once is easily lost.
**Pair with a device** takes the long code another device shows (`uwu2_…`, which
carries the address and fingerprint) or the short one read out loud plus the
address; a vault already on this device is opened first, because its secrets
move into the account's. **Add a device** shows both codes with a ten-minute
countdown and waits; **Revoke** needs the master password; **Disconnect**
re-wraps the vault without the account key once the password opened it, so the
password alone opens it again, and tells the server this device left.

A worker thread, not the terminals' runtime, runs the passes: every five
seconds it looks whether something waits to be pushed, pulls once a minute, and
at once when the page asks. It runs only while the vault is open, drops its
connection after any failure (the next pass connects afresh) and backs off for
half a minute. A pass that brought changes tells the page, which loads the host
list again. Unlocking a synced vault takes the account key the device kept; a
device that lost it asks for the recovery kit's code next to the password.

The transport is careful about a server that lies: addresses are parsed the
way reqwest parses them (plain HTTP only to a loopback host), redirects are
not followed, answers are read up to a limit, a pass pulls at most a thousand
pages, and a failed pairing always closes its session. Phase E of the
end-to-end run drives two app instances against a real server (see below).

### The protocol

| Endpoint                             | Purpose                                                                     |
| ------------------------------------ | --------------------------------------------------------------------------- |
| `POST /v1/accounts`                  | Create an account from an invite: vault header, auth verifier, first device |
| `POST /v1/session`                   | A device signs a challenge (Ed25519) → token, one hour                      |
| `GET /v1/vault`, `PUT /v1/vault/key` | The vault header; a new one on a password change                            |
| `GET /v1/records?since=<seq>`        | Envelopes newer than a cursor, paginated; `&manifests=1` includes manifests |
| `POST /v1/records`                   | Batch push, each record with its `base_seq`                                 |
| `GET /v1/events`                     | Server-sent events: "changes from seq N"                                    |
| `POST /v1/pair`, `/v1/pair/{id}`     | Relay for device pairing (SPAKE2), ten minutes                              |
| `GET /v1/devices`, `POST …/revoke`   | List and revoke devices; another one only with the master password          |

The cursor is a monotonic server sequence number, not a timestamp: clocks across
devices are a bug source. That number is also a record's version — a push
carries the `base_seq` it was based on, and a mismatch is the conflict. A
device-local counter would be wrong here, since two devices count on their own.

Server-sent events rather than a WebSocket: "there is something new from N"
needs no channel back, and SSE survives every reverse proxy.

## The sync server

One binary, one Docker image, one SQLite file — and deliberately dumb. It hands
out sequence numbers, keeps the newest version of each record, pages through
them from a cursor, and refuses a write whose `base_seq` is not the version it
holds. It cannot read a record, so it cannot merge one either; everything clever
happens on the devices.

```
accounts (id, vault_id, kdf_*, salt, wrapped_key, auth_verifier, created_ms)
devices  (id, account_id, name, public_key, cursor, last_seen_ms, revoked_ms)
records  (account_id, id, kind, seq, hlc, deleted, nonce, blob)
invites  (code_hash, expires_ms, used_ms)
```

What it does enforce is size and rate, which needs no key: 500 records per
request, 256 KiB per envelope, a 24-byte nonce, the schema version, and limits
on account creation, login and pairing. A record too large for that is left out
of a push rather than offered and refused, so one oversized snippet cannot stop
everything else from syncing.

`UWURDP_TLS=auto` generates a certificate on first start and logs its
fingerprint; the app pins it, which is the model an SSH client uses anyway, and
pairing passes the fingerprint to the joining device inside the SPAKE2 channel.
No domain, no Let's Encrypt, works over a Tailscale address. A reverse proxy
with a real certificate is still an option; plain HTTP is not, except against
localhost.

Backups are `VACUUM INTO`, not a copy of a live WAL database, plus a nightly
snapshot with fourteen kept — cheap insurance against a client bug that pushes
nonsense to every device.

### The account key, and how a device joins

```
master password ── Argon2id(salt, params) ──┐
account key (128 bits, on paired devices)  ┴─ HKDF
     ├─ master key   never leaves the device  → wraps the vault key
     └─ auth secret  → the server stores only its SHA-256
```

The account key is the part the server never sees, and it is what makes a stolen
server database worthless: without it, whoever took the server could guess the
master password offline against the wrapped vault key it has to store. The cost
is stated plainly in the setup: losing every device **and** the recovery kit
means losing the data.

The key is 128 bits, and it is mixed in _after_ Argon2id through HKDF rather
than into Argon2's salt. That is what makes turning sync on cheap for a vault
that already has hosts: the salt, the parameters and every record stay as they
are, and one 32-byte key is wrapped again. Changing the master password is the
same operation, so there is one function for both.

On paper the key is 28 characters in four groups, from an alphabet with no
letter that can be read as a digit, with two characters of checksum. A mistyped
kit therefore says "typo" rather than "wrong password" — which is the
difference between looking in the right place and the wrong one. A vault that
needs the key says so in its header, and the server passes that on to a joining
device, for the same reason.

Pairing follows Magic Wormhole rather than a QR code, because desktops rarely
have cameras. The device that is already in shows `K7M4Q-tiger-radio-kiwi`: the
server's session id and three of 128 words — exactly 128, so one random byte
picks one without favouring any. A word that is not on the list means the code
was misheard rather than mistyped, and those are worth telling apart. Next to
it sits one pasteable string carrying the address and the fingerprint too, for
when the two devices can copy and paste at each other.

Then SPAKE2 over the server's relay gives the two a key the server cannot
derive, and an attacker exactly one guess per code. **The order is what makes
that guess worthless**: messages, then the joining device proves it derived the
same key, and only then does the other one hand over the certificate
fingerprint, the account key and a one-time token. Handing the secret over
first and asking afterwards would give it to that one guess. Last, the joining
device says what it is called, so the other can show a name instead of "a
device".

The server hands over the wrapped vault key only once the new device has proved
it knows the master password as well. So an intercepted code is worth nothing
without the password, and the password is worth nothing without a device that
approved the join.

`uwurdp-sync`'s `flow` module is where these steps are put in order, once —
connect a server, offer a pairing, join from one — so the interface above has
three calls and no chance to get the order wrong.

## Import

Every importer is an adapter that writes into one neutral `ImportedHost`
intermediate form. Everything after that — preview with checkboxes, duplicate
detection on `address:port` and user, group assignment, then the write — is
shared. A new source costs one adapter, not a new pipeline. Identities and keys
are written lazily, only when a host that is actually imported uses them, so
importing Termius a second time adds nothing and asks for no vault.

`.ppk` files from PuTTY and KiTTY need no conversion: russh reads PuTTY key files
natively, encrypted or not, so a session imported from PuTTY logs in with its
own key file as it is. The importers live in `crates/uwurdp-import`, testable
against real registry dumps and sample configs without launching the app.

### Termius, which has no export

Termius removed its export, so there is no file to ask the user for. What there
is instead is Termius' own Electron IndexedDB on disk, with every sensitive
field sealed under a local key the OS keychain holds
(`Termius/localKey` in the Windows Credential Manager). The same user on the
same machine can read both — no Termius account, no password, no plain-text
export file left in the downloads folder.

Reading it is three layers, each a file-format reader rather than an
app-specific scraper, and each tested against files built by a matching writer:

1. **`chromium::leveldb`** reads every table and log file in the database
   directory and keeps the newest write per key. That sidesteps the custom
   comparator IndexedDB registers (which makes a stock LevelDB refuse the
   database) and copes with Termius running at the same time — a torn log tail
   fails its CRC and is dropped, a half-written table has no footer and is
   skipped.
2. **`chromium::idb`** decodes Chromium's key prefixes (database, store and
   index ids), the database and store names, and Blink's value envelope,
   Snappy-compressed values included.
3. **`chromium::v8`** deserializes V8's structured-clone format — the objects,
   arrays, strings, numbers and byte buffers a record is made of — with limits
   against hostile nesting and lengths.

On top of that, `termius` opens the sealed fields (XSalsa20-Poly1305, libsodium's
secretbox) with the local key, and maps Termius' entities into the neutral
bundle: hosts with the port and login they inherit from their group chain,
logins and keys shared by index, the host keys Termius already trusts, snippets.
Records marked deleted stay deleted; records sealed with a key this device does
not have — a team vault's — are named in the preview rather than half-imported.
Because Termius' internals are not a stable contract, the layout was read off a
real install with example programs that print structure and counts and never a
value.

### PuTTY and KiTTY, out of the registry

PuTTY and KiTTY keep one registry key per session under
`HKCU\Software\SimonTatham\PuTTY\Sessions` (KiTTY is a fork and kept the format,
only the path differs), so `putty::read_sessions` walks that tree and maps each
key into a host — `HostName`, `PortNumber` as a DWORD, `UserName`, the `.ppk`
in `PublicKeyFile`, and the `homelab/prox-1` folder names people fake in
session names. A missing key is not an error, just nothing to import; the reader
is tested against a throwaway registry tree written and read back on Windows.

A portable KiTTY keeps the same sessions as files instead (`savemode=dir`): a
`Sessions` folder next to `kitty.exe`, one file per session, named like the
registry key and holding `Name\Value\` lines. And people move PuTTY sessions
between machines as `.reg` exports. **KiTTY / PuTTY sessions from a folder…**
takes either — the KiTTY folder, its `Sessions` folder, or a folder with
`.reg` files — through a folder picker; the page never names a path itself.
`session_files.rs` reads both shapes (UTF-16 or ANSI, sizes and counts capped,
`Default Settings` left out) into the same mapping as the registry.

These hosts reference their key by file path and type their password, so this
import carries **no secrets** and needs no vault — unlike Termius. The store
requires an unlocked vault only when a set actually seals something, so a PuTTY
import writes with the vault untouched.

### OpenSSH `~/.ssh/config`

`ssh_config::read_default` reads the user's config — or wherever
`UWURDP_SSH_CONFIG` points — and follows `Include` directives, splicing each
included file in where it appears (the order OpenSSH applies them), with `~`
expansion and a `*`/`?` glob in the final path component for the common
`config.d/*` case, bounded by a depth limit and a visited set against cycles. A
`Host` pattern with wildcards is a rule, not a machine, so it is reported and
skipped rather than turned into a host nobody can reach. Like PuTTY, these
hosts reference a key file and type their password, so the import needs no
vault. (`ProxyJump` is recorded on the imported host but not yet linked into a
chain — that is the ProxyJump feature, not the import.)

### The vault an import lands in

An import carries passwords and private keys, so it needs a sealed home before
it can run. `crates/uwurdp-vault` is that home's crypto (the [vault](#vault)
section has the shape), built earlier than the rest of M2 for exactly this
reason. The store keeps the vault header and, once unlocked, seals each imported
secret as it is written — SQLite only ever sees ciphertext — so importing
requires an unlocked vault, and a failure partway rolls the whole import back,
sealed secrets included. Hosts already present by `address:port` and user are
skipped and a host key already trusted is never overwritten, so a second import
is safe. An import that brings no new secret needs no vault at all.

### UwURDP's own export

Settings → Import & Export writes everything — workspaces, groups, hosts, keys,
trusted host keys, snippets, and optionally the passwords and private keys — into
one `.uwurdp` file: JSON inside an envelope (`format`, `version`, the app
version). With secrets, the whole inner document is sealed with a password of
its own: Argon2id with the vault's parameters, then XChaCha20-Poly1305, with the
KDF parameters and the salt in the associated data, so a file can't be made to
open with other settings than it was written with. Without secrets the file is
plain JSON, and the dialog says so.

Reading one back is the import pipeline again: the file is chosen in a native
dialog opened by Rust and parsed there (64 MiB at most, no more key derivation
work than 256 MiB and eight passes), the page gets a token and a preview with
counts, and the write goes through the same duplicate checks as any other
source. A file is data from anywhere, so it is taken with care: host keys only
for the hosts in the same file, only when the fingerprint really is that key's,
and never over a key that exists or once existed; key paths only when they are
on this computer; hosts only when the host form would have accepted them.

## Connecting

The order is the security model, and it is the order PuTTY and OpenSSH use:

1. **Connect and check the host key.** An unknown key ends the attempt with the
   fingerprint and randomart for the trust dialog; a changed one ends it with
   both fingerprints. Nothing about the user has been sent, and the user has not
   been asked for anything.
2. **Then ask for secrets.** A missing password or passphrase is a question for
   the user, and the verified connection waits for the answer — up to 110
   seconds, just under OpenSSH's default `LoginGraceTime` — so the answer, and a
   second try after a typo, go over the same connection.
3. **Open the shell** on the authenticated connection.

An earlier version asked for the password before connecting, to save a round
trip. Nothing was ever sent to a changed key, but the user typed the password
first and read about a possible man in the middle second. The end-to-end run
caught that; the order above is the fix.

Trusting a key goes through the Rust side, which only accepts a fingerprint a
server actually presented in the last attempt, so a compromised webview cannot
hand in a key of its own choosing. Replacing a key that was already trusted
needs `replace` set explicitly, which only the changed-key warning does. That
warning used to make you type the address; it is two buttons now, with focus on
Reject — the warning is what protects, and typing an address you can read right
above it protected nothing.

Where the login comes from is the host's identity, resolved just before
connecting: a password to ask for, a key file on disk, or a password or key
sealed in the vault (what an import produces). A vault secret is revealed
locally, in memory, right before the connection — the engine still verifies the
host key before it authenticates, so a revealed secret is never sent to an
unverified server, and key material reaches the engine as bytes rather than a
file path. If the host needs a vault secret and the vault is locked, connecting
stops with `vault-locked`, and the app asks for the master password and
reconnects.

After the first login to a host, the same connection runs one short command on
an exec channel — `uname`, `/etc/os-release` and a few marker files, 8
seconds at most, the channel closed on every way out — and together with the
server's SSH banner (`Cisco-1.25`, `ROSSSH`, `OpenSSH_for_Windows`) that
names the system. Only while the system is unknown: a key with a forced command
in `authorized_keys` runs that command for every exec. The result is stored
locally and shown as an icon; it never syncs and never decides anything.

### The password helper

A session that logged in with a password keeps that password in Rust memory
for as long as the session lives, or until the host's stored password is
forgotten. When the cursor's line is sudo's, sudo-rs' or doas' prompt for this
login (`[sudo] password for uwu:`, `[sudo: authenticate] Password:`,
`doas (uwu@host) password:`), a pill offers to type it. Only those bring it
up on their own: a bare `Password:` may be su, docker or ftp, and git's
`Password for 'https://…':` is a website.

The toolbar button and Ctrl+Shift+P always work, whatever is on screen: the
user decides. Enter follows only when the cursor sits after a question — the
line ends in a colon — checked right before typing, so at a shell prompt the
password is typed but never runs as a command or lands in the history. The
page only asks `type_session_password` for its own session and never gets
the password; Rust writes it into that session and nowhere else.

## Files

A file tab opens its own SSH connection with the host's login — the same
negotiation as a terminal: host key first, then the stored or asked-for
secret — and speaks SFTP on it (`russh-sftp`). The left pane is this computer,
read and written by Rust; the right pane is the server. Transfers run in Rust
with a cancel token, checked in every loop, and report progress over a
channel. Names that come from the server go through `safe_local_name` before
they touch the disk, which refuses anything that could leave the target folder
or mean something else to Windows (`..`, separators, drive letters, device
names like `COM1 .txt` or `CONOUT$`, alternate data streams). Downloads get
the same "from the internet" mark a browser gives them (`Zone.Identifier`), so
SmartScreen and Office's Protected View treat them that way.

**Nothing is overwritten unless you say so.** A transfer whose name is already
taken stops before writing anything and asks; replacing then overwrites files
and merges folders. Remote files are created with `O_EXCL`, which also refuses
a link in their place, and replacing one removes what is there — the link
itself, never what it points to. Links are shown as links, and permissions
can't be changed on one, since the server would change its target instead.

**Root.** SFTP has no `sudo`, so UwURDP does what people do by hand: it runs
the server's own `sftp-server` through `sudo` on a pseudo-terminal, with a
prompt marker of its own. The password is written once, only when that marker
shows up, and a second marker says `sftp-server` started before the terminal
is switched to raw and the SFTP handshake begins. A missing or wrong password
keeps the verified connection waiting for the next try, like a login. As root
the pane starts at `/`, and the password is only taken at sudo's own prompt —
a marker at the very end of what the server printed, not one inside some echoed
command line.

Deleting as root doesn't walk the tree over SFTP. From the client that walk can
be raced: a user on the server swaps a listed folder for a link to `/etc`, and
the next delete goes in there as root. SFTP can't open a folder without
following links, so a root delete runs the server's own `rm -rf` through
`sudo` instead, which walks relative to folders it already opened and never
follows a link. The path is quoted for the shell and must be absolute, not `/`,
and free of `..`.

**SMB.** The right pane can also show a share on the same host
(`\\host\share`), connected by Windows with the host's user and a password
typed for it (`WNetAddConnection2W`), then browsed like a local folder. Never
the stored SSH password: SMB has nothing like a host key, so whoever answers to
the name — a spoofed `nas` on hotel Wi-Fi — would get a crackable hash of it.
The address must be a plain host name or IPv4 address, nothing Windows reads as
a port or WebDAV. A local copy never goes into itself, checked on the resolved
paths (a mapped drive is its share, case doesn't matter), and never deeper than
128 folders.

## Tabs

Every session has its own tab, and every tab its own xterm.js terminal and
`TerminalDriver`. Clicking a host opens a tab — or, when one is open, brings
the one shown last back to the front (and reconnects it if its connection
ended). **Open another tab** in the host's context menu starts a second
connection next to it; a small number tells them apart. The local shell in
the sidebar works the same way. Nothing opens on start unless Settings →
Terminal → Open on start names a local shell or hosts. Background tabs stay mounted with `display: none` and keep streaming, so
switching is instant and nothing scrolls out of view while you look elsewhere;
the resize observer refits a tab once it shows again.

A connection attempt is named after its tab, not its host. A verified
connection that waits for a password is keyed by that name in the
`SessionManager`, so two tabs asking for the same host's password never answer
each other's question. Questions are queued: one dialog at a time, and the tab
that asks is brought to the front. Closing a tab cancels its questions, drops a
waiting connection and closes its session.

A page that loads — first start or a reload — calls `close_all_sessions`
first: sessions of an earlier page can't be reached from the new one, and a
flow-controlled session nobody acknowledges would otherwise hang with its SSH
connection open.

The app's shortcuts all need Ctrl and never Alt (AltGr is Ctrl+Alt on German
keyboards), and tab shortcuts add Shift, because plain Ctrl+W belongs to bash.
Paste keys are passed to the webview instead of xterm.js, so pasting uses the
browser's own paste event — no clipboard permission, bracketed paste intact.

## Languages

The app and UwUKeygen speak German and English. German is the source: every
string is written in German where it is shown and wrapped in `t()`
(`lib/i18n.ts`), and `src/i18n/en/*.json` maps each German string to its
English one, a file per area of the app. Placeholders are `{name}`; strings in
module-level constants are marked with `N_()` and translated where they are
shown. The language follows Windows until Settings → Appearance → Language
picks one, and switches at once: components that show text subscribe to the
setting. `pnpm lint` runs `scripts/check-i18n.mjs`, which fails when a German
string has no English. Error messages that come from Rust stay English. The
installer has its own two languages.

## Window

The window has no system frame (`decorations: false`), so the title bar draws
minimize, maximize/restore and close itself, sized like Windows' own caption
buttons, and double-clicking the bar maximizes. The page may do exactly that
and no more: `capabilities/default.json` adds dragging, minimize, toggle
maximize, close and destroy to `core:default`. Closing with open SSH
connections asks first (Settings → Terminal can turn that off).

## Installer and updates

Every system gets UwURDP's own setup, the same one UwUMail uses: a small Tauri
app in `apps/setup` with Nyu in it. On Windows it is with the release builds of the app and of UwUKeygen packed inside
(two zstd payloads; UwUKeygen is optional, ticked by default, and remembered for
updates). It installs
per user into `%LOCALAPPDATA%\Programs\UwURDP` without an admin prompt, adds
Start menu and desktop shortcuts, registers with "Installed apps", replaces the
standard NSIS install of 0.0.1 if it finds one, and fetches WebView2 where it is
missing. The same executable updates (`--update`) and uninstalls
(`--uninstall`, optionally keeping hosts and the vault). Both the setup and the
app only load linked DLLs from System32, since they run from folders the user
can write to.

Updates come in two channels, Stable and Beta, as Tauri updater feeds on the
`updates` branch of this repository. The app checks 20 seconds after start and
every six hours, downloads a newer setup quietly and offers a restart. Only the
setup is signed (minisign, key in `tauri.conf.json`), so the signature is
checked on download and again on the file on disk right before it runs, with
the file locked against changes from that check until the setup has started.
The signature must also name the versioned file for exactly the version the
feed offers (`UwURDP-Setup-<version>.exe` on Windows; the release publishes
the same bytes as `UwURDP-windows-x64-setup.exe` and signs a copy under the
versioned name, so download links never change while the version stays bound), the waiting update must sit exactly where the app put
it, and the setup refuses to replace a newer version with an older one — or to
update at all when it can't tell which version is installed. While another UwURDP window is
running from the same file, nothing hands over, so its connections aren't cut
off. `pnpm release` builds, signs, verifies and publishes; see
[release-notes/README.md](../release-notes/README.md).

**macOS and Linux** run the same setup page on `install_unix.rs`. The apps
arrive as folders — `UwURDP.app` and `UwUKeygen.app`, or the two AppImages
unpacked into AppDirs — packed as one tar archive with a 256 MiB zstd window,
so the second AppDir's copy of WebKitGTK costs almost nothing. The setup
unpacks only those two folders, entry by entry, next to where they go, and
swaps each in by moving the old one aside first. macOS gets them in
`/Applications` (or `~/Applications`), registered with Launch Services; Linux in
`~/.local/share/uwurdp`, with menu entries and an optional desktop icon,
unpacked so that no FUSE is needed. What went where is kept in `install.json`
in the setup's config folder; there is no "Installed apps" list, so the setup,
started again, offers **Uninstall …**. People download one universal `.dmg`
on macOS; the updater runs the bare setup program from it (no quarantine mark,
since it never passed a browser), with the same two signature checks and the
same refusal to go back — and only for a copy the setup installed. Bundles are
signed ad hoc on macOS, which Apple silicon requires; nothing is notarized.

**Linux** downloads are packages now: a `.deb` and an `.rpm` (Tauri's own,
package `uwurdp`, installed to `/usr`) for x64 and arm64, and a portable
`.tar.gz` of the unpacked AppImage. A package installs its own updates:
`updates.rs` sees the bundle type Tauri stamped into the program and checks
that dpkg (`/var/lib/dpkg/info/uwurdp.list` lists the running program) or rpm
(`rpm -qf`) really owns it, then reads the feed entry `linux-<arch>-deb` /
`-rpm`, checks the signature names `UwURDP-<version>-linux-<arch>.deb` /
`.rpm`, and on **Restart now** hands the checked bytes from a private temp
folder to `pkexec dpkg -i` / `pkexec rpm -U`. The AUR package repacks the
`.deb` but isn't dpkg's, so it updates through pacman; the portable folder
doesn't update. The setup AppImage is still built for x64, as the updater-only
`UwURDP-update-linux-x64.AppImage`, for copies the setup of 0.1 installed in
`~/.local/share/uwurdp`.

CI (`.github/workflows/installers.yml`) checks the workspace on macOS and
Linux and builds Windows on ARM, the universal macOS disk image and the Linux
packages for x64 and arm64 when a tag is pushed, installing the `.deb` once as
a check. It holds no key: `pnpm release` downloads what it built and signs it
here. `.github/workflows/aur.yml` pushes the AUR package `uwurdp-bin` once a
release is published, with the AUR key and nothing else.

## UwUKeygen

`crates/uwurdp-keygen` generates RSA (1024–4096, 2048 by default), Ed25519
and ECDSA (P-256, P-384, P-521) keys and writes them as OpenSSH, PuTTY `.ppk`
v3 and v2, or PKCS#8 PEM, encrypted when there is a passphrase. Randomness
comes from the operating system; what the mouse adds on Nyu's laser pad is
mixed in on top, never instead. The UI is `components/keygen`, used by the host
form, by Settings → Vault & Keys, and by `apps/keygen`, the standalone app,
which includes the same Rust commands and knows nothing about a vault.

A copied private key goes through Rust, with the clipboard formats that keep it
out of Windows' clipboard history and cloud clipboard, and is cleared again
after a minute unless something else was copied. A saved one gets an access
list for this user, SYSTEM and Administrators only — what Win32-OpenSSH wants
anyway. Reading a key file checks its own key derivation settings before a
passphrase is tried (`check_costs`): a `.ppk` asking for four billion Argon2
passes, or PKCS#8 asking scrypt for a terabyte, is refused rather than run.

## Storage

`crates/uwurdp-store` keeps hosts, identities and trusted host keys in one
SQLite file — bundled SQLite, WAL — in the app data directory, or wherever
`UWURDP_DB` points. Records carry the sync header from the first row on, so sync
in M2 needs no data migration. `secure_delete` is on, and the write-ahead log
is truncated after a secret is forgotten, so an old password's ciphertext
doesn't linger in free pages.

**Nothing secret is stored outside the vault.** A host either asks for its
password, references a key by file path, or keeps its password or key in the
vault, sealed, revealed only while the vault is unlocked. The one exception is
the vault key sealed with DPAPI when the vault is remembered on this device (see
[above](#opening-with-the-windows-account)).

## How it is tested

- **Unit tests** in every crate: clock, crypto, merge, parsers, flow control,
  the store.
- **Both halves against each other**, in the server repository's
  `tests/client.rs`: it pulls this repository's store, vault, sync engine and
  transport as a git dependency and runs them against the real server over
  HTTP. A host with a password in the vault travels to a second device that
  joined by being told three words; a device that heard the wrong words gets
  nothing; and with the stored records in front of it, none of them contains
  the hostname, the address, the group or the password. That test found a real
  bug the first time it ran — a header written by its own SQL, missing a field.
- **Two devices against a server**, in `crates/uwurdp-sync/src/tests.rs`: two
  real stores sharing one vault, syncing through `MemoryServer` — which is the
  server's rules as running code, so the server's own tests can later be held
  against the same thing. They check what sync has to get right: a host and its
  sealed password arriving on the other device, two simultaneous renames ending
  the same way on both, a delete that does not come back, a device joining with
  hosts of its own, a second pass finding nothing to do, two devices that
  trusted different keys for one machine agreeing on one — and a server that
  misbehaves getting nowhere: a flipped tombstone flag, an old version replayed
  under a new clock, a record from another vault. Plus the two properties
  underneath: nothing readable in what the server holds, and a field a newer
  build wrote surviving an edit by an older one.
- **SSH integration tests** in `crates/uwurdp-core/tests/ssh.rs` run a real SSH
  server in-process — russh's server half — and check on every commit what
  matters most: no login attempt against an untrusted or changed key, the
  password asked for only after the key checked out and sent over that same
  connection, keystroke order, resize, remote exit, an 8 MiB flood that
  arrives complete under a deliberately slow renderer, two tabs logging in to
  the same server side by side, and a reloaded page closing what the old one
  left open. `tests/files.rs` does the same for files: browsing, upload,
  download, rename, delete and cancel against an in-process SFTP server, root
  through a toy `sudo` with no, a wrong and the right password on one login, and
  the system probe.
- **The setup** is tested against a sandbox (`UWURDP_SETUP_SANDBOX`): files,
  shortcuts and registry entries land in a throwaway folder and key, including
  replacing the old NSIS install and uninstalling with and without the data.
- **End to end**, `node apps/desktop/e2e/run.mjs`, drives the real app against
  `crates/uwurdp-core/examples/dev_sshd.rs` over WebView2's DevTools protocol:
  adding a host, trusting its key, a wrong and a right password, a 32 MiB
  flood, the session ending, a reconnect, the server's key changing, deleting
  the host, and opening the import dialog — counting connections and password
  attempts in the server's log. It also opens a second tab to the same server,
  types into both, closes one while the other keeps working, maximizes and
  restores the window, checks that closing with an open connection asks first,
  and changes a terminal setting in the settings dialog. A third phase runs a separate app on a
  database seeded with a vault-key host (via the `seed_vault_key` example) and
  a `dev_sshd` that authorizes the key: connecting unlocks the vault, trusts
  the key and logs in with the vault key, and the server's log confirms an
  accepted public key and no password. Since 0.1.0-beta.2 the run also checks
  the detected system and its icon, highlighting, Ctrl+wheel, the sudo helper
  (typed once, only after the click), saving a password into a new vault this
  device remembers, dragging hosts between groups and workspaces, a file tab
  downloading and uploading by drag and drop, and an export that a fourth phase
  reads back into a fresh database, wrong password first.

  Phase E (0.1.0-beta.8) is sync, for real: a UwUSSH server from the sibling
  repository with its own certificate, and two app instances — one through
  `pnpm tauri dev`, one straight from the debug binary on its own DevTools port
  and database. The first connects with the setup code, is shown the recovery
  kit, imports a host and offers a pairing code; the second types the short
  code with the address and fingerprint and gets the host; a group made on the
  second reaches the first; the first revokes the second with the master
  password (a wrong one first), whose next pass is refused; and the revoked
  device leaves, after which the master password alone opens its vault and its
  hosts are still there. `run.mjs --only=e` runs just that.

- **macOS and Linux** are checked by CI on every tag and on `ci/**` branches:
  formatting, clippy and the whole test suite on both, then the setups built.

The end-to-end run earned its place on its first outing. It found four bugs no
other test could see: the password asked for before a changed host key was
shown; Enter in a dialog reaching a button behind it and starting a second
connection; a resize feedback loop that grew the terminal to 5000 columns; and a
validation message that stayed after the field was fixed.
