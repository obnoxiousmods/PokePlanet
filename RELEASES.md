# PokePlanet releases

PokePlanet ships the native game and the matching Rust network sidecar together. Mixing a game
from one commit with a sidecar from another can break sign-in or multiplayer, so loose binaries
are never release artifacts.

## Release gate

A platform is published only after all of the following pass on its native runner:

1. the game and `pokeplanet-net` build from the tagged commit;
2. the archive contains both binaries and required SDL/art assets;
3. the game process reaches its frame loop;
4. the sidecar can connect to the configured server and begin Discord authentication;
5. the artifact hash appears in `SHA256SUMS` and `release-manifest.json`.

The website consumes GitHub Releases and will not show a platform merely because it appears in
this document.

## Current platform state

- **Windows:** native 32-bit SDL2 game and 64-bit sidecar; ZIP and 7z packaging are automated.
- **Linux:** native 32-bit SDL2 game and 64-bit sidecar; portable ZIP and tar.zst packaging are automated.
- **macOS:** app/DMG packaging and native sidecar browser launching are implemented. The game is
  not publishable until its GBA-era 32-bit pointer encoding is made 64-bit safe and the generated
  assembly is emitted as Mach-O. Modern macOS cannot run the existing 32-bit binary.
- **Android:** the existing SDL2 client produces APK/AAB artifacts. Multiplayer release remains
  gated on hosting the Rust sidecar inside the application process because Android applications
  cannot launch it as a neighboring executable.

The first coordinated version is `0.1.0-alpha.1`. Unsigned alpha builds include clear platform
security instructions; code signing is enabled later through CI secrets without changing the
archive contract.
