#!/usr/bin/env bash
# Diff-aware gate: run only the scripts/gates.sh targets the change actually touches.
#
# Usage: scripts/gate-diff.sh [--dry-run] [--provision] [--only core,shell,...] [<gates-script>]
#
#   --dry-run   print which paths select which target, run nothing
#   --provision before running, make this tree able to run shell/tauri: `npm ci` in shell/ if
#               shell/node_modules is missing (shell or tauri selected), and `npm run build` there
#               if tauri is selected, so shell/dist exists. For a bare checkout (the daemon's
#               post-land worktree). A provisioning failure exits non-zero. Never runs under
#               --dry-run. Without it a missing node_modules still fails in gates.sh: locally that
#               means "you have not set this up".
#   --only L    comma list; a selected target outside it is printed as skipped and not run
#               (for a machine where Node/Go may be absent: a gate that fails on a missing
#               toolchain is reported as broken code. The post-land gate runs every target, with
#               --provision, on the owner's machine, which has them all)
#   <gates>     the gates script to run, default scripts/gates.sh. Pass it as an ARGUMENT, so it is
#               a word of the gate_command line and the daemon's tamper check (`worktree_scripts`
#               in core/src/gate.rs) still compares it; called only from in here it would not be.
#
# Changed paths = working tree vs `git merge-base HEAD $NUCLEOS_GATE_BASE` (default master), so
# uncommitted edits count, plus untracked files. If the merge-base cannot be found the answer is the
# conservative one: `core`, with a warning.
set -uo pipefail

dry=0 provision=0 only="" gates="scripts/gates.sh"
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry=1 ;;
    --provision) provision=1 ;;
    --only) shift; only="${1:-}" ;;
    --only=*) only="${1#--only=}" ;;
    -*) echo "usage: $0 [--dry-run] [--provision] [--only core,shell,...] [<gates-script>]" >&2; exit 2 ;;
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
      # own-cargo-target.sh and build-slot.sh wrap the cargo of every target, so they stay `all`
      # even though nucleos.tests.yaml lists them under hooks (their own tests live there).
      scripts/gates.sh|scripts/gate-diff.sh|scripts/own-cargo-target.sh|scripts/build-slot.sh|.github/workflows/*)
        add all "$p" ;;
      # nucleos.tests.yaml (`tests.groups`) is the source of truth for this path -> target list;
      # keep this case in step with it. First match wins, so a path in two groups uses add twice.
      core/hooks/*|core/assets/nucleos-dev/scripts/*) add core "$p"; add hooks "$p" ;;
      core/*|Cargo.toml|Cargo.lock|rust-toolchain.toml|.config/*|.nucleos/flows.yaml) add core "$p" ;;
      sidecars/*|scripts/build-sidecars.sh) add sidecars "$p" ;;
      # src-tauri does not path-depend on core and is outside the cargo workspace, so a core change
      # never reaches it. shell/package.json and the lockfile only feed the frontend (tsc, vitest,
      # csp): src-tauri embeds the BUILT dist, which the gate takes as given, so they select shell.
      shell/src-tauri/*) add tauri "$p"; add shell "$p" ;;
      shell/*) add shell "$p" ;;
      scripts/csp-gate.mjs) add shell "$p" ;;
      .claude/hooks/*|scripts/test-*.py|scripts/refresh-models.py|scripts/run-daemon.ps1|VERSION|scripts/bump-version.py|scripts/release-manifest.py|scripts/install.ps1|scripts/install.sh)
        add hooks "$p" ;;
      scripts/heavy.py|scripts/heavy_classify.py|scripts/heavy_guard.py|scripts/heavy_cost.py|scripts/heavy_watch.py|scripts/heavy_tests/*)
        add broker "$p" ;;
    esac
  done <<< "$paths"
else
  echo "gate-diff: warning: no merge-base with '$base'; falling back to core" >&2
  add core "(no merge-base)"
fi

# `all` is core + sidecars + shell + tauri + hooks + broker, expanded here so `--only` can still filter it.
if [ -n "${why[all]:-}" ]; then
  for t in core sidecars shell tauri hooks broker; do why[$t]="${why[$t]:-}${why[$t]:+ }${why[all]}"; done
fi
targets=""
for t in core sidecars shell tauri hooks broker; do
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

if [ "$provision" -eq 1 ]; then
  sel_shell=0 sel_tauri=0
  for t in $run_list; do
    [ "$t" = shell ] && sel_shell=1
    [ "$t" = tauri ] && sel_tauri=1
  done
  # A lockfile newer than npm's own record of the install means the tree moved past it.
  if [ $((sel_shell + sel_tauri)) -gt 0 ] && { [ ! -d shell/node_modules ] ||
      [ shell/package-lock.json -nt shell/node_modules/.package-lock.json ]; }; then
    echo "gate-diff: provisioning shell/node_modules (npm ci)"
    (cd shell && npm ci) || { echo "gate-diff: provisioning failed: npm ci in shell/" >&2; exit 1; }
  fi
  if [ "$sel_tauri" -eq 1 ]; then
    echo "gate-diff: provisioning shell/dist (npm run build)"
    (cd shell && npm run build) || { echo "gate-diff: provisioning failed: npm run build in shell/" >&2; exit 1; }
  fi
fi

for t in $run_list; do
  echo "gate-diff: running $t"
  bash "$gates" "$t" || exit $?
done
