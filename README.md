# PokePlanet — Roadmap

An Emerald MMORPG for PC: the pret pokeemerald decompilation, via the SDL2 port, turned into a
server-authoritative multiplayer game. The bar is *"as if Nintendo built an Emerald MMORPG"* —
polished, authentic, a genuinely fresh take, and **no pay-to-win**.

> **This file and `README.md` are kept identical.** Change one, change both, in the same commit.
> Update it whenever something moves between Done, In progress and Planned — a roadmap that
> lags is worse than none, because people act on it.

**Last updated:** 2026-08-10 &nbsp;·&nbsp; **Building and running:** [docs/BUILDING.md](docs/BUILDING.md)

---

## See it running

https://ss.obby.ca/t_2026_08_10_02_01_24_DLA0eV.mp4

Two clients on one machine, each signed in as a different account, seeing each other move in
the same world.

> Clips and screenshots are worth more than a paragraph for anything involving movement,
> timing or a freeze. When reporting a bug, a few seconds of video says immediately what a
> description takes several exchanges to pin down.

---

## Architecture in one paragraph

The game is a 60fps frame-locked GBA program running on two threads: an SDL event loop and an
`AgbMain` game thread, where `VBlankIntrWait()` is the only yield point. Nothing blocking may
run on the game thread, which is why a separate Rust process — the **sidecar** — owns the
network. The sidecar speaks QUIC to the server on lucy (Postgres behind it) and a small
fixed-size protocol over loopback to the game. Almost every hard bug so far has lived at one of
those two seams.

---

## Done

### World and presence
- **Server-authoritative movement.** Teleports, impossible speeds, diagonal steps and walking
  through walls are all refused server-side, checked against collision exported from the game's
  own map data. 33+ server tests.
- **Presence indexed by map**, so a snapshot concerns only the players sharing a map rather
  than scanning every player each tick.
- **Remote players visible and animated**, moving in step rather than teleporting.
- **One agreed spawn point** between client and server.

### Identity and accounts
- **Discord sign-in** through the sidecar, with a cached token so a relaunch skips the browser.
- **Unique character names**, case-insensitively, with a migration that renames existing
  duplicates rather than failing the index and taking the server down at startup.
- **Signing in twice closes the older session** rather than leaving two clients fighting.
- **Full account names shown everywhere** they were previously cut to the save's seven
  characters: trainer card, battle text, hall of fame, the save box, the Pokédex, a Pokémon's
  OT line, and the versus screen.
- **You look like your own character.** The server assigns each character an overworld sprite;
  the local player now renders as it too, rather than always being Brendan.

### Saving
- **The character lives on the server.** The save is uploaded, stored in Postgres, and handed
  back at sign-in, so deleting the local file loses nothing and a different machine resumes the
  same character.
- **Autosave on every change**, hooked at eleven funnels covering flags, variables, money, the
  bag, and every Pokémon mutation. Verified present in the shipped binary.
- **Local saving removed while signed in.** `SAVE` is gone from the start menu, and the game no
  longer writes a save file that nothing should read.
- **The server reads the save it stores** — sectors, slots, flags, variables, money, coins, the
  bag and the party, including the encrypted substruct decode.

### Anti-cheat (see *Not yet cheatproof* below)
Refused server-side, each verified against real data with a negative control:
- Movement: teleport, speed, diagonal, walls.
- Money above 999,999 and coins above 9,999 — the caps the game itself clamps to.
- Bag quantities of zero, or above 99.
- Party level above 100; effort points above 255 in a stat or 510 in total.
- Experience above 1,640,000, and a level 100 with too little experience behind it.
- Per-Pokémon checksum integrity.
- **Progress going backwards** — a Pokémon losing experience or levels.
- **Gaining faster than the published rates allow** — money or experience appearing in less
  time than any amount of play could produce, measured against the rates the server itself
  publishes, so the ceiling widens on a generous server rather than accusing it.

