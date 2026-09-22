# Vision

What I want UwURDP to be, and what it will never do.

## The problem

SSH clients fall into two camps, and both annoyed me.

**PuTTY, KiTTY, MobaXterm.** Powerful, local, free, and completely stuck. No
sync, barely any grouping, a UI from another era. Move to a different machine
and you start over — or you copy a registry export around like it's 2004,
because it is.

**Termius, Tabby Cloud, and friends.** Calm, modern, pleasant to use. But the
sync that makes them worth it runs through their servers, usually behind a
subscription, and it carries your entire infrastructure: host names, addresses,
usernames, and in some setups your keys. That is a lot of trust to hand over
for the convenience of not retyping a host list.

I wanted the second one's interface with the first one's independence.

## What UwURDP is

An SSH client for people who have more hosts than they can remember and more
than one machine to reach them from — homelabbers and admins with twenty to a
couple hundred hosts.

Three things it has to get right:

1. **Your hosts, your keys, your server.** Sync is end-to-end encrypted and the
   server is yours. It relays ciphertext and a sequence number; it cannot read
   what it stores. Running no server at all is an equal option, not a punished
   one.
2. **Keys and logins belong in the vault.** Not scattered across `.ppk` files,
   not re-entered per device. Unlock once, and every host you own is reachable
   from every machine you own.
3. **It has to take your old setup with it.** An SSH client that starts empty
   is an SSH client you close again. Import comes early, not eventually.

## What it will never do

- **Phone home.** No telemetry, no analytics, no crash pings, no account with
  me.
- **Hold your data hostage.** Everything exports. A vault you cannot leave is
  lock-in wearing a security badge.
- **Charge a subscription for sync.** Sync is a protocol and a small binary,
  not a service I should be renting to anyone.
- **Grow into an everything-client.** No RDP, no VNC, no serial-over-IP
  appliance manager. SSH, SFTP, tunnels, and a local shell — that is the shape.
- **Make security cute.** Nyu is playful everywhere except where it matters. A
  changed host key, a failed unlock, a request to forward your agent: those are
  plain, blunt, and free of kaomoji, in both tones.
- **Pretend to be a team product.** Shared vaults may come one day, but this is
  built for one person with several machines, and that is what it will always
  be good at first.

## Who it's for

Me, first. If it fits you too, take it — it's GPL, fork it and make it yours.
But I build what I need, I answer issues when I get around to it, and I don't
promise a release schedule. That trade is the whole point: the app stays
opinionated because nobody has to be talked out of an opinion.
