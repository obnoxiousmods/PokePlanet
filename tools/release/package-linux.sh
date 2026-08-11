#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/common.sh"

GAME=${GAME:-"$ROOT/pokeemerald"}
SIDECAR=${SIDECAR:-"$ROOT/server/target/release/pokeplanet-net"}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
bundle="$stage/PokePlanet-$VERSION-linux-x86"

for file in "$GAME" "$SIDECAR"; do
  [[ -f "$file" ]] || { echo "missing release input: $file" >&2; exit 1; }
done
copy_common_assets "$bundle"
install -m755 "$GAME" "$bundle/pokeplanet-bin"
install -m755 "$SIDECAR" "$bundle/pokeplanet-net"
install -m755 "$ROOT/packaging/linux/pokeplanet" "$bundle/pokeplanet"
cp "$ROOT"/BG*.png "$ROOT/Border.png" "$bundle/"

(cd "$stage" && tar --zstd -cf "$DIST/PokePlanet-$VERSION-linux-x86.tar.zst" "$(basename "$bundle")")
(cd "$stage" && zip -q -9 -r "$DIST/PokePlanet-$VERSION-linux-x86.zip" "$(basename "$bundle")")
