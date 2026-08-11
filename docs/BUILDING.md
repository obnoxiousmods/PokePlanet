# Building PokePlanet

How to build the client, the sidecar, and the server. For **what is built and what is planned**,
see [ROADMAP.md](../ROADMAP.md) — this file is only the build steps, so there is one place for
status, not three.

The project has three build targets:

| Target | Toolchain | Output |
|---|---|---|
| Windows client | `i686-w64-mingw32` (32-bit) | `pokeemerald.exe` → renamed `pokeplanet.exe` |
| Linux client | native 32-bit (`HOST_BITS=32`) | `pokeemerald` |
| Server + sidecar | Rust (stable) | `pokeplanet-server`, `pokeplanet-net.exe` |

The client is a 32-bit build in both cases — a constraint inherited from the GBA decompilation.

## Prerequisites

- The pokeemerald build tools' dependencies: `libpng-dev`, `zlib1g-dev`, a C/C++ host compiler.
- For the Windows client: the `i686-w64-mingw32` cross toolchain and its 32-bit `SDL2` (and
  `SDL2_image`).
- For the Linux headless client: 32-bit `gcc`/`g++` (`gcc-multilib`) and 32-bit `SDL2`.
  `SDL2_image` is optional — pass `NO_SDL_IMAGE=1` to skip the decorative border art it loads.
- For the server: a stable Rust toolchain (`rustup`), and PostgreSQL to run against.

The repository is best kept on a native Linux filesystem (e.g. WSL's ext4), not a `/mnt/c`
drvfs mount, which makes the C build several times slower.

## Windows client

```sh
make -f Makefile_pc -j"$(nproc)"        # -> pokeemerald.exe
tools/deploy-windows.sh                 # builds both, renames, and installs to the play folder
```

`deploy-windows.sh` builds the client and the sidecar together (they share a protocol, so
shipping one without the other silently breaks login), renames the client to `pokeplanet.exe`
and `pokeplanet_tester.exe`, and copies the sidecar, the `.bmp` art and `SDL2.dll`. It skips a
byte-identical `SDL2.dll` and reports cleanly if a file is locked because the game is running.

The same binary ships under two names: the game reads its profile from `argv[0]`, so
`pokeplanet.exe` runs the normal account and `pokeplanet_tester.exe` gets its own save, config,
log, token cache and sidecar port — which is what lets you run both at once to test multiplayer
on one machine.

## Linux headless client

```sh
make -f Makefile_pc NATIVE_LINUX=1 NO_SDL_IMAGE=1 rom -j"$(nproc)"   # -> ./pokeemerald
tools/debug/headless-smoke.sh                                        # proves it runs with no display
```

The architecture is explicit. `HOST_BITS=32` is the verified release target; `HOST_BITS=64`
uses a separate `build/linux64` object tree for ongoing Linux/macOS portability work and must
not be shipped until the pointer-width warnings described in `RELEASES.md` are resolved.

This is the build the server runs for replay validation. It runs under `SDL_VIDEODRIVER=dummy`
with no window; the smoke test asserts the game loop actually turns rather than just that the
process started.

## Server and sidecar

```sh
cd server
cargo build --release          # pokeplanet-server, and pokeplanet-net for the host
cargo test --all               # the world, save-parsing, rates and validation suite
cargo clippy --all -- -D warnings
cargo fmt --all -- --check
```

CI runs all of the above plus the whole-game build and the README/ROADMAP identity check, and
`master` is protected on them. Keep your Rust toolchain current — CI's clippy can be stricter
than an older local one.

The server reads its configuration from the environment (see `server/server/src/config.rs`) and
its gameplay rates from a `rates.conf` in its working directory. Secrets (the database URL, the
Discord OAuth credentials) are not in the repo.
