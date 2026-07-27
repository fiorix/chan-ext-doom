#!/bin/sh
#
# Host tests for the parts that must reject bad input or preserve an exact
# order. Runs with a plain host compiler and node: no emscripten, no SDL,
# no WADs.
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

# boolean must have one ABI whatever the include order. Two objects reach
# <stdbool.h> and the Doom headers in opposite orders and are linked together;
# a split here silently changes sizeof(player_t) between objects.
$CC -std=c99 -Wall -Wextra -Werror -Wno-unused-parameter \
    -Isrc -Isrc/doom \
    test/test_abi_a.c test/test_abi_b.c test/test_abi.c \
    -o "$OUT/test_abi"

"$OUT/test_abi"

"$OUT/test_net_ws_frame"

# The loopback transport owns the packets its callers dup into it, so its
# overflow path is checked against a counting allocator.
$CC -std=c99 -Wall -Wextra -Werror -Wno-unused-parameter \
    -Isrc -Isrc/doom \
    src/net_loop.c src/net_packet.c test/test_net_loop.c \
    -o "$OUT/test_net_loop"

"$OUT/test_net_loop"

# The desync canary must call two peers equal when only their consoleplayer
# byte differs or they stopped recording at different moments, and unequal on
# any difference inside the anchored span.
$CC -std=c99 -Wall -Wextra -Werror -Wno-unused-parameter \
    -Isrc -Isrc/doom \
    src/d_democanary.c test/test_democanary.c \
    -o "$OUT/test_democanary"

# The exit-state canary must move on any simulation change and stay put on
# any per-peer presentation change. The serializer reads engine globals, so
# the test supplies a synthesized mini-state rather than booting the game.
$CC -std=c99 -Wall -Wextra -Werror -Wno-unused-parameter \
    -Isrc -Isrc/doom \
    src/doom/d_statecanary.c src/d_democanary.c test/test_statecanary.c \
    -o "$OUT/test_statecanary"

"$OUT/test_statecanary"

"$OUT/test_democanary"

# Mod ordering is loader-side, so it is tested where it lives.
node test/test_mod_order.mjs

# Address ownership contract, guarded mechanically because getting it wrong
# is silent: NET_RecvPacket in net_io.c takes the one reference consumers
# release, so a transport module that also references leaks one refcount per
# received packet and its address table grows without bound. No transport
# module may reference in its own RecvPacket.
for mod in src/net_websockets.c src/net_sdl.c src/net_loop.c; do
    if sed -n '/RecvPacket(net_addr_t \*\*addr/,/^}/p' "$mod" \
        | grep -q NET_ReferenceAddress; then
        echo "FAIL: $mod references the address in its own RecvPacket" >&2
        echo "      net_io.c NET_RecvPacket already does; see net_sdl.c" >&2
        exit 1
    fi
done
echo "address ownership contract: 3 transport modules checked, 0 violations"
