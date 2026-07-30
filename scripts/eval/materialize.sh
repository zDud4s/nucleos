#!/usr/bin/env bash
# Lay a git revision out as a plain directory, for use as a candidate tree.
#
#   scripts/eval/materialize.sh <revision> <destination> [--repo PATH]
#
# `git archive` rather than `git worktree add`: the scorer's proof runs happen on
# a machine where someone else is working in the same repository, and adding a
# worktree writes to shared .git state. An archive extraction reads and nothing
# else, and the result is what an agent hands over anyway -- a directory of
# files, not a checkout.
#
# The destination is wiped first. It must not contain a space: the worktree
# tests resolve their tempdir from the process's current directory and assert it
# is space-free, so a spaced path fails ~25 tests for reasons that have nothing
# to do with the task.

set -uo pipefail

die() { printf 'materialize.sh: %s\n' "$*" >&2; exit 2; }

revision=""
dest=""
repo=""

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) if [ -z "$revision" ]; then revision="$1"; elif [ -z "$dest" ]; then dest="$1";
       else die "unexpected argument: $1"; fi; shift ;;
  esac
done

[ -n "$revision" ] || die "a revision is required"
[ -n "$dest" ] || die "a destination is required"

if [ -z "$repo" ]; then
  repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && git rev-parse --show-toplevel)" \
    || die "not inside a git repository; pass --repo"
fi

case "$dest" in
  *\ *) die "destination contains a space ($dest) — the worktree tests will fail on it" ;;
esac

sha="$(git -C "$repo" rev-parse --verify --quiet "${revision}^{commit}")" \
  || die "no such commit in $repo: $revision"

rm -rf "$dest" || die "cannot clear $dest"
mkdir -p "$dest" || die "cannot create $dest"

git -C "$repo" archive --format=tar "$sha" | ( cd "$dest" && tar -xf - )
status=("${PIPESTATUS[@]}")
[ "${status[0]}" -eq 0 ] && [ "${status[1]}" -eq 0 ] || die "extracting $sha failed"

printf '%s\t%s\n' "$sha" "$dest"
