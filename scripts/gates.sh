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
# Usage: scripts/gates.sh [core|sidecars|shell|security|all]   (default: all)
set -uo pipefail

target="${1:-all}"
case "$target" in
  core|sidecars|shell|security|all) ;;
  *) echo "usage: $0 [core|sidecars|shell|security|all]" >&2; exit 2 ;;
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
