# Vision

What I want UwURDP to be, and what it will never do.

## The problem

Remote desktop clients fall into three camps, and all of them annoyed me.

**mstsc.** Built in, solid, and made for exactly one server at a time. Sixty
servers means sixty `.rdp` files, or typing addresses from memory.

**Remote Desktop Connection Manager.** The one that actually fits the job: a
tree of servers, a login a whole group inherits, thumbnails of everything
that's open. But it looks like 2008, it lives in one `.rdg` file on one
machine, its saved passwords only open on the Windows account that saved them,
and Microsoft keeps it alive with the least effort possible.

**The modern ones.** Royal TS, Devolutions, the Microsoft Store app and
friends. Some are good. Most want an account, a subscription or a licence per
seat, and the ones that sync send your server list — addresses, users, often
passwords — through somebody else's cloud.

I wanted RDCMan's way of working, with a face from this decade and a sync
server of my own.

## What UwURDP is

An RDP client for people who look after more Windows machines than they can
remember, from more than one computer — admins and homelabbers with twenty to
a couple hundred servers.

Four things it has to get right:

1. **Groups and logins, the RDCMan way.** A group has a login, its hosts
   inherit it, a host can have its own. Open a whole group at once, see all of
   it as thumbnails, jump between desktops without hunting for windows.
2. **Your hosts, your passwords, your server.** Sync is end-to-end encrypted
   and the server is yours. It relays ciphertext; it cannot read what it
   stores. Running no server at all is an equal option, not a punished one.
3. **It has to take your old setup with it.** An RDP client that starts empty
   is an RDP client you close again. RDCMan and `.rdp` import came before the
   first beta, not after.
4. **The desktop has to feel like mstsc.** Keys where your fingers expect them,
   in the server's layout, Ctrl+Alt+End for Ctrl+Alt+Del, a desktop that follows
   the window. If it feels worse than the built-in client, nothing else matters.

## What it will never do

- **Phone home.** No telemetry, no analytics, no crash pings, no account with
  me.
- **Hold your data hostage.** Everything exports, into a file UwURDP reads back
  and you can read yourself.
- **Charge a subscription for sync.** Sync is a protocol and a small binary,
  not a service I should be renting to anyone.
- **Grow into an everything-client.** SSH has [UwUSSH](https://github.com/MinifyX/UwUSSH-Client).
  VNC, SPICE and serial consoles are other tools. UwURDP does RDP.
- **Make security cute.** Nyu is playful everywhere except where it matters. A
  changed certificate, a failed unlock, a login the server rejected: those are
  plain, blunt, and free of kaomoji.
- **Pretend to be a team product.** This is built for one person with several
  computers, and that is what it will always be good at first.

## Who it's for

Me, first. If it fits you too, take it — it's GPL, fork it and make it yours.
But I build what I need, I answer issues when I get around to it, and I don't
promise a release schedule. That trade is the whole point: the app stays
opinionated because nobody has to be talked out of an opinion.
