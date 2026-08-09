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
    if ! cp -v "$1" "$2" 2>/dev/null; then
        echo "could not replace $(basename "$1"): it is most likely still running." >&2
        echo "close the game and the sidecar, then run this again." >&2
        return 1
    fi
}

failed=0
copy_or_explain "$SRC/pokeemerald.exe" "$DEST/" || failed=1
copy_or_explain "$SRC/server/target/x86_64-pc-windows-gnu/release/pokeplanet-net.exe" "$DEST/" || failed=1
cp -v "$SRC"/*.bmp "$DEST/"
cp -v /usr/i686-w64-mingw32/bin/SDL2.dll "$DEST/"
[ "$failed" -eq 0 ] || exit 1

echo "== deployed =="
ls -la --time-style=+%H:%M "$DEST"/pokeemerald.exe "$DEST"/pokeplanet-net.exe
