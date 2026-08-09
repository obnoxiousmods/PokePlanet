#!/bin/bash
# Deploy the full client: game AND sidecar.
#
# These two speak a shared protocol, so shipping one without the other silently breaks
# login the moment a message changes shape. Deploying them together is the whole point.
set -euo pipefail
SRC="$HOME/src/PokePlanet"
DEST="${1:-/mnt/c/Users/a/PokePlanet}"
mkdir -p "$DEST"

echo "== building sidecar =="
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
( cd "$SRC/server" && cargo build -q -p pokeplanet-net --bin pokeplanet-net \
      --release --target x86_64-pc-windows-gnu )

cp -v "$SRC/pokeemerald.exe" "$DEST/"
cp -v "$SRC"/*.bmp "$DEST/"
cp -v /usr/i686-w64-mingw32/bin/SDL2.dll "$DEST/"
cp -v "$SRC/server/target/x86_64-pc-windows-gnu/release/pokeplanet-net.exe" "$DEST/"

echo "== deployed =="
ls -la --time-style=+%H:%M "$DEST"/pokeemerald.exe "$DEST"/pokeplanet-net.exe
