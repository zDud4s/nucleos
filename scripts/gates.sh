#!/usr/bin/env bash
# Run every gate this repo treats as its definition of green, for all three stacks.
#
# These commands used to live only in people's shell history, which is how a red suite and an
# AGENTS.md claiming "green" managed to coexist. This script is where they live now, and
# `.github/workflows/ci.yml` calls it rather than repeating them — one definition of green, run in
# two places.
#
# The gate stops at its first red step and names the steps it skipped (owner's call, 2026-10-08).
# Running on used to be the rule, so that one run reported every failure, but 11 of the 12 gates in
# the broker log were red and kept going: one ran a 36-minute test suite after clippy had already
# failed, holding a build slot that every other worktree was waiting for. NUCLEOS_GATE_KEEP_GOING=1
# restores the old behaviour for a run that wants the whole list.
#
# Usage: scripts/gates.sh [core|sidecars|shell|tauri|hooks|security|broker|all]   (default: all)
set -uo pipefail

# A gate reads nothing. A daemon started without a console hands down a dead stdin handle, and every
# Python `subprocess` under this script then dies with WinError 6: a false red.
exec </dev/null

failures=""
skipped=""

# Every step's output is streamed exactly as it always was AND kept, so that a red step's own last
# lines can be repeated under its name in the closing summary. The daemon hands the node that must
# answer a red gate only the last 4096 bytes of its output (`GATE_OUTPUT_TAIL` in core/src/job.rs),
# and the summary is the one part of the output certain to be inside them. Job 27, 2026-09-14:
# `core: fmt` failed first and `core: test` then printed about a megabyte, so the retry saw
# `core: fmt` only as a name in a list and never rustfmt's diff. Beside an unrelated flaky test it
# decided the red was not its own, left the file unformatted, and the next gate failed on fmt again.
#
# Twelve lines of at most 200 bytes, and no more than 800 bytes per step all told: three red steps
# — every step `core` has — come to about 2.5 KB with their labels, well inside that tail.
summary_lines=12
summary_width=200
summary_bytes=800

# One file, overwritten by each step: only the step that has just run is ever read back.
captures="$(mktemp -d 2>/dev/null)" || captures=""
trap 'rm -rf "$captures"' EXIT

