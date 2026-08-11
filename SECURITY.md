# Security Policy

PokePlanet is a server-authoritative online game. Its central promise is that a client cannot
forge progress or affect anyone else's game — so a way to break that promise is a security bug,
not just a gameplay one.

## What counts as a security issue

- **Forging state the server should own** — money, items, badges, Pokémon, story flags, position:
  anything a modified client can make the server accept that an honest client could not produce.
- **Affecting another player** — moving, impersonating, disconnecting, or reading data for a
  character that is not yours.
- **Account or session compromise** — anything that lets one account act as another, bypass a
  ban, or hijack a login.
- **Denial of service** — crashing or wedging the server, or a single client consuming resources
  out of proportion to honest play.

Ordinary cheating that the server already refuses (and logs) is working as intended; a way
*around* those refusals is the bug.

## Reporting

Please report privately rather than opening a public issue, so a fix can ship before the method
spreads:

- Open a [private security advisory](https://github.com/obnoxiousmods/PokePlanet/security/advisories/new), or
- message the maintainer directly.

Include: what you did, what the server accepted that it should not have, and — if you can — the
smallest reproduction. A packet capture or a diff against the stock client is ideal.

## What to expect

This is a small project, so there is no formal SLA, but a plausible report of a real
state-forgery or account bug will be treated as the highest priority. You will get an
acknowledgement, a fix, and credit in the fix's commit unless you would rather stay anonymous.

## Scope

The server (`server/`), the sidecar (`server/net/`), and the networked parts of the client
(`src/platform/net_client.c`, `src/mmo_*.c`) are in scope. The upstream pokeemerald game code is
not a target in itself, but a bug reachable *through* the network path is.
