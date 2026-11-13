#!/usr/bin/env bash
# Build every Go sidecar next to the daemon exe so the núcleo's supervisor finds it.
#
# core/src/sidecar.rs resolves each sidecar as `<dir-of-nucleos-core>/nucleos-<name><GOEXE>`
# (i.e. target/<profile>/nucleos-<name><GOEXE>), or as `$NUCLEOS_SIDECAR_DIR/nucleos-<name><GOEXE>`
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

# Version for the Windows version resource: core/Cargo.toml's package version.
version="$(sed -n 's/^version = "\([0-9][^"]*\)".*/\1/p' "$repo_root/core/Cargo.toml" | head -n1)"
version="${version:-0.0.0}"

built=0
for gomod in "$repo_root"/sidecars/*/go.mod; do
  [ -e "$gomod" ] || continue   # no matches -> the glob stays literal; skip it
  dir="$(dirname "$gomod")"
  name="$(basename "$dir")"
  exe="$(cd "$dir" && go env GOEXE)"
  out="$out_dir/nucleos-$name$exe"
  printf 'building %-10s -> %s\n' "$name" "$out"
  syso=""
  if [ "$(cd "$dir" && go env GOOS)" = "windows" ]; then
    # Embed a VERSION resource (Task Manager name). Fail open: a missing generator or a
    # failure just builds the exe without one.
    arch="$(cd "$dir" && go env GOARCH)"
    syso="$dir/rsrc_windows_$arch.syso"
    if ! ( cd "$repo_root/sidecars/tools/winres" && go run main.go -name "$name" \
        -version "$version" -arch "$arch" -out "$syso" ) 2>/dev/null; then
      echo "warning: version resource skipped for $name" >&2
      rm -f "$syso"; syso=""
    fi
  fi
  rc=0
  ( cd "$dir" && go build -o "$out" . ) || rc=$?
  [ -z "$syso" ] || rm -f "$syso"
  [ "$rc" -eq 0 ] || exit "$rc"
  built=$((built + 1))
done

if [ "$built" -eq 0 ]; then
  echo "no sidecars found under sidecars/*/ (nothing with a go.mod)" >&2
  exit 1
fi
echo "built $built sidecar(s) into $out_dir"