# Persist a step's whole capture, because the kept tail (and the daemon's 4096 bytes) cannot hold a
# failing test's panic text. Keeps the newest 40 logs; every failure here is swallowed, so a full
# disk or an odd filesystem can never turn a green step red or change an exit code.
save_log() {
  # save_log <label> <capture> <status>
  local label="$1" capture="$2" status="$3" home logs name path
  [ "$capture" = /dev/null ] && return 0
  if [ -n "${NUCLEOS_DEV_HOME:-}" ]; then
    home="$NUCLEOS_DEV_HOME"
  elif [ -d /c/nucleos-dev ]; then
    home=/c/nucleos-dev
  else
    home="${TMPDIR:-/tmp}"
  fi
  logs="$home/scratch/gate-logs"
  name="$(printf '%s' "$label" | tr -c 'A-Za-z0-9._-' '_')"
  path="$logs/$(date -u +%Y%m%dT%H%M%SZ)-$$-$name.log"
  {
    mkdir -p "$logs" && cp "$capture" "$path"
  } 2>/dev/null || return 0
  # Newest first by name (the UTC stamp leads), so everything past the 40th goes.
  ( ls -1 "$logs"/*.log 2>/dev/null | sort -r | tail -n +41 | while IFS= read -r old; do rm -f "$old"; done ) 2>/dev/null || true
  [ "$status" -ne 0 ] && printf 'full log: %s\n' "$path"
  return 0
}

run() {
  # run <label> <dir> <command...>
  local label="$1" dir="$2" capture=/dev/null status started
  shift 2
  [ -n "$captures" ] && capture="$captures/step"
  printf '\n=== %s ===\n' "$label"
  if [ -n "$failures" ] && [ "${NUCLEOS_GATE_KEEP_GOING:-0}" != 1 ]; then
    printf 'skip %s\n' "$label"
    skipped="$skipped  $label"$'\n'
    return 0
  fi
  # stderr joins stdout on its way into `tee`, because the kept lines need both in the order they
  # were printed: rustfmt's diff goes to one and cargo's `error:` to the other. The daemon already
  # reads the two as one stream. The cost: a step that leaves a background process holding its
  # output now holds the gate until that process lets go, since `tee` waits for every writer.
  # A heavy cargo subcommand takes a build slot (see `slot_run`; check|clippy pass through it
  # without one). It is decided HERE and not written on each gate line, because the classifier
  # test reads those lines as plain commands.
  # The same goes for the other heavy steps (tsc, npm test, go test/vet): they reach the machine-wide
  # broker through `heavy_run`, which falls back to today's behaviour when no broker is present.
  if [ "$1" = cargo ]; then
    case "$2" in build|check|clippy|test|nextest|run|doc) set -- heavy_run "$@" ;; esac
  elif [ "$1" = npx ] && [ "$2" = tsc ]; then
    set -- heavy_run "$@"
  elif [ "$1" = npm ] && [ "$2" = test ]; then
    set -- heavy_run "$@"
  elif [ "$1" = go ]; then
    case "$2" in test|vet) set -- heavy_run "$@" ;; esac
  fi
  started="$SECONDS"
  ( cd "$dir" && "$@" ) 2>&1 | tee "$capture"
  status="${PIPESTATUS[0]}"
  save_log "$label" "$capture" "$status"
  if [ "$status" -eq 0 ]; then
    printf 'ok   %s\n' "$label"
    # heavy.py re-ran only the tests that failed last time (its stderr line is in the output
    # above); say so here too, so a narrowed pass is never read as a green suite.
    if grep -q 'heavy: PARTIAL pass' "$capture" 2>/dev/null; then
      printf 'note %s: PARTIAL pass, only the previously failing tests re-ran (NUCLEOS_HEAVY_FULL=1 forces a full run)\n' "$label"
    fi
  else
    printf 'FAIL %s\n' "$label"
    failures="$failures  $label"$'\n'"$(kept_lines "$capture")"$'\n'
  fi
  # Measurement hook: how long each step took, so a slow gate can be pinned on a step.
  printf 'elapsed %s: %ss\n' "$label" "$((SECONDS - started))"
}

# Per-run knobs for the cargo steps. All optional; unset leaves the command as it always was.
#   NUCLEOS_TARGET_DIR          the verify executor's warm target dir (`warm:` in nucleos.tests.yaml).
#                               Passed as `--target-dir`, NOT as CARGO_TARGET_DIR: cargo hands its
#                               environment to rustc and sccache hashes every CARGO_* variable into
#                               its key, so a per-worktree path in the env means ~0% hits across
#                               worktrees (see scripts/heavy.py `with_target_dir`).
#   NUCLEOS_GATE_JOBS           cargo `-j` (build parallelism).
#   NUCLEOS_GATE_TEST_THREADS   `--test-threads` for nextest / the libtest binaries.
# gate_build_args sets `gate_args` (flags for a compiling subcommand), gate_threads sets `gate_tt`.
gate_build_args() {
  gate_args=()
  [ -n "${NUCLEOS_TARGET_DIR:-}" ] && gate_args+=(--target-dir "$NUCLEOS_TARGET_DIR")
  [ -n "${NUCLEOS_GATE_JOBS:-}" ] && gate_args+=(-j "$NUCLEOS_GATE_JOBS")
  return 0
}

gate_threads() {
  # gate_threads <nextest|libtest>
  gate_tt=()
  [ -z "${NUCLEOS_GATE_TEST_THREADS:-}" ] && return 0
  if [ "$1" = nextest ]; then
    gate_tt=(--test-threads "$NUCLEOS_GATE_TEST_THREADS")
  else
    gate_tt=(-- --test-threads "$NUCLEOS_GATE_TEST_THREADS")
  fi
}

kept_lines() {
  # kept_lines <capture>: the last non-blank lines of a step's output, indented to sit under its
  # label. Blank lines go before counting, so twelve lines are twelve lines of evidence. Counted in
  # bytes (LC_ALL=C) because the budget is the daemon's and it counts bytes; a character cut in half
  # at the width comes out as one U+FFFD from the daemon's lossy decoding, not as an error.
  LC_ALL=C tail -n 200 "$1" 2>/dev/null | LC_ALL=C awk \
    -v lines="$summary_lines" -v width="$summary_width" -v budget="$summary_bytes" '
      { gsub(/\r/, ""); if ($0 !~ /[^[:space:]]/) next; kept[++n] = substr($0, 1, width) }
      END {
        if (n == 0) { print "      (no output)"; exit }
        first = n + 1
        for (i = n; i >= 1 && n - i < lines; i--) {
          used += length(kept[i]) + 7
          if (used > budget) break
          first = i
        }
        for (i = first; i <= n; i++) print "      " kept[i]
      }'
}

print_summary() {
  if [ -n "$failures" ]; then
    printf '\ngates FAILED:\n%s' "$failures" >&2
    # After the evidence, so it stays inside the daemon's 4096-byte tail with it.
    [ -n "$skipped" ] && printf '\nnot run, after the first red step:\n%s' "$skipped" >&2
    return 1
  fi
  printf '\nall gates green.\n'
}

# Build slots: at most NUCLEOS_BUILD_SLOTS (default 2) heavy cargo builds at once, machine-wide.
# Every session builds nucleos-core in its own CARGO_TARGET_DIR, so nothing else serialises them: on
# 2026-10-01 nine full builds ran together, saturating the CPU and filling the disk. `flock` is not
# guaranteed under Git bash, so the lock is portable: one file per holder, `held/<pid>`, and a slot
# whose pid no longer answers `kill -0` is reaped by the next acquirer (a SIGKILLed holder cannot
# clean up after itself). A short-lived `mkdir` mutex makes reap-count-claim one atomic step.
#
# `cargo check` and `cargo clippy` are free: `slot_run` runs them directly, without a slot.
#
# The logic lives HERE and not in a file this one sources, for the tamper-check reason below;
# scripts/build-slot.sh is a thin wrapper that sources this file and calls `slot_run`.
#
#   NUCLEOS_BUILD_SLOTS_DIR      absolute; default $HOME/.nucleos/build-slots
#   NUCLEOS_BUILD_SLOTS          integer; default 2; 0 = bypass (run the command directly)
#   NUCLEOS_BUILD_SLOT_TIMEOUT   seconds to wait for a slot; default 1800; then exit 75
#   NUCLEOS_BUILD_SLOT_HELD      set by a holder; a nested call under it takes no second slot
_slot_file=""

slot_release() {
  [ -n "$_slot_file" ] && rm -f "$_slot_file"
  _slot_file=""
}

_slot_holders() {
  # One "<pid> <cwd>" per live holder, for the waiting message.
  local f
  for f in "$1"/held/*; do
    [ -f "$f" ] || continue
    printf ' %s %s' "$(basename "$f")" "$(sed -n 3p "$f")"
  done
}

_self_pid() {
  # Sets _pid to the pid of the CURRENT (sub)shell, which is what $BASHPID means. macOS ships bash
  # 3.2, which has no BASHPID, and `set -u` made the first read of it end the gate there. The
  # fallback relies on `exec`: the command substitution's subshell becomes sh, so sh's parent is
  # the shell that asked.
  if [ -n "${BASHPID:-}" ]; then
    _pid="$BASHPID"
  else
    _pid="$(exec sh -c 'echo "$PPID"')"
  fi
}

_slot_try() {
  # _slot_try <dir> <n> <command...>: one attempt, under the mutex. 0 = slot claimed.
  local dir="$1" n="$2" f pid count=0 got=1
  shift 2
  if ! mkdir "$dir/.mutex" 2>/dev/null; then
    pid="$(cat "$dir/.mutex/pid" 2>/dev/null)"
    if { [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; } \
      || [ -n "$(find "$dir/.mutex" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
      # Break it under a breaker lock: two callers that both judged it stale must not both
      # delete it, or the second removes the first's fresh mutex and both hold it.
      mkdir "$dir/.mutex.break" 2>/dev/null || {
        [ -n "$(find "$dir/.mutex.break" -maxdepth 0 -mmin +1 2>/dev/null)" ] && rm -rf "$dir/.mutex.break"
        return 1
      }
      pid="$(cat "$dir/.mutex/pid" 2>/dev/null)"
      if { [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; }         || [ -n "$(find "$dir/.mutex" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
        rm -rf "$dir/.mutex"
      fi
      mkdir "$dir/.mutex" 2>/dev/null; got=$?
      rm -rf "$dir/.mutex.break"
      [ "$got" -eq 0 ] || return 1
      got=1
    else
      return 1
    fi
  fi
  _self_pid
  printf '%s\n' "$_pid" > "$dir/.mutex/pid"
  for f in "$dir"/held/*; do
    [ -f "$f" ] || continue
    pid="$(basename "$f")"
    if kill -0 "$pid" 2>/dev/null; then
      count=$((count + 1))
    else
      rm -f "$f"
      echo "build-slot: reaped slot of dead pid $pid" >&2
    fi
  done
  if [ "$count" -lt "$n" ]; then
    printf '%s\n%s\n%s\n%s\n' "$_pid" "$(date +%s)" "$(pwd)" "$*" > "$dir/held/$_pid"
    _slot_file="$dir/held/$_pid"
    got=0
  fi
  rm -rf "$dir/.mutex"
  return "$got"
}

heavy_broker() {
  # heavy_broker <main>: true when <main> has the broker (both files) AND this machine runs it.
  # The files are tracked, so their presence alone would switch it on in CI and in every fresh
  # clone; the machine opts in by having the broker's state dir (created by its first run here,
  # ${NUCLEOS_HEAVY_DIR:-~/.nucleos/heavy}) or by setting NUCLEOS_HEAVY=1.
  [ -f "$1/scripts/heavy.py" ] && [ -f "$1/scripts/heavy_classify.py" ] || return 1
  [ "${NUCLEOS_HEAVY:-}" = 1 ] && return 0
  [ -d "${NUCLEOS_HEAVY_DIR:-${HOME:-/nonexistent}/.nucleos/heavy}" ]
}

# This file's directory, resolved once when it is read: steps run in subdirectories (`run` cds into
# shell/, sidecars/...), where a relative BASH_SOURCE no longer leads here.
gates_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && { pwd -W 2>/dev/null || pwd; })"

heavy_entry() {
  # heavy_entry <main>: the heavy.py to run. <main>'s, unless it predates the single broker and this
  # checkout's copy forwards to master's staged one (`_staged_broker`): the main checkout is the
  # owner's and can sit on a branch from before that, which kept every gate on an old broker. An
  # explicit NUCLEOS_HEAVY_MAIN is taken as it is. The path is one the native python can open.
  local own=""
  if [ -z "${NUCLEOS_HEAVY_MAIN:-}" ] && ! grep -q _staged_broker "$1/scripts/heavy.py" 2>/dev/null; then
    own="$gates_dir/heavy.py"
    grep -q _staged_broker "$own" 2>/dev/null || own=""
  fi
  printf '%s\n' "${own:-$1/scripts/heavy.py}"
}

heavy_run() {
  # heavy_run <command...>: hand the command to the machine-wide broker (<main>/scripts/heavy.py)
  # when the main checkout has one; otherwise fall back to the build slot (cargo) or run it plainly.
  # NUCLEOS_HEAVY_MAIN overrides the main checkout, NUCLEOS_HEAVY_PYTHON the interpreter. See
  # heavy_broker for when the broker counts: a CI run or a fresh clone keeps the old behaviour.
  local main py
  main="${NUCLEOS_HEAVY_MAIN:-$(dirname "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)")}"
  if [ "${OS:-}" = Windows_NT ]; then py="${NUCLEOS_HEAVY_PYTHON:-python}"; else py="${NUCLEOS_HEAVY_PYTHON:-python3}"; fi
  if heavy_broker "$main"; then
    "$py" "$(heavy_entry "$main")" -- "$@"
  elif [ "$1" = cargo ] || [ -n "${heavy_slot_any:-}" ]; then
    # build-slot.sh sets heavy_slot_any: whatever it is asked to run takes a slot, as before.
    slot_run "$@"
  else
    "$@"
  fi
}

slot_run() {
  # slot_run <command...>: run it while holding one of N build slots. Never `exec`s: the holder pid
  # must outlive the command, because that pid is what proves the slot is still in use.
  local dir="${NUCLEOS_BUILD_SLOTS_DIR:-${HOME:-}/.nucleos/build-slots}"
  local n="${NUCLEOS_BUILD_SLOTS:-2}" timeout="${NUCLEOS_BUILD_SLOT_TIMEOUT:-1800}"
  local waited=0 child="" status
  # `cargo check` and `cargo clippy` never take a slot. Measured 2026-10-02: a 15s clippy waited 49
  # minutes behind two other sessions' long suites. With sccache they are cheap; the slot exists to
  # protect the heavy build and test steps, not these.
  if [ "$1" = cargo ]; then
    case "$2" in check|clippy) "$@"; return $? ;; esac
  fi
  case "$n" in
    ''|*[!0-9]*) echo "build-slot: NUCLEOS_BUILD_SLOTS must be a non-negative integer, got '$n'" >&2; return 2 ;;
  esac
  case "$timeout" in
    ''|*[!0-9]*) echo "build-slot: NUCLEOS_BUILD_SLOT_TIMEOUT must be a non-negative integer, got '$timeout'" >&2; return 2 ;;
  esac
  case "$dir" in
    /?*|[A-Za-z]:[/\\]?*) ;;
    *) echo "build-slot: NUCLEOS_BUILD_SLOTS_DIR must be an absolute path, got '$dir'" >&2; return 2 ;;
  esac
  if [ "$n" -eq 0 ] || [ -n "${NUCLEOS_BUILD_SLOT_HELD:-}" ]; then
    "$@"
    return $?
  fi
  mkdir -p "$dir/held" || return 2
  until _slot_try "$dir" "$n" "$@"; do
    if [ "$waited" -ge "$timeout" ]; then
      echo "build-slot: gave up after ${timeout}s; held by:$(_slot_holders "$dir")" >&2
      return 75
    fi
    if [ $((waited % 60)) -eq 0 ]; then
      echo "build-slot: waiting for a build slot ($n of $n held:$(_slot_holders "$dir"))" >&2
    fi
    sleep 1
    waited=$((waited + 1))
  done
  _self_pid
  export NUCLEOS_BUILD_SLOT_HELD="$_pid"
  trap slot_release EXIT
  # The command runs in the background and is waited on: bash defers a trap until a FOREGROUND
  # child ends, so a TERM would otherwise leave the slot held for as long as the build ran.
  # `<&0` keeps stdin, which a bare `&` would replace with /dev/null.
  trap 'kill "$child" 2>/dev/null; slot_release; exit 130' INT
  trap 'kill "$child" 2>/dev/null; slot_release; exit 143' TERM
  "$@" <&0 &
  child=$!
  wait "$child"
  status=$?
  slot_release
  trap - EXIT INT TERM
  return "$status"
}

# Whether a gate area holds the worktree for its whole run (`heavy.py hold-worktree`, below). Only
# the areas that build Rust do: the hold pins the cargo target slot, and holding it for an area that
# never compiles only queues that area behind whoever else holds the worktree.
#   core, tauri  run the Rust build/clippy/test steps.
#   sidecars     runs only gofmt, go vet and go test.
#   shell        runs tsc -b, vitest and scripts/csp-gate.mjs (vite into its own shell/dist-csp-gate,
#                no Rust build; the `os error 32` gotcha belongs to the `tauri` area).
#   hooks        runs hermetic python tests on temp dirs.
#   security     runs the advisory audit (reads Cargo.lock, builds nothing) and gitleaks.
#   broker       drives brokers of its own.
# Their heavy steps still reach the broker one by one (`heavy_run`), which admits them against the
# machine budget and takes no worktree lock for a non-Rust command. Why it matters: on 2026-11-01 the
# daemon started core/hooks/shell/sidecars at once in the main checkout, and hooks, shell and
# sidecars waited ~25 min behind core on an idle machine.
gate_holds_worktree() {
  case "$1" in
    core|tauri|all) return 0 ;;
    *) return 1 ;;
  esac
}

# Sourced rather than run: stop here, with the functions above defined and no gate started. That is
# how scripts/test-gates-summary.py drives them with fake steps. They live in this file and not in
# one it sources because the daemon's tamper check (`worktree_scripts` in core/src/gate.rs) compares
# only the files the gate command names — a sourced helper would be the one part of the gate a run
# could rewrite unseen, and it would be the part that decides between `ok` and `FAIL`.
if [ "${BASH_SOURCE[0]}" != "$0" ]; then
  return 0
fi

# Only the daemon's final safety-net trees run the full suite unconditionally: the post-merge gate
# tree (.nucleos/worktrees/postgate-<project>) and the merge queue's (.nucleos/worktrees/integration-*).
# There, tell the broker not to narrow a test command to the previously failed tests
# (scripts/heavy_failfirst.py). In an agent's own worktree a re-run after a failure should run only
# the failed tests: that is the waste failed-first exists to cut. A caller-set NUCLEOS_HEAVY_FULL
# wins either way. Failures are still recorded in every tree.
if [ -z "${NUCLEOS_HEAVY_FULL+x}" ]; then
  gate_top="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
  case "$gate_top" in
    */.nucleos/worktrees/postgate-*|*/.nucleos/worktrees/integration-*) export NUCLEOS_HEAVY_FULL=1 ;;
  esac
fi

target="${1:-all}"
case "$target" in
  core|sidecars|shell|tauri|hooks|security|broker|all) ;;
  *) echo "usage: $0 [core|sidecars|shell|tauri|hooks|security|broker|all]" >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

