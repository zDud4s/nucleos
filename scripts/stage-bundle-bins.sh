#!/usr/bin/env bash
# Stage the núcleo and every Go sidecar where Tauri's `externalBin` expects them.
#
# Tauri resolves each `externalBin` entry `binaries/<name>` as
# `shell/src-tauri/binaries/<name>-<host triple>[.exe]`. The list of sidecars is derived from
# `sidecars/*/go.mod` (never hardcoded), so it cannot drift from shell/src-tauri/tauri.release.conf.json.
#
# Usage: scripts/stage-bundle-bins.sh [--dry-run]
#   --dry-run  print every "src -> dest" pair and build nothing.
set -euo pipefail

dry=0
case "${1:-}" in
  "") ;;
  --dry-run) dry=1 ;;
  *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

triple="$(rustc -vV | sed -n 's/^host: //p' | tr -d '\r')"
[ -n "$triple" ] || { echo "could not read the host triple from rustc -vV" >&2; exit 1; }
case "$triple" in
  *windows*) exe=".exe" ;;
  *) exe="" ;;
esac

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
  target_dir="$CARGO_TARGET_DIR"
else
  target_dir="$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | sed 's#\\#/#g')"
fi
[ -n "$target_dir" ] || { echo "could not resolve the cargo target directory" >&2; exit 1; }

dest_dir="$repo_root/shell/src-tauri/binaries"
sidecar_tmp="${TMPDIR:-/tmp}/nucleos-sidecars-$$"

names=()
for gomod in "$repo_root"/sidecars/*/go.mod; do
  [ -e "$gomod" ] || continue
  names+=("$(basename "$(dirname "$gomod")")")
done
[ "${#names[@]}" -gt 0 ] || { echo "no sidecars found under sidecars/*/" >&2; exit 1; }

if [ "$dry" -eq 1 ]; then
  echo "$target_dir/release/nucleos-core$exe -> $dest_dir/nucleos-core-$triple$exe"
  for n in "${names[@]}"; do
    echo "$sidecar_tmp/$n-sidecar$exe -> $dest_dir/$n-sidecar-$triple$exe"
  done
  exit 0
fi

cargo build --release -p nucleos-core
mkdir -p "$dest_dir" "$sidecar_tmp"
cp "$target_dir/release/nucleos-core$exe" "$dest_dir/nucleos-core-$triple$exe"

NUCLEOS_SIDECAR_DIR="$sidecar_tmp" bash "$repo_root/scripts/build-sidecars.sh" release
for n in "${names[@]}"; do
  cp "$sidecar_tmp/$n-sidecar$exe" "$dest_dir/$n-sidecar-$triple$exe"
done
echo "staged $((${#names[@]} + 1)) binaries into $dest_dir"
