#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/common.sh"

GAME=${GAME:-"$ROOT/pokeplanet-macos"}
SIDECAR=${SIDECAR:-"$ROOT/server/target/release/pokeplanet-net"}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
app="$stage/PokePlanet.app"
contents="$app/Contents"

for file in "$GAME" "$SIDECAR"; do
  [[ -f "$file" ]] || { echo "missing release input: $file" >&2; exit 1; }
done
install -d "$contents/MacOS" "$contents/Resources"
install -m755 "$GAME" "$contents/MacOS/pokeplanet"
install -m755 "$SIDECAR" "$contents/MacOS/pokeplanet-net"
cp "$ROOT"/BG*.png "$ROOT/Border.png" "$contents/Resources/"
sed "s/@VERSION@/$VERSION/g" "$ROOT/packaging/macos/Info.plist.in" > "$contents/Info.plist"
codesign --force --deep --sign - "$app"
ditto -c -k --sequesterRsrc --keepParent "$app" "$DIST/PokePlanet-$VERSION-macos-universal.zip"
hdiutil create -quiet -volname PokePlanet -srcfolder "$app" -ov -format UDZO "$DIST/PokePlanet-$VERSION-macos-universal.dmg"