# The whole gate is one broker session (`heavy.py hold-worktree`), so the cargo target slot its
# first cargo step leases stays its own until the gate ends. Each step is a separate broker run, and
# between clippy and test another worktree used to take the slot; the gate's next step then rebuilt
# the crate from clean, at a median of 453s against 202s on a slot that had not changed hands
# (broker log, 2026-10-03..08). Skipped when a session already holds this run (the test selector
# opens one), when the broker is off, and where there is no broker (CI, a fresh clone).
# Only the areas that build Rust hold one, see gate_holds_worktree.
if [ -z "${NUCLEOS_HEAVY_HELD:-}" ] && [ "${NUCLEOS_HEAVY:-}" != 0 ] && gate_holds_worktree "$target"; then
  heavy_main="${NUCLEOS_HEAVY_MAIN:-$(dirname "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)")}"
  if [ "${OS:-}" = Windows_NT ]; then heavy_py="${NUCLEOS_HEAVY_PYTHON:-python}"; else heavy_py="${NUCLEOS_HEAVY_PYTHON:-python3}"; fi
  if heavy_broker "$heavy_main"; then
    # The broker is a native program: under Git bash `$BASH` is `/usr/bin/bash`, which it cannot open.
    heavy_bash="$BASH"
    command -v cygpath >/dev/null 2>&1 && heavy_bash="$(cygpath -m "$BASH")"
    exec "$heavy_py" "$(heavy_entry "$heavy_main")" hold-worktree -- "$heavy_bash" "$repo_root/scripts/gates.sh" "$target"
  fi
