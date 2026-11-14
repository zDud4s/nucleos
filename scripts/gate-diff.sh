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
#
# When `core` is selected it is also narrowed to the workspace crates the change reaches (printed as
# `gate-diff: core crates: ...`, under --dry-run too) and handed to gates.sh as NUCLEOS_GATE_CRATES;
# see "Which of the four core crates" below for how a path is mapped to a crate.
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
      scripts/heavy.py|scripts/heavy_classify.py|scripts/heavy_guard.py|scripts/heavy_cost.py|scripts/heavy_watch.py|scripts/heavy_wait.py|scripts/heavy_cache.py|scripts/heavy_failfirst.py|scripts/heavy_tests/*)
        add broker "$p" ;;
    esac
  done <<< "$paths"
else
  echo "gate-diff: warning: no merge-base with '$base'; falling back to core" >&2
  add core "(no merge-base)"
  crates_all_reason="no merge-base"
fi

# `all` is core + sidecars + shell + tauri + hooks + broker, expanded here so `--only` can still filter it.
if [ -n "${why[all]:-}" ]; then
  for t in core sidecars shell tauri hooks broker; do why[$t]="${why[$t]:-}${why[$t]:+ }${why[all]}"; done
  crates_all_reason="${crates_all_reason:-${why[all]%% *} selects every target}"
fi

# --- Which of the four core crates `core` gates (NUCLEOS_GATE_CRATES for gates.sh) -------------------
# nucleos-base <- nucleos-core <- nucleos-app <- nucleos-daemon: a changed crate is gated with every
# crate above it. nucleos-core, nucleos-app and nucleos-daemon share core/src, so a path's crate is
# read from the module declarations of their roots (lib.rs, app.rs, main.rs) at gate time, not from
# its directory: core/src/<mod>.rs and core/src/<mod>/... belong to the root that declares `mod
# <mod>`, and an inline `mod m { mod c; }` gives core/src/m/c to its root (longest match wins, so
# lib.rs's `map { intent }` beats app.rs's `map`). Two more facts widen that, never narrow it: a
# `#[path = "..."]` anywhere in core/src makes its target part of the declaring file's crate, and a
# file an `include_str!`/`include_bytes!` names is also an input of the including file's crate.
# The same `#[path]` fact owns core/tests: its file-reading guards are pulled into the daemon's unit
# tests by core/src/main.rs, so a change to one gates nucleos-daemon alone.
# Whatever is not confidently one crate (a root itself, a manifest, the lockfile, toolchain,
# .config/, core/hooks, core/assets, a core/tests file no `#[path]` names, migrations, any unknown
# path, the no-merge-base fallback, an `all` trigger) gates all four.
gate_crates_all="nucleos-base nucleos-core nucleos-app nucleos-daemon"
declare -A mod_owner=()   # module key under core/src (e.g. `fleet`, `map/intent`) -> crate names
declare -A path_owner=()  # exact file a `#[path]` names -> crate names
declare -A incl_owner=()  # exact file an include_str!/include_bytes! names -> crate names

# norm_path <a/./b/../c>: the path with `.` and `..` folded (a `..` above the start is kept).
norm_path() {
  local IFS=/ part out=()
  for part in $1; do
    case "$part" in
      ''|.) ;;
      ..) if [ ${#out[@]} -gt 0 ] && [ "${out[${#out[@]}-1]}" != .. ]; then out=("${out[@]:0:${#out[@]}-1}"); else out+=(..); fi ;;
      *) out+=("$part") ;;
    esac
  done
  printf '%s' "${out[*]}"
}

# root_modules <root.rs> <crate>: one "<key> <crate>" line per module the root declares at its top
# level, or one level down inside an inline `mod m { ... }` (as `m/<child>`). A `#[path]` on such a
# declaration names the file instead (dropped when it leaves core/src: those paths gate all four).
root_modules() {
  awk -v crate="$2" '
    function modname(s) {
      sub(/^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+/, "", s)
      sub(/[^A-Za-z0-9_].*$/, "", s)
      return s
    }
    function reg(prefix, name) {
      if (pend != "") {
        if (pend !~ /\.\./) { k = pend; sub(/\.rs$/, "", k); print prefix k " " crate }
      } else {
        print prefix name " " crate
      }
      pend = ""
    }
    /^[[:space:]]*#\[path[[:space:]]*=[[:space:]]*"/ { p = $0; sub(/^[^"]*"/, "", p); sub(/".*$/, "", p); pend = p; next }
    /^[[:space:]]*#\[/ { next }
    /^[[:space:]]*\/\// { next }
    /^}/ { blk = ""; pend = ""; next }
    blk == "" && /^(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*;/ { reg("", modname($0)); next }
    blk == "" && /^(pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*\{/ { blk = modname($0); pend = ""; next }
    blk != "" && /^    (pub(\([a-z]+\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z0-9_]+[[:space:]]*;/ { reg(blk "/", modname($0)); next }
    { pend = "" }
  ' "$1"
}

# add_owner <array-name> <key> <crate>: append crate to the space list at array[key].
add_owner() {
  local -n arr="$1"
  case " ${arr[$2]:-} " in *" $3 "*) ;; *) arr[$2]="${arr[$2]:-}${arr[$2]:+ }$3" ;; esac
}

# owners_of <path>: the crates the file compiles in, by declaration alone (empty when unknown).
# Sets `owners`; a subshell-free return, since this runs once per path and per include.
owners_of() {
  local p="$1" rel key best=0 len o
  owners=""
  case "$p" in
    core/src/lib.rs) owners="nucleos-core"; return ;;
    core/src/app.rs) owners="nucleos-app"; return ;;
    core/src/main.rs) owners="nucleos-daemon"; return ;;
    core/build.rs) owners="nucleos-core"; return ;;
    core/crates/nucleos-base/src/*|core/crates/nucleos-base/tests/*) owners="nucleos-base"; return ;;
    # A core/tests file compiles only where a `#[path]` pulls it in (no package auto-discovers
    # tests: autotests = false), so its owner is that declarer's crate; unnamed = unknown.
    core/tests/*) owners="${path_owner[$p]:-}"; return ;;
    core/src/*) ;;
    *) return ;;
  esac
  rel="${p#core/src/}"
  for key in "${!mod_owner[@]}"; do
    case "$rel" in
      "$key".rs|"$key"/*)
        len=${#key}
        if [ "$len" -gt "$best" ]; then best=$len; owners="${mod_owner[$key]}"
        elif [ "$len" -eq "$best" ]; then for o in ${mod_owner[$key]}; do case " $owners " in *" $o "*) ;; *) owners="$owners $o" ;; esac; done
        fi ;;
    esac
  done
  for o in ${path_owner[$p]:-}; do case " $owners " in *" $o "*) ;; *) owners="${owners}${owners:+ }$o" ;; esac; done
}

gate_crates="$gate_crates_all"
if [ -n "${why[core]:-}" ] && [ -z "${crates_all_reason:-}" ]; then
  [ -f core/src/lib.rs ] && while read -r k c; do add_owner mod_owner "$k" "$c"; done < <(root_modules core/src/lib.rs nucleos-core)
  [ -f core/src/app.rs ] && while read -r k c; do add_owner mod_owner "$k" "$c"; done < <(root_modules core/src/app.rs nucleos-app)
  [ -f core/src/main.rs ] && while read -r k c; do add_owner mod_owner "$k" "$c"; done < <(root_modules core/src/main.rs nucleos-daemon)
  # `#[path]` first (it decides where a file compiles), then the includes, which read owners_of.
  refs="$(grep -rHoE --include='*.rs' '(#\[path[[:space:]]*=[[:space:]]*|include_(str|bytes)!\()"[^"]+"' core/src core/crates 2>/dev/null)"
  for pass in path incl; do
    while IFS= read -r line; do
      [ -n "$line" ] || continue
      file="${line%%:*}" m="${line#*:}"
      case "$pass:$m" in path:'#['*|incl:include*) ;; *) continue ;; esac
      arg="${m#*\"}"; arg="${arg%\"}"
      target="$(norm_path "${file%/*}/$arg")"
      owners_of "$file"
      [ -n "$owners" ] || owners="$gate_crates_all"   # an unknown includer: assume the worst
      for o in $owners; do add_owner "${pass}_owner" "$target" "$o"; done
    done <<< "$refs"
  done

  sel=""
  for p in ${why[core]}; do
    case "$p" in
      core/src/lib.rs|core/src/app.rs|core/src/main.rs|*Cargo.toml|Cargo.lock) crates_all_reason="$p is not one crate's"; break ;;
    esac
    owners_of "$p"
    # Unknown by declaration = all four, even if something includes it (core/hooks, core/assets,
    # migrations are read by modules of several crates, and at run time by more).
    if [ -z "$owners" ]; then crates_all_reason="$p is not one crate's"; break; fi
    for o in ${incl_owner[$p]:-}; do case " $owners " in *" $o "*) ;; *) owners="$owners $o" ;; esac; done
    sel="$sel $owners"
  done
  if [ -z "${crates_all_reason:-}" ]; then
    # The reverse-dependency closure, in the order of $gate_crates_all.
    case " $sel " in
      *" nucleos-base "*) gate_crates="$gate_crates_all" ;;
      *" nucleos-core "*) gate_crates="nucleos-core nucleos-app nucleos-daemon" ;;
      *" nucleos-app "*) gate_crates="nucleos-app nucleos-daemon" ;;
      *) gate_crates="nucleos-daemon" ;;
    esac
  fi
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
  if [ "$t" = core ]; then
    if [ "$gate_crates" = "$gate_crates_all" ]; then
      echo "gate-diff: core crates: all four (${crates_all_reason:-a change in nucleos-base})"
    else
      echo "gate-diff: core crates: $gate_crates"
    fi
  fi
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
  if [ "$t" = core ]; then
    NUCLEOS_GATE_CRATES="$gate_crates" bash "$gates" "$t" || exit $?
  else
    bash "$gates" "$t" || exit $?
  fi
done
