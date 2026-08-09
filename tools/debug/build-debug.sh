#!/bin/bash
# Build the natively debuggable 32-bit Linux binary: -g -O0, no SDL2_image.
# Produces ./pokeemerald, runnable under gdb with real breakpoints and symbols.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PKG_CONFIG_PATH=/usr/lib32/pkgconfig:${PKG_CONFIG_PATH:-}
exec make -f Makefile_pc linux NO_SDL_IMAGE=1 DINFO=1 -j"$(nproc)" "$@"
