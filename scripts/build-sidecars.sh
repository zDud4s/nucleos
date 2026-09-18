#!/usr/bin/env bash
# Build every Go sidecar next to the daemon exe so the núcleo's supervisor finds it.
#
# core/src/sidecar.rs resolves each sidecar as `<dir-of-nucleos-core>/<name>-sidecar<GOEXE>`
# (i.e. target/<profile>/<name>-sidecar<GOEXE>), or as `$NUCLEOS_SIDECAR_DIR/<name>-sidecar<GOEXE>`
# when that variable is set. `<GOEXE>` is `go env GOEXE`: `.exe` on Windows and empty elsewhere,
# the same suffix `sidecar::binary_file_name` takes from Rust's `std::env::consts::EXE_SUFFIX`.
# This script compiles each `sidecars/*/` Go module
# (auto-discovered by its go.mod) into the same directory, named after the module dir, so the
# build and the daemon agree on where they are.
#
# Usage: scripts/build-sidecars.sh [debug|release]   (default: debug)
set -euo pipefail

profile="${1:-debug}"
case "$profile" in
  debug|release) ;;
  *) echo "usage: $0 [debug|release]" >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out_dir="${NUCLEOS_SIDECAR_DIR:-$repo_root/target/$profile}"
mkdir -p "$out_dir"

built=0
for gomod in "$repo_root"/sidecars/*/go.mod; do
  [ -e "$gomod" ] || continue   # no matches -> the glob stays literal; skip it
  dir="$(dirname "$gomod")"
  name="$(basename "$dir")"
  exe="$(cd "$dir" && go env GOEXE)"
  out="$out_dir/$name-sidecar$exe"
  printf 'building %-10s -> %s\n' "$name" "$out"
  ( cd "$dir" && go build -o "$out" . )
  built=$((built + 1))
done

if [ "$built" -eq 0 ]; then
  echo "no sidecars found under sidecars/*/ (nothing with a go.mod)" >&2
  exit 1
fi
echo "built $built sidecar(s) into $out_dir"
