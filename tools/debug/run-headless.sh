#!/bin/bash
# Run the debug build headless with scripted input, optionally under gdb.
#
#   tools/debug/run-headless.sh "enter,x,a,a,a"
#   GDB=1 tools/debug/run-headless.sh "enter,x,a" -ex 'break CB2_NewGame' -ex run
#
# No desktop and no screenshots: this is how the client should be exercised.
set -euo pipefail
cd "$(dirname "$0")/../.."
export SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy
export POKEPLANET_AUTOKEYS="${1:-}"
export POKEPLANET_AUTOKEY_FRAMES="${AUTOKEY_FRAMES:-25}"
shift || true
if [ -n "${GDB:-}" ]; then
    exec gdb -batch -nx -q -ex 'set confirm off' -ex 'set pagination off' "$@" --args ./pokeemerald
fi
exec ./pokeemerald "$@"
