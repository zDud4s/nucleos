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

failures=""

run() {
  # run <label> <dir> <command...>
  local label="$1" dir="$2"
  shift 2
  printf '\n=== %s ===\n' "$label"
  if ( cd "$dir" && "$@" ); then
    printf 'ok   %s\n' "$label"
  else
    printf 'FAIL %s\n' "$label"
    failures="$failures  $label"$'\n'
  fi
}

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
# `.claude/hooks/ask_daemon.py` is not a helper script: `core/src/triage.rs` does `include_str!` on
# it, so it is compiled INTO the daemon, and its filter decides whether a git operation a person
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
    echo "python missing — the hook filter, eval approver and usage-split tests need it (scripts/doctor.sh reports this)" >&2
    failures="$failures  hooks: python not installed"$'\n'
  else
    run "hooks: filter"   . "$py" scripts/test-hook-filter.py
    run "hooks: evidence" . "$py" scripts/test-evidence-gate.py
    run "eval: approver"  . "$py" scripts/eval/test-auto-approve.py
    run "usage: split"    . "$py" scripts/test-usage-split.py
    run "usage: statusline" . "$py" scripts/test-statusline-context.py
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

if [ -n "$failures" ]; then
  printf '\ngates FAILED:\n%s' "$failures" >&2
  exit 1
fi
printf '\nall gates green.\n'
