#!/usr/bin/env bash
# Lay out an eval task's BASE tree -- the tree an agent is handed, with the bug
# present and the held-out test absent.
#
#   scripts/eval/base.sh --task T1 --dest <path> [--tasks-file F] [--repo R]
#
# TWO KINDS OF BASE
#
#   natural    `reference~1`. Works when the reference commit is small enough
#              that "the repository one commit earlier" is the task and nothing
#              else. T2 is the only one in the set.
#
#   synthetic  `reference` with `bases/<id>.patch` applied. The patch reverts
#              exactly the pieces the task names and deletes the held-out test.
#
# WHY SYNTHETIC BASES EXIST
#
# Two of the three tasks were picked by reading commit SUBJECTS. Read as diffs,
# `36afc92` is 1349 lines across 5 files and `4de2fe0` is 840 across 12 -- so
# `reference~1` does not ask "add a mode to a SQL filter", it asks "build the
# email triage loop, then add it". A synthetic base keeps the subsystem and
# reverts only the named piece.
#
# The cost of that convenience is that the base is no longer a commit anybody
# ever had, so the patch is checked in and this script is the only way to build
# one. A recipe written in prose is a recipe nobody can rerun.
#
# WHAT THE PATCH MUST DO
#
#   1. Reintroduce the bug.
#   2. Delete the held-out test named in tasks.tsv, so the agent cannot read the
#      answer out of the tree it is given, and so the base's own suite is green.
#      A base that ships its own failing test is a different, easier task.
#   3. Leave everything the grafted test needs to COMPILE -- shared helpers
#      especially. Deleting `seed_triage_spend` along with T1's test would make
#      every candidate score inconclusive forever.
#
# `scripts/eval/verify-task.sh` checks all three by using them.
#
# EXIT CODES
#
#   0  the base tree is laid out at --dest
#   2  anything else

set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { printf 'base.sh: %s\n' "$*" >&2; exit 2; }

task=""
dest=""
tasks_file="$script_dir/tasks.tsv"
repo=""

while [ $# -gt 0 ]; do
  case "$1" in
    --task)       task="${2:-}"; shift 2 ;;
    --dest)       dest="${2:-}"; shift 2 ;;
    --tasks-file) tasks_file="${2:-}"; shift 2 ;;
    --repo)       repo="${2:-}"; shift 2 ;;
    -h|--help)    awk 'NR > 1 { if (/^#/) { sub(/^# ?/, ""); print; next } exit }' "${BASH_SOURCE[0]}"; exit 0 ;;
    *)            die "unknown argument: $1 (try --help)" ;;
  esac
done

[ -n "$task" ] || die "--task is required"
[ -n "$dest" ] || die "--dest is required"
[ -f "$tasks_file" ] || die "no task table at $tasks_file"

if [ -z "$repo" ]; then
  repo="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null)" \
    || die "this script is not inside a git repository; pass --repo"
fi

row="$(awk -F'\t' -v id="$task" '
  { sub(/\r$/, "") }
  /^[ \t]*#/ { next }
  /^[ \t]*$/ { next }
  $1 == id { print; found = 1; exit }
  END { if (!found) exit 1 }
' "$tasks_file")" || die "unknown task '$task'"

reference="$(printf '%s' "$row" | cut -f2)"
[ -n "$reference" ] || die "task '$task' has no reference commit"

patch_file="$script_dir/bases/$task.patch"

if [ -f "$patch_file" ]; then
  kind="synthetic"
  revision="$reference"
else
  kind="natural"
  revision="$reference~1"
fi

bash "$script_dir/materialize.sh" "$revision" "$dest" --repo "$repo" >/dev/null \
  || die "could not materialize $revision into $dest"

if [ "$kind" = synthetic ]; then
  # `git apply` runs outside a work tree and tolerates the CRLF the archive
  # carries on this machine; `patch -p1` would need --binary and still guess.
  # Not `--3way`: a fuzzy apply against the wrong commit would produce a base
  # nobody can reproduce, which is the failure this script exists to prevent.
  ( cd "$dest" && git apply --whitespace=nowarn "$patch_file" ) \
    || die "bases/$task.patch does not apply to $reference — the patch and the reference commit have drifted apart; regenerate it"
fi

sha="$(git -C "$repo" rev-parse --verify --quiet "${revision}^{commit}")"
printf '%s\t%s\t%s\t%s\n' "$task" "$kind" "$sha" "$dest"
