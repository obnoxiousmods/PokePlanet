# Contributing to PokePlanet

PokePlanet turns the [pokeemerald](https://github.com/pret/pokeemerald) decompilation (via the
`pokeemerald-multiplatform` SDL2 port) into a server-authoritative MMO. The bar is: it should
feel like Nintendo built an Emerald MMO for PC — polished, authentic, no pay-to-win.

## The one rule that shapes everything

**The server owns the state; the client only requests.** A client asks to move, to spend, to
gain — and the server decides whether that is legitimate and records it. Any change that lets the
client *tell* the server what its state is, rather than *ask* to change it within the rules, is
working against the whole design. If you are adding something a player can gain, ask "what stops
a modified client claiming it for free?" before writing the client side.

## Repository layout

- `server/server/` — the QUIC game server: sign-in, world, save parsing/authoring, validation.
- `server/net/` — the sidecar that bridges the game to the server over loopback.
- `server/proto/` — the wire protocol shared by both.
- `src/`, `include/`, `data/`, `graphics/` — the game itself (the decomp, plus `src/mmo_*.c` and
  `src/platform/` for the port and networking).
- `tools/` — build tools, the collision/questflag extractors, and debug harnesses.

## Building and testing

- **Server:** `cd server && cargo test --all`. CI enforces `cargo clippy --all -- -D warnings`
  and `cargo fmt --all -- --check` — run both before pushing (CI's clippy can be stricter than an
  older local one, so keep your toolchain current).
- **Game (Windows):** `make -f Makefile_pc` → `pokeemerald.exe`; `tools/deploy-windows.sh` renames
  and installs it.
- **Game (Linux headless):** `make -f Makefile_pc NATIVE_LINUX=1 NO_SDL_IMAGE=1 rom`, checked by
  `tools/debug/headless-smoke.sh`.

CI must be green. It builds the whole game, runs the server suite, checks formatting and lints,
and verifies `README.md` and `ROADMAP.md` are byte-identical.

## Expectations for a change

- **Validate server-side, with a negative control.** A test that a cheat is refused is only
  half; also test that honest play is *not* refused — several changes here have compiled, read
  correctly, and been wrong, and two would have locked players out of their own characters.
- **Anti-cheat rules must not strand honest players.** Prefer deriving a rule from the game's own
  behaviour over hand-listing it. Where a rule cannot be proven complete, log first and enforce
  only once real play shows it quiet.
- **Keep `README.md` and `ROADMAP.md` identical** — update both in the same commit.
- **Commit messages explain *why*.** The history is the design record; match its style.

## Reporting bugs

Use the bug report template. For anything that lets a client forge state or affect another
player, follow [SECURITY.md](SECURITY.md) and report privately first.
