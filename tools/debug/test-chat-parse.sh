#!/bin/bash
# Compile and run the chat scope tests on the host. Takes about a second.
set -euo pipefail
SRC="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# -iquote, not -I: the game has an include/strings.h, and glibc's <string.h> pulls in
# <strings.h>. On the normal include path the game's would answer that and the build would
# fail somewhere baffling. -iquote applies only to "quoted" includes, which is all we need.
gcc -std=gnu99 -Wall -Wextra -Werror -O1 \
    -iquote "$SRC/include" \
    -o "$OUT/test-chat-parse" \
    "$SRC/tools/debug/test-chat-parse.c" \
    "$SRC/src/mmo_chat_parse.c"

"$OUT/test-chat-parse"
