#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/common.sh"

GAME=${GAME:-"$ROOT/pokeemerald.exe"}
SIDECAR=${SIDECAR:-"$ROOT/server/target/x86_64-pc-windows-gnu/release/pokeplanet-net.exe"}
SDL_DLL=${SDL_DLL:-/usr/i686-w64-mingw32/bin/SDL2.dll}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
bundle="$stage/PokePlanet-$VERSION-windows-x86"

for file in "$GAME" "$SIDECAR" "$SDL_DLL"; do
  [[ -f "$file" ]] || { echo "missing release input: $file" >&2; exit 1; }
done
copy_common_assets "$bundle"
cp "$GAME" "$bundle/pokeplanet.exe"
cp "$SIDECAR" "$bundle/pokeplanet-net.exe"
cp "$SDL_DLL" "$bundle/SDL2.dll"
cp "$ROOT"/*.bmp "$bundle/"

(cd "$stage" && zip -q -9 -r "$DIST/PokePlanet-$VERSION-windows-x86.zip" "$(basename "$bundle")")
if command -v 7z >/dev/null; then
  7z a -bd -mx=9 "$DIST/PokePlanet-$VERSION-windows-x86.7z" "$bundle" >/dev/null
fi
