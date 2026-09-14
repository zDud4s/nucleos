#!/usr/bin/env bash
# Give the checkout this runs in a cargo target directory of its own, then run a command with it.
#
# Usage: scripts/own-cargo-target.sh <absolute root> <command...>
#
# Exports CARGO_TARGET_DIR=<root>/<checkout name>-<8 hex digits of a hash of its path>, creates it,
# and execs the command. A checkout keeps its tree for as long as it keeps being built; a tree
# nobody has built under this script for 24 hours is deleted by the next run under the same root.
#
# Why it exists. The daemon's gate command gave every checkout it gates — job worktrees, run
# worktrees, the main checkout — one `CARGO_TARGET_DIR=C:/Projects/.cargo-target-gate`. Cargo keys a
# workspace member's artifacts by its workspace-relative path, which is the same in every checkout,
# and judges them fresh by source mtimes, so an artifact built for checkout A is reused for checkout
# B whenever none of B's files is newer. Job 27's second gate, 2026-09-14: `core/tests/module_map.rs`
# ran a binary compiled in `.nucleos/worktrees/run-900507` and failed its compiled-in-vs-running-in
# assertion. That one was loud. The same reuse can have B's gate run A's code outright, and pass it.
#
# Why a script. The daemon does not run `gate_command` through a shell: `core/src/gate.rs` peels the
# leading `NAME=value` words off and executes the program directly in the worktree, so nothing in the
# YAML can compute a directory from the checkout's path — a `$(...)` would reach cargo as text. Nor
# can the line be wrapped in `bash -c`, which collapses it into one word and leaves the tamper check
# (`worktree_scripts`) comparing nothing. This script is a word of the command, so that check holds it
# against the project root's copy exactly as it holds `scripts/gates.sh`.
#
# The line for `.ai/autopilot.yaml`:
#
#   gate_command: '"C:/Program Files/Git/bin/bash.exe" scripts/own-cargo-target.sh C:/Projects/.cargo-target-gates bash scripts/gates.sh core'
#
# The second `bash` is Git's own, not WSL's: the Git bash the first word starts puts `/usr/bin` ahead
# of `C:\Windows\System32`, so `type -P bash` answers `/usr/bin/bash` in it (measured 2026-09-14,
# launched from a Windows process as the daemon launches it). A relative word naming no file, it is
# nothing the tamper check has to compare. A leading `CARGO_TARGET_DIR=...` is no longer needed, and
# is harmless if left: this script overrides it.
#
# What it costs, said here rather than discovered later: the shared tree was chosen for TIME. Each
# checkout's first gate under this script pays a cold build of every dependency — about 15 minutes
# for the binary alone, measured 2026-09-09 — against the gate's 2700s timeout; its later gates are
# warm. And a core tree runs 3-7 GB, so the disk holds one per checkout gated in the last day.
set -uo pipefail

marker=.own-cargo-target

refuse() {
  echo "own-cargo-target: $1" >&2
  echo "usage: scripts/own-cargo-target.sh <absolute root> <command...>" >&2
  exit 2
}

[ "$#" -ge 1 ] || refuse "no root given"
[ "$#" -ge 2 ] || refuse "no command given"
root="${1%/}"
shift
# Absolute, or nothing is created at all. A relative root would resolve against each worktree and
# grow one tree per checkout inside the checkouts themselves: the 11.8 GB `target-test` in the main
# checkout (2026-08-28) is what that looks like.
case "$root" in
  /?*|[A-Za-z]:[/\\]?*) ;;
  *) refuse "the root must be an absolute path, not '$root'" ;;
esac

# `pwd -P`, so two spellings of one checkout are one directory. `cksum` because it is POSIX: Git's
# bash, Linux and macOS all have it, which is not true of `sha1sum`.
here="$(pwd -P)"
hash="$(printf '%s' "$here" | cksum | { read -r crc _; printf '%08x' "$crc"; })"
own="$root/$(basename "$here")-$hash"

if ! mkdir -p "$own" || ! touch "$own/$marker"; then
  echo "own-cargo-target: cannot create $own" >&2
  exit 2
fi

# Only a directory directly under <root> holding the marker, and never this checkout's own. The root
# is somebody's disk; a directory this script did not create is not its to judge, however old. Depth
# exactly two (<root>/<dir>/<marker>) and no `-L`, so neither a deeper marker nor a symlink's target
# is ever a candidate. Best effort: a tree that will not delete now is tried again next time, and
# pruning never decides the command's status.
find "$root" -mindepth 2 -maxdepth 2 -type f -name "$marker" -mmin +1440 2>/dev/null |
  while IFS= read -r stale; do
    dir="${stale%/*}"
    [ "$dir" = "$own" ] && continue
    rm -rf -- "$dir" 2>/dev/null && echo "own-cargo-target: pruned $dir, unbuilt for over 24 hours" >&2
  done

export CARGO_TARGET_DIR="$own"
exec "$@"
