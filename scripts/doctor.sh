#!/usr/bin/env bash
# Environment diagnostics for NucleOS development (spec §7.1). Checks that every toolchain this
# monorepo builds with is present, and reports the active Rust host toolchain (GNU fallback today,
# per spec §7.1). Exits non-zero on the first missing tool with an actionable hint, rather than
# letting a later build fail cryptically.
set -u

fail=0

need() {
  # need <command> <label> <install-hint>
  if command -v "$1" >/dev/null 2>&1; then
    # Go uses `go version` (it rejects `--version`); everything else uses `--version`.
    if [ "$1" = go ]; then
      ver=$("$1" version 2>&1 | head -n1)
    else
      ver=$("$1" --version 2>&1 | head -n1)
    fi
    printf 'ok   %-6s %s\n' "$2" "$ver"
  else
    printf 'MISS %-6s not on PATH — %s\n' "$2" "$3"
    fail=1
  fi
}

need rustc rustc "install via https://rustup.rs"
need cargo cargo "install via https://rustup.rs"
need go    go    "install Go 1.2x from https://go.dev/dl"
need node  node  "install Node.js 20+ from https://nodejs.org"
need npm   npm   "ships with Node.js"

# Report (don't hard-fail on) the active Rust host toolchain and the repo pin — spec §7.1 keeps GNU
# an accepted fallback until the MSVC build tools are repaired, so a specific host is not required yet.
if command -v rustc >/dev/null 2>&1; then
  printf 'info rust host: %s\n' "$(rustc -vV | sed -n 's/^host: //p')"
  [ -f rust-toolchain.toml ] && printf 'info rust-toolchain.toml present (repo pins the active toolchain)\n'
fi

# The commit guards are untracked by design (.gitignore) and core.hooksPath is local config, so a
# fresh clone has neither and says nothing about it — a repo whose guards are inert looks exactly
# like one whose guards work, right up to the commit that should have been blocked. Reported as a
# warning, not a MISS: nothing here stops a build, so it must not fail the environment check.
root="$(git rev-parse --show-toplevel 2>/dev/null || echo .)"
if [ "$(git config core.hooksPath 2>/dev/null || true)" = ".githooks" ] &&
   [ -f "$root/.githooks/pre-commit" ]; then
  printf 'ok   hooks  core.hooksPath=.githooks, commit guards present\n'
else
  printf 'warn hooks  commit guards inert — run scripts/install-hooks.sh\n'
fi

if [ "$fail" -ne 0 ]; then
  echo "doctor: environment is INCOMPLETE — resolve the MISS lines above before building." >&2
  exit 1
fi
echo "doctor: environment OK."
