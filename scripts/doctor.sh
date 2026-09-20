#!/usr/bin/env bash
# Environment diagnostics for NucleOS development (spec §7.1). Checks that every toolchain this
# monorepo builds with is present, and reports the active Rust host toolchain (GNU fallback today,
# per spec §7.1). Exits non-zero on the first missing tool with an actionable hint, rather than
# letting a later build fail cryptically.
#
# Usage: scripts/doctor.sh [--fix]
#
# `--fix` repairs what can be repaired from inside a script, and says so line by line. That is a
# short list on purpose: it installs the commit guards, and nothing else. It does not install
# toolchains — a diagnostic that silently puts a compiler on someone's machine is no longer a
# diagnostic — and it cannot change the PATH of the shell that invoked it, so the two PATH findings
# below print the exact line to run instead of pretending to fix themselves.
set -u

fix=0
for arg in "$@"; do
  case "$arg" in
    --fix) fix=1 ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) printf 'doctor: unknown argument %s (try --help)\n' "$arg" >&2; exit 2 ;;
  esac
done

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

# `cargo` is installed on the primary development machine but deliberately not on PATH, so the bare
# "install it" hint sends the reader to rustup for a toolchain they already have. Told apart here
# because the two findings have nothing in common but their symptom.
cargo_hint="install via https://rustup.rs"
if [ -x "${HOME:-}/.cargo/bin/cargo" ] && ! command -v cargo >/dev/null 2>&1; then
  cargo_hint='installed but not on PATH; prepend it: PATH="$HOME/.cargo/bin:$PATH"'
fi

need rustc rustc "install via https://rustup.rs"
need cargo cargo "$cargo_hint"
need go    go    "install Go 1.2x from https://go.dev/dl"
need node  node  "install Node.js 20+ from https://nodejs.org"
need npm   npm   "ships with Node.js"

# Python is part of the definition of green, which is easy to miss because none of the product is
# written in it. `scripts/gates.sh hooks` runs the cover for `core/hooks/ask_daemon.py` — a file
# `core/src/autopilot.rs` and `core/src/triage.rs` compile INTO the daemon and which decides what a
# person's git commands are allowed to do. Checked under both names: `python3` on Linux and CI,
# `python` on most Windows installs, and a machine with neither cannot run the gate.
#
# RUN it rather than locate it. Windows ships an App Execution Alias named `python3` that is not an
# interpreter — it prints "Python was not found", points at the Microsoft Store, and exits 49.
# `command -v` finds it happily, and the first version of these three lines reported
# `ok python Python não foi encontrado...`, which is a false green with the failure text sitting
# inside it.
python_found=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c "" >/dev/null 2>&1; then
    python_found="$candidate"
    break
  fi
done
if [ -n "$python_found" ]; then
  printf 'ok   python %s (%s)\n' "$("$python_found" --version 2>&1 | head -n1)" "$python_found"
else
  printf 'MISS python no working interpreter as python3 or python — scripts/gates.sh hooks needs one; install from https://python.org\n'
  fail=1
fi

# Nine tests in `gate::` and `transcribe::` spawn `echo` as a PROGRAM. A shell builtin does not
# satisfy them, and on Windows the only real echo.exe is Git's, under /usr/bin. Checked with
# `type -P`, which reports the external executable and ignores the builtin — the whole distinction
# this finding is about.
#
# Reported as a MISS rather than a warning even though nothing here stops a build: what it stops is
# the gate, and it stops it by failing nine tests with `program not found`. That reads like a broken
# repository, sends the reader looking for a defect that is not there, and each panicking test skips
# its cleanup, so every such run also strands temp directories. Preempting exactly that is what this
# script is for.
if type -P echo >/dev/null 2>&1; then
  printf 'ok   echo   %s (the test suite spawns it as a program)\n' "$(type -P echo)"
else
  printf 'MISS echo   no echo PROGRAM on PATH — nine gate:: and transcribe:: tests spawn one (coreutils provides /usr/bin/echo)\n'
  fail=1
fi

# And on Windows that line does not mean what it looks like it means, so it is followed by one that
# says so. Under MSYS this script always sees Git's own /usr/bin, whatever the Windows PATH holds —
# but a Rust test spawns `echo` through the WINDOWS PATH of the shell that launched cargo, which is
# usually PowerShell and usually does not carry Git's usr/bin. The two are not the same list and
# this script cannot read the second one, so it states the fact instead of pretending to check it.
# Worth a line every run because of how the failure presents: nine tests reporting `program not
# found`, which reads like a broken repository and is not, plus a stranded temp directory for each
# panicking test.
case "$(uname -s 2>/dev/null || echo unknown)" in
  MINGW*|MSYS*|CYGWIN*)
    printf 'info echo   cargo test spawns it through the WINDOWS PATH, which this script cannot see; if nine gate::/transcribe:: tests say "program not found", prepend: $env:PATH = "C:\\Program Files\\Git\\usr\\bin;$env:PATH"\n' ;;
esac

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
#
# This is the one finding `--fix` acts on, because the repair is a committed recipe
# (scripts/install-hooks.sh), it is idempotent, and everything it touches is local to this clone.
root="$(git rev-parse --show-toplevel 2>/dev/null || echo .)"
if [ "$(git config core.hooksPath 2>/dev/null || true)" = ".githooks" ] &&
   [ -f "$root/.githooks/pre-commit" ]; then
  printf 'ok   hooks  core.hooksPath=.githooks, commit guards present\n'
elif [ "$fix" -eq 1 ]; then
  printf 'fix  hooks  commit guards inert — running scripts/install-hooks.sh\n'
  if bash "$root/scripts/install-hooks.sh"; then
    printf 'ok   hooks  installed\n'
  else
    printf 'MISS hooks  scripts/install-hooks.sh failed — see the output above\n'
    fail=1
  fi
else
  printf 'warn hooks  commit guards inert — run scripts/install-hooks.sh (or this script with --fix)\n'
fi

# Temp directories stranded by panicking tests. Reported and never removed, even under `--fix`:
# deleting other people's files is not a diagnostic's job, the exact command is one line, and a
# directory left behind by a test that panicked may still be the evidence of why it panicked.
if [ -n "${TMPDIR:-${TEMP:-${TMP:-}}}" ] || [ -d /tmp ]; then
  tmp_root="${TMPDIR:-${TEMP:-${TMP:-/tmp}}}"
  stranded=$(find "$tmp_root" -maxdepth 1 -type d -name 'nucleos-*' 2>/dev/null | wc -l | tr -d ' ')
  if [ "${stranded:-0}" -gt 0 ]; then
    printf 'warn temp   %s stranded nucleos-* dir(s) in %s — from tests that panicked before cleanup; remove with: find "%s" -maxdepth 1 -type d -name "nucleos-*" -exec rm -rf {} +\n' \
      "$stranded" "$tmp_root" "$tmp_root"
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo "doctor: environment is INCOMPLETE — resolve the MISS lines above before building." >&2
  [ "$fix" -eq 1 ] && echo "doctor: --fix installs the commit guards only; it does not install toolchains or edit your PATH." >&2
  exit 1
fi
echo "doctor: environment OK."
