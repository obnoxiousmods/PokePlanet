#!/bin/bash
# Build the native target with AddressSanitizer and UBSan.
#
# Note the clean: make tracks file timestamps, not compiler flags, so switching
# sanitizers on or off without wiping build/linux silently reuses objects built the
# other way and the runtime never gets linked in.
set -euo pipefail
cd "$(dirname "$0")/../.."
rm -rf build/linux pokeemerald
export PKG_CONFIG_PATH=/usr/lib32/pkgconfig:${PKG_CONFIG_PATH:-}
make -f Makefile_pc linux NO_SDL_IMAGE=1 DINFO=1 ASAN=1 -j"$(nproc)" "$@"
echo "asan linked: $(ldd pokeemerald | grep -c asan) entries"
