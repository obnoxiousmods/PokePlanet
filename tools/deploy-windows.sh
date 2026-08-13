#!/bin/bash
# Deploy the full client: game AND sidecar.
#
# These two speak a shared protocol, so shipping one without the other silently breaks
# login the moment a message changes shape. Deploying them together is the whole point.
#
#   tools/deploy-windows.sh              build both, then deploy
#   NO_BUILD=1 tools/deploy-windows.sh   deploy whatever is already built
set -euo pipefail
SRC="$HOME/src/PokePlanet"
DEST="${1:-/mnt/c/Users/a/PokePlanet}"
mkdir -p "$DEST"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc

if [ "${NO_BUILD:-0}" != "1" ]; then
    echo "== building game =="
    ( cd "$SRC" && make -f Makefile_pc -j"$(nproc)" > /tmp/deploy-game.log 2>&1 ) \
        || { tail -20 /tmp/deploy-game.log; exit 1; }
fi

echo "== building sidecar =="
( cd "$SRC/server" && cargo build -q -p pokeplanet-net --bin pokeplanet-net \
      --release --target x86_64-pc-windows-gnu )

# Windows locks a running executable, so copying over one fails with a bare "Permission
# denied" that reads like a filesystem problem rather than "close the game first".
copy_or_explain() {
    # Not cp -v: it announces the copy before attempting it, so a failure prints a line
    # claiming the file was replaced immediately above the error saying it was not.
    if cp "$1" "$2" 2>/dev/null; then
        echo "  $(basename "$1") -> $2"
    else
        echo "could not replace $(basename "$2"): it is most likely still running." >&2
        echo "close the game and the sidecar, then run this again." >&2
        return 1
    fi
}

# One binary. The world (Normal or Deadman) is chosen at the main menu's two-save picker every
# launch, so there is no longer a separate Deadman executable. The tester copy is still shipped:
# the game reads its profile from argv[0], so pokeplanet_tester.exe gets its own save, config, log,
# token cache and sidecar port, which is what makes running two clients for multiplayer testing work.
failed=0
copy_or_explain "$SRC/pokeemerald.exe" "$DEST/pokeplanet.exe" || failed=1
copy_or_explain "$SRC/pokeemerald.exe" "$DEST/pokeplanet_tester.exe" || failed=1
copy_or_explain "$SRC/server/target/x86_64-pc-windows-gnu/release/pokeplanet-net.exe" "$DEST/" || failed=1
# The old dedicated Deadman binary is obsolete now that the menu picks the world; remove it so
# players do not launch a stale build.
rm -f "$DEST/pokeplanet-deadmon.exe" "$DEST/pokeemerald-deadmon.cfg" 2>/dev/null || true
# These were plain `cp -v`, which under `set -e` aborted the whole script the instant a file
# was locked -- before the friendly "close the game" summary, on the one failure that actually
# happens. Route them through the same handler as the binaries.
for bmp in "$SRC"/*.bmp; do
    copy_or_explain "$bmp" "$DEST/" || failed=1
done

# Skip SDL2.dll when it is already the exact same file: it changes almost never, and copying
# it is the step most likely to be blocked (Windows holds the DLL of a running process), so
# re-copying an identical DLL would fail the deploy for no reason.
SDL_SRC=/usr/i686-w64-mingw32/bin/SDL2.dll
if ! cmp -s "$SDL_SRC" "$DEST/SDL2.dll"; then
    copy_or_explain "$SDL_SRC" "$DEST/" || failed=1
else
    echo "  SDL2.dll unchanged, skipping"
fi

[ "$failed" -eq 0 ] || exit 1

echo "== deployed =="
ls -la --time-style=+%H:%M "$DEST"/pokeplanet.exe "$DEST"/pokeplanet_tester.exe \
    "$DEST"/pokeplanet-net.exe
