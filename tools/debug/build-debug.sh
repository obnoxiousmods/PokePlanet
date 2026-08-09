#!/bin/bash
# Build the natively debuggable 32-bit Linux binary.
#
#   ASAN=1 tools/debug/build-debug.sh    with AddressSanitizer + UBSan
#          tools/debug/build-debug.sh    plain -g -O0
#
# make tracks timestamps, not compiler flags, so switching sanitizers on or off while
# reusing build/linux mixes instrumented and uninstrumented objects. Linking then fails on
# undefined __asan_*/__ubsan_* symbols, or silently produces a binary with no sanitizer
# runtime at all. A marker file records which mode the tree was built in and forces a wipe
# only when it actually changes.
set -euo pipefail
cd ~/src/PokePlanet
export PKG_CONFIG_PATH=/usr/lib32/pkgconfig:${PKG_CONFIG_PATH:-}

MODE="plain"
[ "${ASAN:-0}" = "1" ] && MODE="asan"
MARKER=build/linux/.build-mode

if [ -f "$MARKER" ] && [ "$(cat "$MARKER")" != "$MODE" ]; then
    echo "build mode changed ($(cat "$MARKER") -> $MODE); wiping build/linux"
    rm -rf build/linux pokeemerald
fi

make -f Makefile_pc linux NO_SDL_IMAGE=1 DINFO=1 ASAN="${ASAN:-0}" -j"$(nproc)" "$@"

mkdir -p build/linux && echo "$MODE" > "$MARKER"
echo "built in $MODE mode; asan symbols linked: $(ldd pokeemerald 2>/dev/null | grep -c asan)"
