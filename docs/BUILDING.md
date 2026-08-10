# PokePlanet

A massively-multiplayer Pokémon Emerald, built on the [pret decompilation][pret] and the
[pokeemerald-multiplatform][upstream] native port.

PokePlanet runs the decompiled game code directly — it is not an emulator and ships no
commercial ROM or game assets. What it adds is a real MMO spine: Discord sign-in, a QUIC
server, a shared overworld where you see other trainers walking around, and server-held
progression.

> **Status: early.** Sign-in, the shared overworld and other visible players work today.
> Chat UI, server-authoritative saves and player-vs-player battles are in progress. See
> [Roadmap](#roadmap).

---

## How it fits together

```
┌──────────────────┐   loopback    ┌──────────────────┐    QUIC/TLS    ┌──────────────────┐
│  pokeemerald.exe │◄─────────────►│ pokeplanet-net   │◄──────────────►│ pokeplanet-server│
│  32-bit, SDL2    │  fixed-layout │ sidecar, 64-bit  │    UDP 4433    │  Rust, on lucy   │
│  the game itself │    frames     │ QUIC · TLS · auth│                │                  │
└──────────────────┘               └──────────────────┘                └────────┬─────────┘
                                                                                │
                                                          ┌─────────────────────┼──────────────┐
                                                     PostgreSQL              Valkey        Solanum IRC
                                                   characters, party,      presence,      #pokeplanet
                                                   inventory, story        sessions        chat bridge
```

**Why a sidecar?** The game is a 32-bit binary whose C is run through the decomp's
charmap preprocessor. Linking QUIC and TLS into it would be painful and fragile. Instead a
separate 64-bit Rust process owns the network and speaks a tiny fixed-layout protocol to the
game over loopback, so the game itself needs nothing beyond winsock. It also means the
netcode can be iterated without a full game rebuild.

### Repository layout

| Path | What it is |
| --- | --- |
| `src/`, `include/`, `data/` | The game. Upstream decomp plus PokePlanet's additions. |
| `src/mmo_players.c` | Other players, drawn as ordinary overworld object events. |
| `src/platform/net_client.c` | Winsock link to the sidecar, on its own thread. |
| `src/mmo_text.c` | Converts network text into the game's own charmap. |
| `server/proto/` | Wire protocol shared by server and sidecar. |
| `server/server/` | The authoritative server. |
| `server/net/` | The client sidecar, plus the `ghost` headless test player. |

---

## Playing

Download a release, unzip it, and run `pokeemerald.exe`. The sidecar starts automatically.

On first launch you will be asked to sign in with Discord; a browser opens, you approve, and
the game picks it up. The token is cached, so later launches sign in silently.

Signing in is optional — press **B** at the prompt to play offline single-player.

### Controls

| GBA | Key |
| --- | --- |
| A / B | `Z` / `X` |
| Start / Select | `Enter` / `\` |
| L / R | `A` / `S` |
| D-pad | Arrow keys |
| Fast-forward | `Space` |
| Pause / Soft reset | `Ctrl+P` / `Ctrl+R` |

XInput controllers work through the SDL2 backend.

### Configuration

`pokeemerald.cfg` sits next to the executable and is shared by the game and the sidecar:

```ini
server=pokeplanet.obby.ca
serverPort=4433
sidecarPort=38400
```

Point `server` elsewhere to play on a different server. Display settings live in the same
file and are written back by the in-game **Options → Display** page.

---

## Building

The Windows client is cross-compiled from Linux (or WSL). You need a 32-bit MinGW
toolchain, the SDL2 MinGW development tree, ImageMagick, and a host C toolchain with libpng.

On Arch:

```sh
pacman -S --needed base-devel mingw-w64-gcc mingw-w64-binutils libpng imagemagick
# SDL2 for the i686 target, from libsdl.org's MinGW development tarball
curl -LO https://github.com/libsdl-org/SDL/releases/download/release-2.30.7/SDL2-devel-2.30.7-mingw.tar.gz
tar xzf SDL2-devel-2.30.7-mingw.tar.gz
sudo cp -a SDL2-2.30.7/i686-w64-mingw32/include/SDL2 /usr/i686-w64-mingw32/include/
sudo cp -a SDL2-2.30.7/i686-w64-mingw32/lib/.      /usr/i686-w64-mingw32/lib/
sudo cp -a SDL2-2.30.7/i686-w64-mingw32/bin/.      /usr/i686-w64-mingw32/bin/
```

Then:

```sh
make -f Makefile_pc -j$(nproc)          # → pokeemerald.exe
```

Ship `pokeemerald.exe` alongside `SDL2.dll` (the 32-bit one), `Border.bmp` and `BG*.bmp`.

### Server and sidecar

```sh
cd server
cargo build --release                                     # server, for this host
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release -p pokeplanet-net --target x86_64-pc-windows-gnu
cargo test                                                # 13 tests
```

The server reads its configuration from the environment; see `server/server/src/config.rs`.
Secrets belong in a root-only systemd `EnvironmentFile`, never the repository.

### Testing multiplayer without a second account

`ghost` is a headless player that signs in with an existing session token and walks a loop,
printing every snapshot it receives:

```sh
cargo run -p pokeplanet-net --bin ghost -- \
  --token <session-token> --map 0:9 --at 18,17
```

---

## Roadmap

| | Status |
| --- | --- |
| Discord OAuth2 sign-in, session tokens | **Done** |
| QUIC transport, presence, reconnection | **Done** |
| Other players visible and animated in the overworld | **Done** |
| Random overworld sprite per character | **Done** |
| Server-held save summary on the sign-in screen | **Done** |
| Server-side chat routing + IRC bridge | **Done** (no in-game UI yet) |
| In-game chat: global, per-map, private | In progress |
| Friends, PMs and battle invitations in the PokéNav | In progress |
| Server-authoritative saves — no local save file | Planned |
| Player-vs-player battles, simulated server-side | Planned |

The full architectural plan, including why server-side battles reuse the game's own engine
rather than reimplementing it, is tracked in the project's masterplan.

---

## Design notes

A few decisions worth knowing if you are reading the code.

**Other players are ordinary object events.** They spawn through
`SpawnSpecialObjectEventParameterized` with reserved local IDs from 200 up, so they inherit
sprites, elevation, reflections and walk animations for free. A one-tile move is played as a
real walk; anything larger snaps.

**Movement rides QUIC datagrams, not streams.** A dropped position is superseded 100ms
later, so retransmitting it would only add latency. Control traffic — auth, chat, invitations
— uses a reliable stream.

**Two encodings, on purpose.** Server↔sidecar is bincode and free to evolve.
Sidecar↔game is fixed-layout little-endian, so the 32-bit C side can read a record without a
parser or an allocator.

**Presence is keyed by connection, not character.** A player can briefly hold two
connections during a reconnect; without session epochs the older one's teardown would evict
the live session and silently stop its updates.

---

## Credits and legal

Built on [pret/pokeemerald][pret] and [gradenGnostic/pokeemerald-multiplatform][upstream].
The enormous work of decompiling and porting the game belongs to those projects.

Pokémon and Pokémon Emerald are trademarks of Nintendo, Creatures Inc. and GAME FREAK inc.
This is an unofficial fan project, not affiliated with or endorsed by them. No copyrighted
game assets or ROM data are distributed here; you supply your own. The scoped licence in
[LICENSE](LICENSE) covers only original modifications contributed through this fork and does
not relicense upstream code or third-party components.

[pret]: https://github.com/pret/pokeemerald
[upstream]: https://github.com/gradenGnostic/pokeemerald-multiplatform
