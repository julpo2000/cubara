#!/usr/bin/env bash
#
# Launch Cubara (release build). Double-click in Finder on macOS to run it (the
# .command extension makes Finder open it in Terminal), or run it from a shell.
# Any extra arguments pass straight through to the app, so this covers every mode:
#
#   ./run.command                      # windowed game
#   ./run.command --caps               # GPU adapter capability report
#   ./run.command --bench              # headless benchmark (default radius 12)
#   ./run.command --bench 20           # headless benchmark at a larger radius
#   ./run.command --screenshot out.png # headless single-frame screenshot
#
# macOS + Linux. Windows users: double-click run.bat instead.

set -euo pipefail

# Run from the repo root regardless of where the script is launched from.
cd "$(dirname "$0")"

cargo run --release -- "$@"

# Keep the Terminal window open after the app exits so a double-click user can read
# the output (benchmark results, an error, …). Only when attached to a terminal.
if [ -t 0 ]; then
    echo
    read -r -n 1 -s -p "Press any key to close..."
    echo
fi
