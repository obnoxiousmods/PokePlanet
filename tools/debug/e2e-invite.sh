#!/bin/bash
# End-to-end battle invitation test, entirely headless in WSL. Both directions:
#
#   outgoing: the game walks up to the ghost, presses A, answers YES, and the ghost -- which
#             auto-accepts -- answers back, so the game shows "... accepted your challenge!"
#   incoming: with --incoming, the ghost challenges the game instead, and the game's own
#             prompt is answered by the scripted input
#
# Tokens. The game side needs a real session token for the account under test. Put one in
# ~/.pokeplanet-test-token or POKEPLANET_TEST_TOKEN; it is deliberately not in the repo. The
# ghost uses a fixed development token for the Tester character, which exists purely for this.
set -euo pipefail
cd ~/src/PokePlanet
export PATH="$HOME/.cargo/bin:$PATH"

TESTER_TOKEN="${POKEPLANET_TESTER_TOKEN:-testertoken-for-local-testing-00000000001}"
TOKEN_FILE="$HOME/.pokeplanet-test-token"
PLAYER_TOKEN="${POKEPLANET_TEST_TOKEN:-}"
if [ -z "$PLAYER_TOKEN" ] && [ -f "$TOKEN_FILE" ]; then
    PLAYER_TOKEN="$(tr -d '[:space:]' < "$TOKEN_FILE")"
fi
if [ -z "$PLAYER_TOKEN" ]; then
    echo "no token for the account under test." >&2
    echo "put one in $TOKEN_FILE, or set POKEPLANET_TEST_TOKEN." >&2
    exit 1
fi

INCOMING=0
[ "${1:-}" = "--incoming" ] && INCOMING=1

# Kill by pid, never by pattern: a pattern broad enough to match these also matches the
# shell running this script, which then kills itself halfway through setup.
PIDS=()
cleanup() {
    for pid in ${PIDS+"${PIDS[@]}"}; do
        kill -9 "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

printf '{\n  "token": "%s"\n}\n' "$PLAYER_TOKEN" > /tmp/e2e-player-auth.json

./server/target/debug/pokeplanet-net --token /tmp/e2e-player-auth.json \
    --ipc-port 39400 --log /tmp/e2e-sidecar.log < /dev/null > /dev/null 2>&1 &
PIDS+=($!)
sleep 6

PLAYER_ID=$(grep -ao 'player_id=[0-9]*' /tmp/e2e-sidecar.log | head -1 | cut -d= -f2)
if [ -z "$PLAYER_ID" ]; then
    echo "the sidecar never signed in; see /tmp/e2e-sidecar.log" >&2
    exit 1
fi
echo "signed in as player $PLAYER_ID"

# Stand the ghost on the tile the player is facing, rather than assuming a spawn point.
# Facing: 1 south, 2 north, 3 west, 4 east.
POSE=$(ssh -o BatchMode=yes lucy "sudo -u postgres psql -t -A -F, -d pokeplanet \
    -c \"select map_group,map_num,pos_x,pos_y,facing from characters where id=$PLAYER_ID;\"" 2>/dev/null)
IFS=, read -r MG MN PX PY FACING <<< "$POSE"
case "$FACING" in
    2) GX=$PX; GY=$((PY - 1));;
    3) GX=$((PX - 1)); GY=$PY;;
    4) GX=$((PX + 1)); GY=$PY;;
    *) GX=$PX; GY=$((PY + 1));;
esac
echo "player at $MG:$MN ($PX,$PY) facing $FACING; ghost at ($GX,$GY)"

GHOST_ARGS=(--token "$TESTER_TOKEN" --map "$MG:$MN" --at "$GX,$GY" --still)
[ "$INCOMING" = "1" ] && GHOST_ARGS+=(--challenge "$PLAYER_ID")

./server/target/debug/ghost "${GHOST_ARGS[@]}" < /dev/null > /tmp/e2e-ghost.log 2>&1 &
PIDS+=($!)
sleep 3

export SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy
# Menu, then A repeatedly: talks to the ghost, answers YES, and dismisses the result.
export POKEPLANET_AUTOKEYS=enter,enter,a,a,a,a,a,a,a,a,a,a,a,a,a,a
export POKEPLANET_AUTOKEY_FRAMES=55
timeout 70 ./pokeemerald > /tmp/e2e-game.log 2>&1 || true

echo
echo "=== ghost ==="
grep -aE "signed in|challenge sent|challenged; accepting|accepted challenge|Answered" /tmp/e2e-ghost.log | tail -6
echo "=== sidecar ==="
grep -aiE "signed in|invitation" /tmp/e2e-sidecar.log | tail -6

# The whole point is that one of these lines exists.
if [ "$INCOMING" = "1" ]; then
    grep -aq "invitation received" /tmp/e2e-sidecar.log \
        && echo "PASS: the challenge reached the game" \
        || { echo "FAIL: the game was never told it was challenged" >&2; exit 1; }
else
    grep -aq "invitation answered" /tmp/e2e-sidecar.log \
        && echo "PASS: the answer came back to the challenger" \
        || { echo "FAIL: no answer reached the challenger" >&2; exit 1; }
fi
