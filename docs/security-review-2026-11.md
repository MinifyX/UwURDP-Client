# Security review, November 2026

Dated 2026-11-07, against everything between `v0.1.0-beta.14` (`937ef9b`, the
last published release) and `5c4fa41` (0.2.0-beta.2), release notes and lock
files left out: `git diff v0.1.0-beta.14 HEAD -- . ':!release-notes' ':!*.lock'`.
The code under review is that of `2e82c3f`; the commit after it only sets
version numbers.

Two changes carry the round:

- **Sign in with UwUAuth** (#20, `e3d7525`, `35a87c2`): a UwUSync server
  paired with UwUAuth lets a person in through the browser instead of an
  invite or a code from another device. New are `uwurdp-sync/src/sso.rs`, the
  `SsoStart`/`SsoStarted`/`SsoFinish`/`SsoTicket` messages and the
  `sso_ticket` fields in `uwurdp-proto/src/api.rs`, `parse_server`,
  `create_account_sso` and `join_sso` in `flow.rs`, four Tauri commands in
  `apps/desktop/src-tauri/src/sync.rs`, and the `SsoForm` in
  `SyncSettings.tsx`.
- **Manifest check** (`2e82c3f`): `check_manifests` in
  `uwurdp-store/src/manifest.rs` now takes one entry per (kind, id) across
  every manifest held — the newest clock, the tombstone at an equal one —
  before it compares with what is here.

## The trust boundary

As in September. Two parties are new:

- **UwUAuth** says who a person is, to the sync server. It is the server, not
  this device, that is the OpenID Connect client: the device opens the
  browser, listens on a loopback port for the one answer, and hands the code
  and its PKCE verifier to the server, which gives back a short-lived ticket.
  UwUAuth must not learn the master password or the account key, and a sign-in
  there must not open a vault.
- **The browser** and anything else on the machine can reach the loopback
  port. It must not be able to slip in a code of its own.

The sync server stays what it was: it may be hostile, holds only sealed
records, and must not be able to read the vault, forge a record or keep one
back unnoticed. With SSO the person can now also reach a server by typing its
address, so the server is no longer always one another device vouched for.

## Findings

No Critical or High finding. SSO-M1, SSO-L1 and SSO-I3 were fixed before
0.2.0-beta.2 shipped (see _Fixed before the release_ below); the rest stand.

| Severity | Where                 | What                                                                                                            |
| -------- | --------------------- | --------------------------------------------------------------------------------------------------------------- |
| Medium   | `flow.rs` `join_sso`  | A device that joins with UwUAuth has no manifest to wait for and trusts other devices' certificate pins at once |
| Low      | `join_sso`, `SsoForm` | The server decides whether the account key goes into the login key                                              |
| Low      | `sso.rs` listener     | Anything that finds the loopback port can end or stall a sign-in                                                |
| Info     | `sso.rs` `begin`      | The `state` comes from the server and is taken as it is                                                         |
| Info     | `sync.rs`             | A second sign-in un-cancels the first                                                                           |
| Info     | `flow.rs:373-375`     | The doc comment compares an SSO join with a restore from the kit, which this app does not have                  |

### SSO-M1, Medium — an SSO join trusts other devices' certificates without a manifest

`crates/uwurdp-sync/src/flow.rs:376-430` (`join_sso`). `join` ends with
`store.set_manifest_floor(handover.manifest)` (`flow.rs:346`): the device
that added this one says which of its manifests must arrive, and until a
complete pull has found it, `known_host`
(`uwurdp-store/src/known_hosts.rs:42-68`) answers "new" for every pin another
device made. That is the fix of September's first shared Medium (`41da1d2`).
`join_sso` has nobody to hand it a floor and sets none, so a device that came
in through UwUAuth starts with an empty `manifest_violations` table and trusts
every certificate pin the server hands it.

A hostile server — or one that was broken into — can then leave out every
manifest (nothing says one should be there) and hand the new device an old
pin for a host whose certificate was replaced, or keep back the tombstone of
a pin the user removed on purpose. With the old certificate's private key, a
man in the middle is then trusted on that device, and CredSSP gives it the
login. This is exactly what the floor exists for, and SSO is now the easiest
way to add a second device.

**Fix.** A device added without a floor has to hold other devices' pins in
doubt until something it trusts says otherwise. The smallest change: at the
end of `join_sso`, write a `Problem::Unchecked` row for `EntityKind::Manifest`
with the nil id (as `set_manifest_floor` does for a real one), and clear it
only when a pairing from an already connected device delivers a floor, or when
the person confirms each pin once on this device. Until then a connection asks
as for a new host, showing that another device had trusted a fingerprint and
whether it is the same. Say so in the sign-in form ("certificates are asked
again on this device"). A test in `uwurdp-sync/src/tests.rs` in the style of
the floor tests: SSO join, server withholds all manifests, `known_host` for a
pin from device A is `None`.

### SSO-L1, Low — the server decides whether the account key is mixed in

`crates/uwurdp-sync/src/flow.rs:395` and
`apps/desktop/src/components/SyncSettings.tsx:448,599`. Whether an account
needs the account key comes from the server twice: as
`SsoTicket::needs_account_key`, which decides whether the form even shows the
recovery code field, and as `WireVaultParams::needs_account_key`, after which
`join_sso` throws away a code the person did type
(`account_key.filter(|_| params.needs_account_key)`). A server that says
"no account key" gets an `auth_key` derived from the master password alone,
and `join_sso` sends it before `unlock_with` fails.

With an account key mixed in, that login key cannot be guessed at any cost;
without one it can be guessed offline at the Argon2id costs the server named.
`login_key_from` holds those to `KdfParams::FLOOR` (19 MiB, t=2) when no
account key is used, so this is a guess per password at the OWASP floor, not a
free one, and the vault itself still needs the account key. What is lost is
the second secret's protection of the master password — which matters most if
it is used elsewhere. `join` does not have this: it mixes in the key it got
from the other device whatever the server says. Accounts without an account
key exist (vaults from before it, `schema.rs:263`), so the device cannot simply
refuse.

**Fix.** In `join_sso`, use a typed account key whenever there is one, and when
the server says none is needed although one was typed, stop with an error
before anything is sent (or try with the key first). Show the recovery code
field always when joining, marked optional only when the server claims the
account has none, with a hint that every account made by UwURDP has one.

### SSO-L2, Low — anything that finds the port can end or stall a sign-in

`crates/uwurdp-sync/src/sso.rs:133-173, 190-239`. The listener takes one
connection at a time and gives each up to `REQUEST_DEADLINE` (2 s) to send its
request. Eight requests on `/uwusync/callback` with a wrong or missing `state`
end the sign-in (`MAX_WRONG_STATE`); a page in the browser that scans the
ephemeral ports with `fetch` (Firefox does not ask; newer Chrome asks before
a page reaches local addresses) can do that in the five minutes. A local
process can instead keep a silent connection open, one after the other, and
hold the real answer back until the sign-in times out. Neither can bring in a
code of its own — the state check is sound — so this is denial of service
only: the person signs in again.

**Fix**, if wanted: count wrong states only for requests whose `Host` header is
`127.0.0.1:<port>`, read connections on a short-lived thread each (still
capped) so a silent one does not hold up the next, and say in the error that
something else on the machine answered.

### SSO-I1, Info — the `state` is the server's

`sso.rs:83-88`. The device checks the answer against `SsoStarted::state` as the
server sent it, in constant time, but takes it at any length, the empty
string included; with an empty one, `?state=&code=…` matches. A correct
server makes it random and binds it to the PKCE challenge, and the code is
useless without the verifier, so nothing follows today. Refusing a state
shorter than 22 characters (128 bits in base64url) in `begin` costs nothing and
keeps a broken server from turning the check off.

### SSO-I2, Info — a second sign-in un-cancels the first

`apps/desktop/src-tauri/src/sync.rs:789`. `sync_sso_sign_in` clears the one
`SSO_CANCEL` flag, so a sign-in still waiting from before keeps waiting for its
five minutes and, should it finish last, its ticket replaces the newer one in
`SSO_PENDING`. The form allows only one sign-in at a time and cancels on
leaving, so this needs a quick back-and-forth to happen. A sign-in counter
(cancel by id) instead of one flag removes it.

### SSO-I3, Info — "like one restored from the kit"

`flow.rs:373-375` says a device joining through UwUAuth "starts like one
restored from the kit". UwURDP has no restore from the kit; the sentence hides
that this is the first way in without a manifest floor (SSO-M1). Reword it with
that fix.

## Fixed before the release

- **SSO-M1.** `join_sso` ends with `Store::wait_for_any_manifest`: a floor
  with a reserved id (`Uuid::max()`) and the join's wall clock. The check takes
  any other device's manifest at least that new as the floor; until one is
  here the floor stays `Unchecked`, which raises no alarm (the other devices
  may simply not have synced since) but keeps `known_host` from trusting pins
  other devices made. An older manifest does not count, since it is one the
  server could pick to leave out a newer pin; this device's own does not count
  either. A pin the person confirms on this device is its own and trusted as
  before. Test: `a_device_that_joined_alone_waits_for_any_newer_manifest`.
  Unlike the suggestion above, the doubt clears by itself once another device
  publishes a manifest after the join, which a server cannot forge.
- **SSO-L1.** `join_sso` no longer drops a typed account key: if the server
  says the account has none although one was typed, it stops with
  `FlowError::UnexpectedAccountKey` before anything is sent. The sign-in form
  shows the recovery code field whenever it joins an existing account.
- **SSO-I3.** The doc comment says what the join does about manifests now.

The same three changes went into UwUSSH 0.4.0-beta.3, whose UwUAuth sign-in
shipped with 0.4.0-beta.1 and had the same gaps.

## Checked and fine

- **The master password never reaches UwUAuth.** The browser sees UwUAuth's
  own sign-in only. What the device sends anywhere is the `auth_key` to the
  sync server, derived from the password and the account key exactly as for
  an invite or a pairing (`login_key_from`, with the cost cap and floor). The
  account key and recovery kit for a new account are made on the device
  (`create_account_with`), as before.
- **A sign-in does not open a vault.** The ticket only takes the place of the
  invite (`CreateAccount`) or the enrolment token (`EnrolDevice`,
  `VaultParamsRequest`); `join_sso` opens the vault with
  `header.unlock_with(password, account_key)` and nothing else. A server that
  hands over another vault's header gets a failed unlock.
- **Loopback redirect.** Bound to `127.0.0.1` on a fresh port, redirect URI
  `http://127.0.0.1:<port>/uwusync/callback` (RFC 8252, no `localhost`), one
  answer, five minutes, cancellable. Requests capped at 16 KiB and 2 s; only
  `GET` on the exact path is read; parameters through `form_urlencoded`.
- **State and PKCE.** 32 random bytes as the verifier, S256 checked against
  RFC 7636's example; the verifier goes only to the sync server, and only
  after the answer. The state is compared in constant time, and an `error` is
  only believed with the right state, so a page cannot end a sign-in with a
  fake refusal. `iss` (RFC 9207) is passed on for the server to check.
- **What the browser gets.** The authorization address is opened only when it
  is `https`, or `http` to `127.0.0.1` as parsed — no userinfo, no other
  scheme; tested against `127.0.0.1.example.com` and `127.0.0.1@…`. The
  callback pages are fixed text with `no-store` and `no-referrer`, so the code
  does not leak onwards.
- **The ticket stays in Rust.** It lives in `SSO_PENDING` as
  `Zeroizing<String>`, never goes to the page and is never logged or written
  to disk; the page learns `hasAccount`, `needsAccountKey`, the names and the
  server address. A ticket is dropped once used, on cancel, on `sso-off` or
  `account-disabled`, and after `SSO_TICKET_TRIES` wrong passwords, matching
  the server's own count. Names from the server reach the page as React text.
- **Server address.** `parse_server` takes a setup code (with its pinned
  fingerprint; the invite is dropped when a ticket is used) or an `https`
  address checked against the system's roots; plain `http` only to
  `localhost`, `127.0.0.1` and `[::1]`; no userinfo, query or fragment.
- **Command surface and page.** Four new commands, taking a server string, a
  password, a name and an optional recovery code; the same paired, UwULock
  and locked-vault checks as `sync_connect` and `sync_join`.
  `capabilities/*.json`, the content security policy and the opener's
  permissions are unchanged; the opener is still used from Rust only.
- **Dependencies.** None new: `Cargo.lock` changes only UwURDP's own version
  numbers, and `sso.rs` uses `url`, `sha2` and `rand`, which `uwurdp-sync`
  already had.
- **The manifest change cannot hide a withheld record.** A manifest is sealed
  under the vault key like every record, so the server can leave one out or
  hand an older version — as before — but cannot write an entry. Collapsing
  per (kind, id) only lets a tombstone a device sealed, at a clock at least
  that of the live entry, stand for it; and `check_entry` still holds that
  tombstone against what is here: a record held here in an older, live version
  is still `Older`, and only a record this device never had passes. Since a
  tombstone beats every edit when records merge, such a record ends deleted
  either way, so nothing that should be here is missed. A tombstone older than
  a later edit loses the collapse and leaves the edit to be checked (tested).
  The floor check is untouched, kinds this build does not know are still
  skipped, and host-key superseding works as before. Someone holding the vault
  key could seal a manifest of tombstones to mask entries, but could just as
  well push the records themselves, so no new power comes from it.