### Multiplayer features
- **Chat** with global, map and private scopes, bridged to IRC, with `/s`, `/w NAME` and `/r`.
  A whisper with nobody to whisper to is dropped rather than broadcast.
- **Battles between two players.** Invitations both ways, the server assigning slots, and the
  link-battle protocol carried over the network instead of a cable.
- **Gameplay rates held by the server**: experience, encounters, money, items, catch and shop
  prices, plus per-species encounter rates. Edit one file, restart, every client is told.

### Tooling
- `tools/debug/two-client-battle.sh` — two real clients, one battle, headless.
- `tools/debug/test-chat-parse.sh` — chat scope parsing, 28 cases on the host.
- Test sidecars run on their own ports, and a game only talks to the sidecar it launched.

---

## In progress

- **Battles freeze mid-turn.** A battle now starts, the intro completes and both players reach
  the first turn. It then stops: in play, after the first moves resolve ("Foe A used TACKLE!"),
  and in the headless harness one step earlier, at action selection waiting for the opponent.
  It reaches further each time it is measured -- most recently close to a second turn -- which
  points at a round trip that completes sometimes rather than one that never happens.

  The intro freeze is fixed, so controller *data* crosses the link. What is not proven is the
  acknowledgement:  does not clear the exec flag locally on the
  link branch -- it queues  and depends on that block coming back,
  which is what clears the per-player bits. Note also that the responder never reaches the
  action-selection stage in the harness, which is expected for the non-master (its engine is a
  dummy and its controllers are driven by the master) but should be confirmed rather than
  assumed.

- **Palettes beyond the hardware's four.** Sprites can carry a palette of their own and both
  renderers honour it; nothing assigns one yet, and the extended bank does not participate in
  fades.

---

## Planned

### Next
- **Every player a recoloured Brendan or May.** Those two have complete frame sets, so cycling
  and surfing stop breaking the illusion; colour derived deterministically from character id.
- **Everything configurable.** Extend the rate config from named scalars to a general table so
  any random chance, reward, drop rate or price is tunable without a protocol change.

### Later
- **Structured progression on the server** — flags, variables, bag and party as data rather
  than an opaque image, so the save upload can eventually be retired.
- **Headless engine.** Running the game's logic server-side. This is the only thing that makes
  a *careful* forgery impossible rather than merely hard, and it is a large piece of work.

---

## Not yet cheatproof

Stated plainly, because it would be easy to read the anti-cheat list above and conclude
otherwise: **movement is genuinely server-validated; progression is not, fully.**

Every hard, unambiguous invariant the game itself enforces is now enforced server-side, and
crude edits are caught. But a patched client can still award itself *legal-looking* things it
never earned — a rare species at level 100 with legal effort points, or money below the cap it
never made. Closing that needs rate-based enforcement, and ultimately the headless engine.

---

## Working notes

Things that cost real time and are easy to trip over again:

- **Deploying to lucy is a separate step and fails quietly.** `ssh lucy` runs as a different
  user, so `$HOME/.cargo/bin/cargo` resolves to the wrong home and the build never runs. Use
  `sudo -u pokeplanet /usr/bin/cargo build --release`, and check the binary's timestamp moved.
- **The rates file lives at `/opt/pokeplanet/rates.conf`**, not in the repo — the service's
  working directory is not the checkout, and a file in the source tree is silently ignored.
- **gdb inferior function calls crash this binary.** Direct memory reads and `set var` writes
  work; calls into the game do not.
- **Read constants from the compiled binary, not from source macros.** `gExperienceTables` is
  macro-generated; asking gdb what the binary contains cannot be misremembered.
- **A skipped test looks exactly like a passing one.** Fixture-gated tests must be re-run
  against a deliberately wrong value to prove the assertion executes. Two save decodes passed
  vacuously against empty fixtures and would have locked players out of their own characters.
- **Reasoning loses to measurement.** Three separate changes compiled, read correctly, and were
  wrong. The battle teardown in particular was found by a backtrace after every reading of the
  source said the check should pass.