fi

# Said before the suite runs, not after. The worktree tests assert a space-free checkout path and
# produce ~25 failures when they don't get one — failures whose text points at the tests, so the
# path never comes up unless something names it.
case "$repo_root" in
  *\ *) echo "warning: repo path contains a space ($repo_root) — the worktree tests will fail on it" >&2 ;;
esac

gofmt_gate() {
  # Runs in whatever directory `run` cd'd into. `gofmt -l` lists unformatted files and still
  # exits 0, so the listing itself is the verdict.
  local unformatted
  unformatted="$(gofmt -l .)" || return 1
  [ -z "$unformatted" ] && return 0
  echo "gofmt would rewrite:" >&2
  printf '%s\n' "$unformatted" >&2
  # Every file at once almost always means CRLF, not formatting: gofmt counts a CR as part of the
  # line. .gitattributes pins *.go to LF, but a checkout taken before that landed keeps the CRs.
  echo "hint: if that is every file, re-checkout — .gitattributes pins *.go to LF" >&2
  return 1
}

if [ "$target" = core ] || [ "$target" = all ]; then
  run "core: fmt"    . cargo fmt --all -- --check
  # `cargo fmt` compiles nothing and takes no --target-dir, so it needs no flag.
  gate_build_args
  run "core: clippy" . cargo clippy ${gate_args[@]+"${gate_args[@]}"} --all-targets -- -D warnings
  # Every target (lib, bin, tests/): `--lib` alone would skip the binary's http.rs tests. And every
  # workspace crate under core/: `-p nucleos-core` alone would skip the modules moved down to
  # core/crates/nucleos-base, the ones moved up to core/crates/nucleos-app, and the daemon binary
  # (core/crates/nucleos-daemon).
  #
  # cargo-nextest when it is installed: one process per test, scheduled across every core. Measured
  # 2026-10-07 on master, both under the same load: `cargo test` ran the suite in ~37 min (median
  # of the broker log), nextest in 15.5 min — libtest's binaries run one after another and each
  # waits for its slowest test. `--no-fail-fast` keeps `cargo test`'s report of every failure.
  # Without nextest (CI, a fresh machine) the gate is what it was.
  if cargo nextest --version >/dev/null 2>&1; then
    gate_threads nextest
    run "core: test"   . cargo nextest run ${gate_args[@]+"${gate_args[@]}"} -p nucleos-base -p nucleos-core -p nucleos-app -p nucleos-daemon --no-fail-fast ${gate_tt[@]+"${gate_tt[@]}"}
  else
    gate_threads libtest
    run "core: test"   . cargo test ${gate_args[@]+"${gate_args[@]}"} -p nucleos-base -p nucleos-core -p nucleos-app -p nucleos-daemon ${gate_tt[@]+"${gate_tt[@]}"}
  fi
