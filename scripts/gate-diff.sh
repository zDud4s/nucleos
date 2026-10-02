#!/usr/bin/env bash
# Diff-aware gate: run only the scripts/gates.sh targets the change actually touches.
#
# Usage: scripts/gate-diff.sh [--dry-run] [--only core,shell,...] [<gates-script>]
#
#   --dry-run   print which paths select which target, run nothing
#   --only L    comma list; a selected target outside it is printed as skipped and not run
#               (the daemon uses `--only core`: Node/Go may be absent there, and a gate that
#               fails on a missing toolchain is reported as broken code)
#   <gates>     the gates script to run, default scripts/gates.sh. Pass it as an ARGUMENT, so it is
#               a word of the gate_command line and the daemon's tamper check (`worktree_scripts`
#               in core/src/gate.rs) still compares it; called only from in here it would not be.
#
# Changed paths = working tree vs `git merge-base HEAD $NUCLEOS_GATE_BASE` (default master), so
# uncommitted edits count, plus untracked files. If the merge-base cannot be found the answer is the
# conservative one: `core`, with a warning.
set -uo pipefail

dry=0 only="" gates="scripts/gates.sh"
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry=1 ;;
    --only) shift; only="${1:-}" ;;
    --only=*) only="${1#--only=}" ;;
    -*) echo "usage: $0 [--dry-run] [--only core,shell,...] [<gates-script>]" >&2; exit 2 ;;
    *) gates="$1" ;;
  esac
  shift
done

cd "$(cd "$(dirname "$0")/.." && pwd)" || exit 2

base="${NUCLEOS_GATE_BASE:-master}"
declare -A why=()   # target -> paths that selected it
add() { # add <target> <path>
  case " ${why[$1]:-} " in *" $2 "*) ;; *) why[$1]="${why[$1]:-}${why[$1]:+ }$2" ;; esac
}

if mb="$(git merge-base HEAD "$base" 2>/dev/null)" && [ -n "$mb" ]; then
  paths="$( { git diff --name-only "$mb"; git ls-files --others --exclude-standard; } 2>/dev/null | sort -u)"
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    case "$p" in
      scripts/gates.sh|scripts/gate-diff.sh|scripts/own-cargo-target.sh|scripts/build-slot.sh|.github/workflows/*)
        add all "$p" ;;
      core/hooks/*) add core "$p"; add hooks "$p" ;;
      core/*|Cargo.toml|Cargo.lock|rust-toolchain.toml) add core "$p" ;;
      sidecars/*) add sidecars "$p" ;;
      # src-tauri does not path-depend on core and is outside the cargo workspace, so a core change
      # never reaches it. shell/package.json and the lockfile only feed the frontend (tsc, vitest,
      # csp): src-tauri embeds the BUILT dist, which the gate takes as given, so they select shell.
      shell/src-tauri/*) add tauri "$p" ;;
      shell/*) add shell "$p" ;;
      scripts/csp-gate.mjs) add shell "$p" ;;
      .claude/hooks/*|scripts/test-*.py|scripts/eval/*|scripts/refresh-models.py|scripts/run-daemon.ps1)
        add hooks "$p" ;;
    esac
  done <<< "$paths"
else
  echo "gate-diff: warning: no merge-base with '$base'; falling back to core" >&2
  add core "(no merge-base)"
fi

# `all` is core + sidecars + shell + tauri + hooks, expanded here so `--only` can still filter it.
if [ -n "${why[all]:-}" ]; then
  for t in core sidecars shell tauri hooks; do why[$t]="${why[$t]:-}${why[$t]:+ }${why[all]}"; done
fi
targets=""
for t in core sidecars shell tauri hooks; do
  [ -n "${why[$t]:-}" ] && targets="$targets $t"
done

if [ -z "$targets" ]; then
  echo "gate-diff: nothing to gate"
  exit 0
fi

run_list=""
for t in $targets; do
  echo "gate-diff: $t <- ${why[$t]}"
  if [ -n "$only" ] && ! case ",$only," in *",$t,"*) true ;; *) false ;; esac; then
    echo "gate-diff: $t skipped (not in --only)"
    continue
  fi
  run_list="$run_list $t"
done

if [ -z "$run_list" ]; then
  echo "gate-diff: nothing to gate"
  exit 0
fi
[ "$dry" -eq 1 ] && { echo "gate-diff: would run:$run_list"; exit 0; }

for t in $run_list; do
  echo "gate-diff: running $t"
  bash "$gates" "$t" || exit $?
done
