#!/bin/bash
# End-to-end battle invitation test, entirely headless in WSL.
#
#   ghost (player 2) parks next to spawn
#   headless game (player 1) signs in via its own sidecar, walks to the ghost, presses A,
#   answers YES to the battle prompt
#   the ghost's log should show the invitation arriving
set -euo pipefail
cd ~/src/PokePlanet
export PATH="$HOME/.cargo/bin:$PATH"

pkill -f 'target/debug/ghost' 2>/dev/null || true
pkill -f 'target/debug/pokeplanet-net' 2>/dev/null || true
sleep 1

# Ghost stands one tile south of the spawn point so the player can face it.
RUST_LOG=info ./server/target/debug/ghost \
    --token ghosttoken-for-local-testing-000000000001 \
    --map 0:9 --at 17,19 --still > /tmp/ghost-invite.log 2>&1 &
GHOST=$!
sleep 3

RUST_LOG=info ./server/target/debug/pokeplanet-net > /tmp/sidecar-invite.log 2>&1 &
SIDE=$!
sleep 4

export SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy
# title -> gate (already signed in, falls through) -> CONTINUE -> walk down -> A -> YES
export POKEPLANET_AUTOKEYS=enter,a,a,down,right,a,a,a
export POKEPLANET_AUTOKEY_FRAMES=90
timeout 150 ./pokeemerald > /tmp/game-invite.log 2>&1 || true

kill $GHOST $SIDE 2>/dev/null || true

echo "=== sidecar (player 1) ==="
grep -aE "signed in|battle|invitation" /tmp/sidecar-invite.log | tail -6
echo "=== server-side invitation ==="
ssh -o BatchMode=yes lucy "sudo journalctl -u pokeplanet --since \"3 minutes ago\" --no-pager | grep -iE \"invit|battle\" | tail -4" 2>/dev/null || true
echo "=== ghost (player 2) ==="
grep -aE "signed in|Battle|invitation|sees" /tmp/ghost-invite.log | tail -6
