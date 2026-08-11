#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
VERSION=$(tr -d '[:space:]' < "$ROOT/VERSION")
DIST=${DIST:-"$ROOT/dist"}

if [[ -z "$VERSION" || ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "VERSION is missing or not SemVer: $VERSION" >&2
  exit 1
fi

mkdir -p "$DIST"

copy_common_assets() {
  local target=$1
  install -d "$target"
  cp "$ROOT/LICENSE" "$ROOT/README.md" "$ROOT/docs/BUILDING.md" "$target/"
}