fi

if [ "$target" = sidecars ] || [ "$target" = all ]; then
  # Discovered, not listed, so adding a sidecar doesn't mean remembering to edit this file.
  for gomod in "$repo_root"/sidecars/*/go.mod; do
    [ -e "$gomod" ] || continue
    dir="$(dirname "$gomod")"
    name="$(basename "$dir")"
    run "sidecars/$name: gofmt" "$dir" gofmt_gate
    run "sidecars/$name: vet"   "$dir" go vet ./...
    # -race because these are the system's concurrent parts: pollers, long-polling update loops,
    # and a notifier goroutine sharing state with them.
    run "sidecars/$name: test"  "$dir" go test -race ./...
  done
  # Shared Go libraries the sidecars depend on (sidecars/lib/*), gated the same way.
  for gomod in "$repo_root"/sidecars/lib/*/go.mod; do
    [ -e "$gomod" ] || continue
    dir="$(dirname "$gomod")"
    name="lib/$(basename "$dir")"
    run "sidecars/$name: gofmt" "$dir" gofmt_gate
    run "sidecars/$name: vet"   "$dir" go vet ./...
    run "sidecars/$name: test"  "$dir" go test -race ./...
  done
fi

if [ "$target" = shell ] || [ "$target" = all ]; then
  if [ ! -d shell/node_modules ]; then
    echo "shell/node_modules missing — run (cd shell && npm ci) first" >&2
    failures="$failures  shell: deps not installed"$'\n'
  else
    # `tsc -b`, not `tsc --noEmit`: the project uses references, and `-b --noEmit` is rejected
    # outright ("referenced project may not disable emit").
    run "shell: typecheck" shell npx tsc -b
    # Cap vitest's fork count for the gate. Its default is cores-1 (31 forks on a 32-core box) and
    # every test file starts a fresh fork. Beside cargo builds that oversubscribes the CPU until a
    # fork cannot report "started" within vitest's hard-coded 60s START_TIMEOUT (in its cli-api
    # chunk, not a config option), which surfaces as "[vitest-pool]: Failed to start forks worker"
    # although every test passes. A quarter of the cores, clamped to 2..8, avoids that.
    # VITEST_MAX_WORKERS is vitest's own override; an exported value wins. Local `npm test` keeps
    # the default.
    if [ -z "${VITEST_MAX_WORKERS:-}" ]; then
      cores="$(nproc 2>/dev/null || echo "${NUMBER_OF_PROCESSORS:-4}")"
      w=$((cores / 4)); [ "$w" -lt 2 ] && w=2; [ "$w" -gt 8 ] && w=8
      export VITEST_MAX_WORKERS="$w"
    fi
    run "shell: test"      shell npm test
    # The one property no test runner can check, because a Content-Security-Policy is enforced by
    # the engine and jsdom does not enforce one. It matters here more than it would elsewhere:
    # `tauri.conf.json` carries a permissive `devCsp` alongside the strict `csp`, so a dependency
    # that injects a <style> works every day in `tauri dev` and is refused the first time somebody
    # installs a build. Three of this app's surfaces were in exactly that state when this gate
    # first ran. Builds its own bundle (~30s) rather than trusting one on disk.
    run "shell: csp"       . node scripts/csp-gate.mjs
  fi
