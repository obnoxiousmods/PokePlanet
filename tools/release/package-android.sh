#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/common.sh"

APK=${APK:-"$ROOT/android/app/build/outputs/apk/release/app-release-unsigned.apk"}
AAB=${AAB:-"$ROOT/android/app/build/outputs/bundle/release/app-release.aab"}
[[ -f "$APK" ]] || { echo "missing Android APK: $APK" >&2; exit 1; }
cp "$APK" "$DIST/PokePlanet-$VERSION-android-arm.apk"
if [[ -f "$AAB" ]]; then
  cp "$AAB" "$DIST/PokePlanet-$VERSION-android-arm.aab"
fi
