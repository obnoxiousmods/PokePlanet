#!/bin/bash
# Two real clients, one battle, headless.
#
# The ghost cannot be used for this: it does not run the battle engine, so it can accept a
# challenge and never exchange a single controller message. Proving a battle works needs two
# actual games.
#
# Ports are deliberately in a range of their own. WSL forwards localhost, so a test sidecar on
# the default port is reachable from a real client on Windows -- which once attached to one,
# signed in as the wrong character, and was pulled into a battle that was not its own.
set -uo pipefail

SRC="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$SRC" || exit 1

A_PORT=39400
B_PORT=39401

pkill -9 -x pokeplanet-net 2>/dev/null
pkill -9 -x pokeemerald 2>/dev/null
pkill -9 -x pokeemerald_tester 2>/dev/null
sleep 1

PLAYER_TOKEN="$(tr -d '[:space:]' < "$HOME/.pokeplanet-test-token" 2>/dev/null)"
TESTER_TOKEN="${POKEPLANET_TESTER_TOKEN:-testertoken-for-local-testing-00000000001}"
if [ -z "$PLAYER_TOKEN" ]; then
    echo "no token for the challenger; put one in ~/.pokeplanet-test-token" >&2
    exit 1
fi

printf '{\n  "token": "%s"\n}\n' "$PLAYER_TOKEN" > /tmp/two-a.json
printf '{\n  "token": "%s"\n}\n' "$TESTER_TOKEN" > /tmp/two-b.json

PIDS=()
cleanup() { for p in ${PIDS+"${PIDS[@]}"}; do kill -9 "$p" 2>/dev/null; done; }
trap cleanup EXIT

# A second binary so the two clients get different profiles: everything after the first
# underscore in argv[0] names the profile, which is the whole configuration step.
cp -f pokeemerald pokeemerald_tester

./server/target/debug/pokeplanet-net --token /tmp/two-a.json --ipc-port "$A_PORT" \
    --log /tmp/two-a.log < /dev/null > /dev/null 2>&1 &
PIDS+=($!)
./server/target/debug/pokeplanet-net --token /tmp/two-b.json --fixed-token --ipc-port "$B_PORT" \
    --log /tmp/two-b.log < /dev/null > /dev/null 2>&1 &
PIDS+=($!)
sleep 8

A_ID=$(grep -ao 'player_id=[0-9]*' /tmp/two-a.log | head -1 | cut -d= -f2)
B_ID=$(grep -ao 'player_id=[0-9]*' /tmp/two-b.log | head -1 | cut -d= -f2)
echo "challenger=$A_ID  responder=$B_ID"
if [ -z "$A_ID" ] || [ -z "$B_ID" ]; then
    echo "a sidecar never signed in; see /tmp/two-a.log and /tmp/two-b.log" >&2
    exit 1
fi

# Stand them next to each other so both are in the same world view.
ssh -o BatchMode=yes lucy "sudo -u postgres psql -q -d pokeplanet \
  -c \"update characters set map_group=0,map_num=9,pos_x=18,pos_y=24,facing=3 where id=$A_ID;\" \
  -c \"update characters set map_group=0,map_num=9,pos_x=17,pos_y=24,facing=4 where id=$B_ID;\"" \
  >/dev/null 2>&1

export SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy
export POKEPLANET_AUTOKEYS=enter,enter,a,a,a,a,a,a,a,a,a,a,a,a,a,a,a,a
export POKEPLANET_AUTOKEY_FRAMES=55

# The responder starts first so it is already in the world when the challenge arrives.
POKEPLANET_SIDECAR_PORT="$B_PORT" timeout 150 gdb -batch -nx -q \
    -x tools/debug/battle-stages.gdb --args ./pokeemerald_tester > /tmp/two-game-b.txt 2>&1 &
PIDS+=($!)
sleep 5
POKEPLANET_SIDECAR_PORT="$A_PORT" timeout 150 gdb -batch -nx -q \
    -x tools/debug/battle-stages-challenger.gdb --args ./pokeemerald > /tmp/two-game-a.txt 2>&1 &
PIDS+=($!)

wait %3 %4 2>/dev/null

echo "=== challenger ==="
grep -aE '^###' /tmp/two-game-a.txt || echo "(nothing -- see /tmp/two-game-a.txt)"
echo "=== responder ==="
grep -aE '^###' /tmp/two-game-b.txt || echo "(nothing -- see /tmp/two-game-b.txt)"
