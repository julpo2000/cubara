#!/usr/bin/env bash
# Start a world and tell people how to join it.
#
# Two windows, one world, each showing the other's character -- the thing this
# whole multiplayer effort was for. This script exists so that reaching it is
# one command rather than a sequence somebody has to remember at the end of a
# long day.
#
# It starts the **dedicated server**: headless, no window, no GPU
# (`ARCHITECTURE.md` Rule 4). Both game windows are then clients, including the
# one on this machine -- which is the arrangement `docs/PHASE2_MULTIPLAYER.md`
# §3.3 describes, and the one that makes a local player and a remote one the
# same thing.
#
# Usage:
#   ./scripts/play-together.sh [port]      (default 25650)
#
# Then, on each machine that should join:
#   cargo run --release -- --connect <this machine's address>
set -uo pipefail
cd "$(dirname "$0")/.."

port="${1:-25650}"
world="${CUBARA_WORLD:-saves/shared}"

# The address other machines use. `hostname -I` is Linux-only and the Mac needs
# `ipconfig`, so both are tried and neither is fatal: a wrong guess here costs a
# printed line, not the session.
lan_ip="$(ipconfig getifaddr en0 2>/dev/null \
        || ipconfig getifaddr en1 2>/dev/null \
        || hostname -I 2>/dev/null | awk '{print $1}' \
        || echo '<this machine>')"

echo "Building the server..."
if ! cargo build --release -p cubara-server --bin cubara-server; then
    echo
    echo "The build failed. If the error mentions 'tapi error: malformed file'"
    echo "and an SDK path, this machine's Command Line Tools are mid-update and"
    echo "the problem is not the code -- see the note in CONTRIBUTING.md."
    exit 1
fi

echo
echo "──────────────────────────────────────────────────────────────────"
echo "  World:  $world"
echo "  Join from this machine:   cargo run --release -- --connect 127.0.0.1:$port"
echo "  Join from another:        cargo run --release -- --connect $lan_ip:$port"
echo
echo "  Both windows are clients. Each machine wears its own colour:"
echo "  Linux red, Windows yellow, macOS green -- the client picks it and"
echo "  says so when it joins, so everyone agrees about who is who."
echo
echo "  Clients must be built from the same commit as this server: the"
echo "  protocol carries the colour now, and an older build cannot say it."
echo
echo "  Ctrl-C stops the world. It saves on the way out."
echo "──────────────────────────────────────────────────────────────────"
echo

exec ./target/release/cubara-server --listen "0.0.0.0:$port" --world "$world"
