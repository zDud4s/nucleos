#!/usr/bin/env bash
# Run every gate this repo treats as its definition of green, for all three stacks.
#
# These commands used to live only in people's shell history, which is how a red suite and an
# AGENTS.md claiming "green" managed to coexist. This script is where they live now, and
# `.github/workflows/ci.yml` calls it rather than repeating them — one definition of green, run in
# two places.
#
# The workflow does not run yet: this repo has no remote, and Actions reads workflows server-side.
# It is committed anyway because it is the thing you need in place BEFORE the first push, not
# after — otherwise whoever adds the remote has to know to write it.
#
# Every stack runs even when an earlier one fails — a summary of three real failures beats
# stopping at the first and re-running twice to discover the other two.
#
# Usage: scripts/gates.sh [core|sidecars|shell|hooks|security|all]   (default: all)
set -uo pipefail

failures=""

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

run() {
  # run <label> <dir> <command...>
  local label="$1" dir="$2" capture=/dev/null status
  shift 2
  [ -n "$captures" ] && capture="$captures/step"
  printf '\n=== %s ===\n' "$label"
  # stderr joins stdout on its way into `tee`, because the kept lines need both in the order they
  # were printed: rustfmt's diff goes to one and cargo's `error:` to the other. The daemon already
  # reads the two as one stream. The cost: a step that leaves a background process holding its
  # output now holds the gate until that process lets go, since `tee` waits for every writer.
  # A heavy cargo subcommand takes a build slot (see `slot_run`). It is decided HERE and not written
  # on each gate line, because the classifier test reads those lines as plain commands.
  if [ "$1" = cargo ]; then
    case "$2" in build|check|clippy|test|run|doc) set -- slot_run "$@" ;; esac
  fi
  ( cd "$dir" && "$@" ) 2>&1 | tee "$capture"
  status="${PIPESTATUS[0]}"
  if [ "$status" -eq 0 ]; then
    printf 'ok   %s\n' "$label"
  else
    printf 'FAIL %s\n' "$label"
    failures="$failures  $label"$'\n'"$(kept_lines "$capture")"$'\n'
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

_slot_try() {
  # _slot_try <dir> <n> <command...>: one attempt, under the mutex. 0 = slot claimed.
  local dir="$1" n="$2" f pid count=0 got=1
  shift 2
  if ! mkdir "$dir/.mutex" 2>/dev/null; then
    pid="$(cat "$dir/.mutex/pid" 2>/dev/null)"
    if { [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; } \
      || [ -n "$(find "$dir/.mutex" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
      rm -rf "$dir/.mutex"
      mkdir "$dir/.mutex" 2>/dev/null || return 1
    else
      return 1
    fi
  fi
  printf '%s\n' "$BASHPID" > "$dir/.mutex/pid"
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
    printf '%s\n%s\n%s\n%s\n' "$BASHPID" "$(date +%s)" "$(pwd)" "$*" > "$dir/held/$BASHPID"
    _slot_file="$dir/held/$BASHPID"
    got=0
  fi
  rm -rf "$dir/.mutex"
  return "$got"
}

slot_run() {
  # slot_run <command...>: run it while holding one of N build slots. Never `exec`s: the holder pid
  # must outlive the command, because that pid is what proves the slot is still in use.
  local dir="${NUCLEOS_BUILD_SLOTS_DIR:-${HOME:-}/.nucleos/build-slots}"
  local n="${NUCLEOS_BUILD_SLOTS:-2}" timeout="${NUCLEOS_BUILD_SLOT_TIMEOUT:-1800}"
  local waited=0 child="" status
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
  export NUCLEOS_BUILD_SLOT_HELD="$BASHPID"
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

# Sourced rather than run: stop here, with the functions above defined and no gate started. That is
# how scripts/test-gates-summary.py drives them with fake steps. They live in this file and not in
# one it sources because the daemon's tamper check (`worktree_scripts` in core/src/gate.rs) compares
# only the files the gate command names — a sourced helper would be the one part of the gate a run
# could rewrite unseen, and it would be the part that decides between `ok` and `FAIL`.
if [ "${BASH_SOURCE[0]}" != "$0" ]; then
  return 0
fi

target="${1:-all}"
case "$target" in
  core|sidecars|shell|hooks|security|all) ;;
  *) echo "usage: $0 [core|sidecars|shell|hooks|security|all]" >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

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
  run "core: clippy" . cargo clippy --all-targets -- -D warnings
  # NOT `--lib`: nucleos-core is a bin-only crate and `--lib` errors out (core/AGENTS.md).
  run "core: test"   . cargo test -p nucleos-core
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
fi

if [ "$target" = shell ] || [ "$target" = all ]; then
  if [ ! -d shell/node_modules ]; then
    echo "shell/node_modules missing — run (cd shell && npm ci) first" >&2
    failures="$failures  shell: deps not installed"$'\n'
  else
    # `tsc -b`, not `tsc --noEmit`: the project uses references, and `-b --noEmit` is rejected
    # outright ("referenced project may not disable emit").
    run "shell: typecheck" shell npx tsc -b
    run "shell: test"      shell npm test
    # The one property no test runner can check, because a Content-Security-Policy is enforced by
    # the engine and jsdom does not enforce one. It matters here more than it would elsewhere:
    # `tauri.conf.json` carries a permissive `devCsp` alongside the strict `csp`, so a dependency
    # that injects a <style> works every day in `tauri dev` and is refused the first time somebody
    # installs a build. Three of this app's surfaces were in exactly that state when this gate
    # first ran. Builds its own bundle (~30s) rather than trusting one on disk.
    run "shell: csp"       . node scripts/csp-gate.mjs
  fi

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
    # F5: the packet-gate and cross-tool usage-accounting tests moved out of this target along
    # with the tools they cover, to `.ai/scripts/`: they are workflow tooling, not product
    # tooling, and this target — unlike `workflow` in `.ai/tests/catalog.yaml` — is a
    # `scripts/gates.sh` leg that CI can run on a fresh, gitignore-respecting clone. They are
    # covered by the `workflow` catalog group's own command instead (see
    # `.ai/tests/catalog.yaml`), which is why only product tests remain below.
    run "hooks: filter"   . "$py" scripts/test-hook-filter.py
    run "gates: summary"  . "$py" scripts/test-gates-summary.py
    run "gates: own target" . "$py" scripts/test-own-cargo-target.py
    run "gates: build slot" . "$py" scripts/test-build-slot.py
    run "eval: approver"  . "$py" scripts/eval/test-auto-approve.py
    run "eval: promote"   . "$py" scripts/eval/test-promote.py
    run "eval: ingest"    . "$py" scripts/eval/test-ingest.py
    run "eval: layer"     . "$py" scripts/eval/test-layer.py
    # Hermetic like its neighbours: the network is behind one seam the test swaps out, so this
    # runs green on a machine with no route to OpenRouter at all.
    run "models: refresh"   . "$py" scripts/test-refresh-models.py
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
    run "security: deps" . cargo audit
  fi

  if ! command -v gitleaks >/dev/null 2>&1; then
    echo "gitleaks missing — run go install github.com/zricethezav/gitleaks/v8@v8.30.0 first" >&2
    failures="$failures  security: gitleaks not installed"$'\n'
  else
    # Never print a detected secret into a CI log: `--redact` is part of this gate's contract.
    run "security: secrets" . gitleaks detect --redact --no-banner
  fi
fi

print_summary || exit 1
