#!/usr/bin/env bash
# Smoke-test a Linux release bundle: install the .deb/.rpm, check every binary landed, start the
# daemon, kill -9 it and prove a restart still works. Run inside a clean ubuntu:22.04 or fedora box.
#
# Usage: scripts/smoke-bundle.sh <dir with the downloaded release assets>
set -euo pipefail

dir="${1:-}"
[ -n "$dir" ] && [ -d "$dir" ] || { echo "usage: $0 <release-assets-dir>" >&2; exit 2; }

port=8791
home_dir="$(mktemp -d)"
log="$home_dir/daemon.log"
pid=""

fail() { echo "SMOKE FAIL: $*" >&2; [ -f "$log" ] && tail -n 40 "$log" >&2; exit 1; }
cleanup() { [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null || true; }
trap cleanup EXIT

SUDO=""
[ "$(id -u)" -eq 0 ] || SUDO="sudo"

# 1. Install whatever package this box can take.
if command -v apt-get >/dev/null 2>&1; then
  pkg="$(find "$dir" -maxdepth 1 -name '*.deb' | head -n 1)"
  [ -n "$pkg" ] || fail "no .deb in $dir"
  $SUDO apt-get update -y
  $SUDO apt-get install -y "$(readlink -f "$pkg")" || fail "apt-get could not install $pkg"
elif command -v dnf >/dev/null 2>&1; then
  pkg="$(find "$dir" -maxdepth 1 -name '*.rpm' | head -n 1)"
  [ -n "$pkg" ] || fail "no .rpm in $dir"
  $SUDO dnf install -y "$(readlink -f "$pkg")" || fail "dnf could not install $pkg"
else
  fail "neither apt-get nor dnf is available"
fi

# 2. Every binary of the bundle must be installed (on PATH, or under the package's files).
pkg_name="$( (dpkg-deb -f "$pkg" Package 2>/dev/null || rpm -qp --qf '%{NAME}' "$pkg") )"
listing="$( (dpkg -L "$pkg_name" 2>/dev/null || rpm -ql "$pkg_name") )"
find_bin() {
  local found
  found="$(command -v "$1" 2>/dev/null || true)"
  [ -n "$found" ] || found="$(printf '%s\n' "$listing" | grep -E "/$1(-[A-Za-z0-9_]+(-[A-Za-z0-9_]+)*)?$" | head -n 1 || true)"
  [ -n "$found" ] || return 1
  echo "$found"
}
for b in nucleos-core browser-sidecar echo-sidecar email-sidecar quota-sidecar remote-sidecar telegram-sidecar web-sidecar; do
  path="$(find_bin "$b")" || fail "$b is not installed by $pkg_name"
  [ -x "$path" ] || fail "$b is installed but not executable ($path)"
  echo "ok: $b -> $path"
done
core="$(find_bin nucleos-core)"

# 3. Start the daemon on 8791 and wait for it to answer (any HTTP status counts as an answer).
start_daemon() {
  NUCLEOS_PORT="$port" HOME="$home_dir" "$core" >>"$log" 2>&1 &
  pid=$!
  for _ in $(seq 1 60); do
    kill -0 "$pid" 2>/dev/null || fail "the daemon exited before answering"
    if curl -s -o /dev/null "http://127.0.0.1:$port/status"; then return 0; fi
    sleep 1
  done
  fail "the daemon did not answer on port $port within 60s"
}

start_daemon
echo "ok: daemon answers on $port"

# 4. Crash it, then prove stale state (lock, pid, socket) does not block a restart.
kill -9 "$pid"
wait "$pid" 2>/dev/null || true
pid=""
start_daemon
echo "ok: daemon restarted after kill -9"
echo "SMOKE PASS"
