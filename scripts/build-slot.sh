#!/usr/bin/env bash
# Run a heavy command (cargo build/test/clippy/check) through the machine-wide broker
# (<main>/scripts/heavy.py) when the main checkout has one; otherwise while holding one of at most
# two build slots, so nine sessions cannot compile nucleos-core at once.
#
# Usage: scripts/build-slot.sh <command...>        (Git bash on Windows, never the WSL `bash`)
#
# Environment: NUCLEOS_BUILD_SLOTS (default 2; 0 bypasses), NUCLEOS_BUILD_SLOTS_DIR (absolute),
# NUCLEOS_BUILD_SLOT_TIMEOUT (seconds, default 1800; gives up with exit 75; slot fallback only),
# NUCLEOS_HEAVY_MAIN / NUCLEOS_HEAVY_PYTHON (broker location and interpreter). A nested call under
# a holder takes no second slot.
#
# The logic lives in scripts/gates.sh, which this file sources, and not here: the daemon's tamper
# check compares only the files its gate command names, and that is gates.sh.
#
# Daemon runs must call `cargo` directly. This wrapper is `bash <script>`, which the classifier does
# not judge safe, so a run using it would park in pending_approval.
[ "$#" -ge 1 ] || { echo "usage: scripts/build-slot.sh <command...>" >&2; exit 2; }
# shellcheck source=gates.sh
source "$(cd "$(dirname "$0")" && pwd)/gates.sh" || exit 2
heavy_slot_any=1
heavy_run "$@"
status=$?
rm -rf "$captures"
exit "$status"