fi

if [ "$target" = tauri ] || [ "$target" = all ]; then
  # shell/src-tauri is deliberately excluded from the cargo workspace (see the root Cargo.toml),
  # which means the root `cargo fmt --all`, `cargo clippy --all-targets` and `cargo test
  # -p nucleos-core` every one of them miss it. Until these three lines existed its Rust side was
  # compiled only as a side effect of `npm run tauri build`, and its tests were never run at all --
  # so a test written there was code nobody executed.
  #
  # Guarded on shell/dist because `tauri::generate_context!()` embeds the built frontend at COMPILE
  # time: without it the crate does not compile, and the gate would report a Rust failure for a
  # missing frontend. A fresh clone has to build the frontend once before the Rust side will build.
  if [ ! -d shell/dist ]; then
    echo "shell/dist missing — run (cd shell && npm run build) first; src-tauri embeds it at compile time" >&2
    failures="$failures  shell/src-tauri: frontend not built"$'\n'
  else
    run "shell/src-tauri: fmt"    shell/src-tauri cargo fmt --all -- --check
    run "shell/src-tauri: clippy" shell/src-tauri cargo clippy --all-targets -- -D warnings
    run "shell/src-tauri: test"   shell/src-tauri cargo test
  fi
fi

# The Python this repo ships. In `all`, unlike `security`, because it is offline and hermetic and
# because being outside the everyday command is exactly how it went uncovered.
#
# `core/hooks/ask_daemon.py` is not a helper script: `core/src/autopilot.rs` and `core/src/triage.rs`
# both `include_str!` it (`.claude/hooks/ask_daemon.py` is a byte copy the tests hold equal), so it
# is compiled INTO the daemon, and its filter decides whether a git operation a person
# types is refused and sent to the queue. It shipped with a hole that a `cd` walked through — the
# filter read only the first two tokens of the command — and nothing here would have noticed,
# because nothing here ran it. Found by accident, in use.
if [ "$target" = hooks ] || [ "$target" = all ]; then
  # `python3` first: it is the name on CI images and on Linux, while Windows installs generally
  # answer to `python`.
  #
  # RUN, don't locate. Windows ships an App Execution Alias at
  # `AppData/Local/Microsoft/WindowsApps/python3` that is not an interpreter: it prints "Python was
  # not found" and exits 49. `command -v` finds it, so a check that only looks for the name picks
  # the stub over the real Python installed beside it and the whole leg fails with a message about
  # the Microsoft Store. Measured on this machine, first run of this gate.
  py=""
  for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c "" >/dev/null 2>&1; then
      py="$candidate"
      break
    fi
  done
  if [ -z "$py" ]; then
    echo "python missing — the hook filter and eval approver need it (scripts/doctor.sh reports this)" >&2
    failures="$failures  hooks: python not installed"$'\n'
  else
    run "hooks: filter"   . "$py" scripts/test-hook-filter.py
    run "hooks: conductor" . "$py" scripts/test-conductor-hook.py
    run "gates: summary"  . "$py" scripts/test-gates-summary.py
    run "gates: own target" . "$py" scripts/test-own-cargo-target.py
    run "gates: build slot" . "$py" scripts/test-build-slot.py
    run "daemon: from copy" . "$py" scripts/test-run-daemon.py
    # Hermetic like its neighbours: the network is behind one seam the test swaps out, so this
    # runs green on a machine with no route to OpenRouter at all.
    run "models: refresh"   . "$py" scripts/test-refresh-models.py
    run "git: worktree sync" . "$py" scripts/test-post-checkout.py
    run "migrate: ai layout" . "$py" scripts/test-migrate-ai-layout.py
    run "git: reference-transaction" . "$py" scripts/test-reference-transaction.py
    # One version for shell, core and sidecars: every manifest must agree with VERSION.
    run "release: versions" . "$py" scripts/bump-version.py --check
    run "release: bump"     . "$py" scripts/test-bump-version.py
    run "release: manifest" . "$py" scripts/test-release-manifest.py
    run "release: install"  . "$py" scripts/test-install-scripts.py
    # The scripts of the bundled `nucleos-dev` workflow, which the daemon packages and every
    # project's controller runs from its library copy. Their tests used to run only from the
    # gitignored `.ai/` catalog, which a fresh clone does not have.
    for bundle_test in core/assets/nucleos-dev/scripts/test_*.py; do
      run "workflow: $(basename "$bundle_test" .py)" . "$py" "$bundle_test"
    done
  fi
