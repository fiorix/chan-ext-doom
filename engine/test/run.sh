#!/bin/sh
#
# Host tests for the parts of the WebSocket transport that must reject bad
# input. Runs with a plain host compiler: no emscripten, no SDL, no WADs.
#
# Usage: engine/test/run.sh

set -e

cd "$(dirname "$0")/.."

CC=${CC:-cc}
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

$CC -std=c99 -Wall -Wextra -Werror -Wno-unused-parameter \
    -Isrc -Isrc/doom \
    src/net_ws_frame.c test/test_net_ws_frame.c \
    -o "$OUT/test_net_ws_frame"

"$OUT/test_net_ws_frame"
