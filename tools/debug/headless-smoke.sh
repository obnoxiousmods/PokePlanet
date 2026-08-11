#!/usr/bin/env bash
# Does the game's logic actually run with no display?
#
# "The process started" is not the same claim as "the game is running", and the difference
# matters here: SDL will happily create a software renderer and sit there while AgbMain never
# gets going. So this asserts on the game's own callbacks rather than on the process existing.
#
# AgbMain is the game thread's entry point; CB2_InitCopyrightScreenAfterBootup is the first
# callback the main loop dispatches. Reaching the second one means the loop is turning, not
# merely that the binary loaded.
#
# Build first:
#   make -f Makefile_pc NATIVE_LINUX=1 NO_SDL_IMAGE=1 rom -j8
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

if [ ! -x ./pokeemerald ]; then
    echo "no ./pokeemerald -- build it with:"
    echo "  make -f Makefile_pc NATIVE_LINUX=1 NO_SDL_IMAGE=1 rom -j8"
    exit 1
fi

out=$(SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy timeout 60 gdb -q -batch \
    -ex "set pagination off" -ex "set confirm off" \
    -ex "break AgbMain" \
    -ex "break CB2_InitCopyrightScreenAfterBootup" \
    -ex "run" -ex "continue" \
    ./pokeemerald 2>&1)

fail=0
for want in "AgbMain ()" "CB2_InitCopyrightScreenAfterBootup ()"; do
    if grep -aq "Breakpoint .*${want}" <<<"$out"; then
        echo "ok    reached ${want%% (*}"
    else
        echo "FAIL  never reached ${want%% (*}"
        fail=1
    fi
done

if [ "$fail" -ne 0 ]; then
    echo
    echo "--- gdb output ---"
    grep -aE "Breakpoint|Program|signal|error" <<<"$out" | head -20
fi

exit "$fail"