fi

# The stack targets are offline and hermetic; these are neither. `cargo audit` fetches RustSec's
# advisory database, and both need tools the other gates do not, so putting them in `all` would make
# the everyday local command need network access and two extra installs.
if [ "$target" = security ]; then
  if ! command -v cargo-audit >/dev/null 2>&1; then
    echo "cargo-audit missing — run cargo install cargo-audit --locked first" >&2
    failures="$failures  security: cargo-audit not installed"$'\n'
  else
    # Advisories the project has consciously accepted, one `--ignore <ID>` per entry. Kept short on
    # purpose: a silent ignore hides a real finding, so every ID added here needs a reason beside it.
    # TODO(controller): after a real `cargo audit` run, list the RUSTSEC ids of the unmaintained
    # gtk3-family crates (pulled in by tauri/wry on Linux) here, each with a one-line reason and a
    # revisit date. Do not guess ids.
    audit_ignore=(
      # RUSTSEC-2026-0459: libsqlite3-sys 0.37.0 bundles a C SQLite with CVE-2026-119375 (medium,
      # 6.8); fixed in libsqlite3-sys >=0.39.0. Not fixable yet: it arrives only through sqlx 0.9.0,
      # whose sqlx-sqlite caps libsqlite3-sys at <0.38 (sqlx main widens that to <0.40, unreleased).
      # Accepted for now: exploiting it needs the app to open an attacker-written .db together with
      # a -wal of a different page size, and NucleOS opens only databases it created itself.
      # Remove once sqlx releases a version allowing libsqlite3-sys >=0.39; review by 2026-12-09.
      RUSTSEC-2026-0459
    )
    audit_args=()
    for id in ${audit_ignore[@]+"${audit_ignore[@]}"}; do audit_args+=(--ignore "$id"); done
    run "security: deps" . cargo audit ${audit_args[@]+"${audit_args[@]}"}
    # shell/src-tauri is outside the workspace (see Cargo.toml `exclude`) and has its own
    # Cargo.lock, which the workspace audit above never reads.
    run "security: deps (shell/src-tauri)" . cargo audit --file shell/src-tauri/Cargo.lock ${audit_args[@]+"${audit_args[@]}"}
  fi

  # Known-vulnerable Go dependencies, per module (sidecars/* and sidecars/lib/*), reachability-aware.
  if ! command -v govulncheck >/dev/null 2>&1; then
    echo "govulncheck missing — run go install golang.org/x/vuln/cmd/govulncheck@v1.9.0 first" >&2
    failures="$failures  security: govulncheck not installed"$'\n'
  else
    for gomod in "$repo_root"/sidecars/*/go.mod "$repo_root"/sidecars/lib/*/go.mod; do
      [ -e "$gomod" ] || continue
      dir="$(dirname "$gomod")"
      run "security: govulncheck ${dir#"$repo_root"/}" "$dir" govulncheck ./...
    done
  fi

  # Production dependencies of the shell only: dev tooling does not ship.
  if ! command -v npm >/dev/null 2>&1; then
    echo "npm missing — install Node to audit the shell's JavaScript dependencies" >&2
    failures="$failures  security: npm not installed"$'\n'
  else
    run "security: npm audit (shell)" shell npm audit --audit-level=high --omit=dev
  fi

  if ! command -v gitleaks >/dev/null 2>&1; then
    echo "gitleaks missing — run go install github.com/zricethezav/gitleaks/v8@v8.30.0 first" >&2
    failures="$failures  security: gitleaks not installed"$'\n'
  else
    # Never print a detected secret into a CI log: `--redact` is part of this gate's contract.
    run "security: secrets" . gitleaks detect --redact --no-banner
  fi
fi

# The build broker's own tests (scripts/heavy.py and its hook). Not in `all`: they take about six
# minutes, almost all of it real waiting on queues, locks and child processes, and they cover a
# tool only the owner's machine runs; CI runs this target on its own Windows leg.
if [ "$target" = broker ]; then
  py=""
  for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c "" >/dev/null 2>&1; then
      py="$candidate"
      break
    fi
  done
  if [ -z "$py" ]; then
    echo "python missing — the broker tests need it (scripts/doctor.sh reports this)" >&2
    failures="$failures  broker: python not installed"$'\n'
  else
    # NUCLEOS_HEAVY_LOCAL: the code under test is this checkout's broker, not master's staged one.
    NUCLEOS_HEAVY_LOCAL=1 run "broker: tests" . "$py" -m unittest discover -s scripts/heavy_tests -p 'test_heavy_*.py'
  fi
fi

print_summary || exit 1
