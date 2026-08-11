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
  bag and the party, including the encrypted substruct decode — and projects the flags, vars,
  bag and party into tables it can query rather than bytes it can only keep.

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
  whole link-battle protocol carried over the network instead of a cable -- intro, turn loop
  and all. Two players can fight a match.
- **Gameplay rates held by the server**: experience, encounters, money, items, catch and shop
  prices, plus per-species encounter rates. Edit one file, restart, every client is told.

### Beyond the hardware
- **More than four sprite palettes.** The GBA gave the overworld four for everyone who is not
  the player, with no reference counting, so characters repainted each other. That ceiling was a
  convention rather than a limit here: sprites now carry a palette of their own, both renderers
  honour it, and the bank follows fades like any other.

### Tooling
- `tools/debug/two-client-battle.sh` — two real clients, one battle, headless.
- `tools/debug/test-chat-parse.sh` — chat scope parsing, 28 cases on the host.
- Test sidecars run on their own ports, and a game only talks to the sidecar it launched.

---

## In progress

- **Per-player colours.** The palette ceiling is gone — sprites carry their own palette, both
  renderers honour it, and the bank follows fades — but nothing assigns one yet. Assigning needs
  a colour on the wire, which neither the remote-player nor the profile message carries.

---

## Planned

### Next
- **Every player a recoloured Brendan or May.** Those two have complete frame sets, so cycling
  and surfing stop breaking the illusion; colour derived deterministically from character id.
- **Everything configurable.** Extend the rate config from named scalars to a general table so
  any random chance, reward, drop rate or price is tunable without a protocol change.

### Later
- **Retire the save upload.** *No longer blocked on data loss; blocked on being worth doing.*
  The typed tables hold flags, variables, money, coins, the bag, the party, the Pokedex, the
  sixty-four game counters, berry trees and trainer rematches. SaveBlock1 holds roughly thirty
  fields that nothing parses -- mail, the daycare, secret bases, contest winners, decorations,
  heal location, link battle records, easy-chat phrases and the rest.

  The server now keeps SaveBlock1 **whole**, alongside the parsed fields. That separates two
  things that were tangled together: *understanding* a field, which is needed to validate it,
  and *preserving* one, which is needed not to destroy it. Preservation is the lower bar, and it
  is the one that governs whether retiring the image is safe. Rebuilding a save from parsed
  tables alone would have returned every unparsed field as zero -- party kept, mail and secret
  bases gone, permanently and without warning. Splicing the server's authoritative fields into
  the preserved block cannot.

  The server can now also **author** a save, not only read one (`write_block1` / `reauthor`).
  That was the missing half: it could understand the image in detail and still not produce one,
  so the client stayed its origin regardless. Sector checksums are recovered from the image
  rather than hardcoded, because the size the game checksums over follows `sizeof(SaveBlock1)`
  and would otherwise break on any future change to that struct; the recovery is then proved on
  each image by rewriting it unchanged and requiring byte-identical output before any real write
  is attempted.

  **Money now travels on its own** -- the first field to do so. The game reports the value from
  `SetMoney` (the one place `AddMoney` and `RemoveMoney` both end up, and so the only place that
  sees every change), and the server writes it into its own copy through the authoring path.
  Reported values get exactly the checks an uploaded save gets: same caps, same
  no-going-backwards rule, same rate ceiling. A second, laxer set of rules for the direct path
  would just make the direct path the way to cheat.

  **Bag items travel on their own too**, as counts rather than deltas -- a delta that arrives
  twice, or not at all, leaves the bag wrong in a way nothing afterwards can notice. The pocket
  mapping lives in the game rather than the server, because SaveBlock1 orders pockets
  differently from the `POCKET_*` constants and a server guessing that wrong would file items
  into the wrong pocket, which to a player is indistinguishable from losing them.

  **The party travels on its own too**, as the game's own bytes rather than as fields. Each
  Pokemon carries four substructures encrypted with `personality ^ ot_id` and ordered by
  `personality % 24`; re-encoding them server-side means reimplementing both, and that decode has
  already produced one confidently wrong answer in this codebase. Carrying the bytes cannot get
  it wrong and gives up nothing -- it is strictly less than the whole save and meets the same
  level, experience and EV checks. Reported by hashing the party each tick, because unlike money
  and items there is no chokepoint: a Pokemon changes from levelling, evolving, learning a move,
  taking damage, being caught, healed or swapped with the PC.

  **Flags, variables, position, the Pokedex, counters, rematches and berry trees** report as
  allowlisted regions. The allowlist is matched exactly, not by containment: accepting a
  subrange would let a caller write one byte at a time at an offset of its choosing, which is an
  arbitrary write into the save with extra steps. Money, the bag and the party are deliberately
  absent from it -- they have their own messages, carrying caps, rate ceilings and level
  consistency checks a raw region write would walk straight past.

  **Any block can now be authored**, not only SaveBlock1 (`write_block` / `reauthor_block`).
  This was the real blocker: the PC boxes live in their own nine sectors and SaveBlock2 in
  another, so switching the upload off while authoring covered only SaveBlock1 would have meant
  every Pokemon in a player's PC ceasing to reach the server -- gone at the next sign-in, not
  degraded.

  **Still to do before the upload can be deleted**, in order:
  1. The client does not yet *report* PC box or SaveBlock2 contents. The server can write them;
     nothing sends them.
  2. Chunked regions for the large SaveBlock1 fields -- secret bases are 4360 bytes against a
     1024-byte cap, so they need splitting across allowlist entries.
  3. Only then switch the upload off, after running both paths side by side through real play
     and comparing. Retiring it while anything still depends on it to carry a field loses that
     field permanently, with no copy left to restore from.

  And the honest limit: even finished, retiring the upload stops the client *choosing the format*
  it reports in, while the client still computes the contents. Real narrowing of the attack
  surface, not the end of it. Parsing continues, because each parsed field is one the server can
  check rather than merely carry -- and checking is what the headless engine below finishes.
### Replay validation — built, and one design question left

The apparatus exists and is tested: `instances.rs` starts, drives and stops headless instances
(`Instances::start` / `send_input` / `stop`), the game reads a key stream from
`POKEPLANET_INPUT_PIPE`, and `save_parse::diverged` compares a client's account against the
server's own run. Instances are wired to sign-in and disconnect, behind
`POKEPLANET_GAME_BINARY`; unset means the check does not run and every existing rule still does.

**Not yet connected, and one part of it is not merely unwritten:**

1. *Inputs are not routed.* The client does not report the keys it pressed, so there is nothing
   to feed `send_input`. This needs a message carrying key bits per frame — mechanical, but a
   protocol change.

2. *State cannot be read back the obvious way.* An instance is a game client, so the natural
   design is to let it report through the same path any client uses. It cannot: signing in as a
   character that is already online sends `Superseded` (`world.rs:119`) and disconnects the
   player. **A validation instance signing in as the character it is validating would kick that
   character off.**

   So the instance has to be readable *without* signing in. The two candidates are a local
   channel that bypasses sign-in — the instance already talks to a sidecar over loopback, and
   that is the natural place — or a flag that makes an instance report state while claiming no
   session. The first keeps one identity per character, which is why it is preferred.

Until 2 is decided, instances start and stop and do nothing else, which is why this is not
described as working.

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
