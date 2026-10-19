#!/usr/bin/env bash
# Install NucleOS on macOS (Apple silicon) or Linux (x86_64) from a GitHub release.
#
#   curl -fsSL https://github.com/zDud4s/nucleos/releases/latest/download/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- 1.2.3        # a specific version
#
# The assets are fetched with curl, which sets no quarantine attribute, so Gatekeeper does not
# block the app. Every download is checked against the release's SHA256SUMS before anything is
# installed; a mismatch deletes it and exits non-zero.
#
# macOS: extracts the .app.tar.gz into /Applications, or ~/Applications without permission.
# Linux: puts the AppImage in ~/.local/bin and writes a menu entry (needs FUSE, libfuse2, to run).
# Windows has its own script, install.ps1.
#
# Environment (all optional):
#   NUCLEOS_VERSION           same as the argument
#   NUCLEOS_RELEASE_BASE      directory URL holding SHA256SUMS and the assets (tests, mirrors)
#   NUCLEOS_INSTALL_DRY_RUN=1 download and verify, install nothing
#   NUCLEOS_APPLICATIONS_DIR  macOS install directory
#   NUCLEOS_BIN_DIR           Linux directory for the AppImage
#   NUCLEOS_UNAME_S / _M      pretend to be another platform (tests)
#
# Exit codes: 0 done; 1 download, checksum or install failure; 2 unsupported platform.
set -euo pipefail

REPO="zDud4s/nucleos"
# Keep these suffixes in step with ASSETS in scripts/release-manifest.py (a test compares them).
LINUX_SUFFIX=".AppImage"
MACOS_SUFFIX=".app.tar.gz"

die() { echo "install.sh: $*" >&2; exit "${2:-1}"; }

version="${1:-${NUCLEOS_VERSION:-}}"
version="${version#v}"

os="${NUCLEOS_UNAME_S:-$(uname -s)}"
arch="${NUCLEOS_UNAME_M:-$(uname -m)}"
case "$os/$arch" in
  Linux/x86_64|Linux/amd64) kind=linux; suffix="$LINUX_SUFFIX" ;;
  Darwin/arm64|Darwin/aarch64) kind=macos; suffix="$MACOS_SUFFIX" ;;
  *) die "unsupported platform $os/$arch (NucleOS has installers for Linux x86_64, macOS on Apple silicon and Windows x64)" 2 ;;
esac

if [ -n "${NUCLEOS_RELEASE_BASE:-}" ]; then
  base="${NUCLEOS_RELEASE_BASE%/}"
elif [ -n "$version" ]; then
  base="https://github.com/$REPO/releases/download/v$version"
else
  base="https://github.com/$REPO/releases/latest/download"
fi
if [ "${NUCLEOS_PRINT_BASE:-}" = 1 ]; then echo "release: $base"; exit 0; fi

command -v curl >/dev/null 2>&1 || die "curl is required"
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "sha256sum or shasum is required to verify the download"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/nucleos-install.XXXXXX")"
trap 'rm -rf "$work"' EXIT

fetch() { curl -fsSL --retry 2 -o "$2" "$1" || die "could not download $1"; }

echo "release: $base"
fetch "$base/SHA256SUMS" "$work/SHA256SUMS"

# One line `<sha256>  <name>` whose name ends with the platform suffix. A `.sig` never matches.
matches="$(tr -d '\r' < "$work/SHA256SUMS" | awk -v s="$suffix" '
  { n = $2; if (length(n) >= length(s) && substr(n, length(n) - length(s) + 1) == s) print }')"
[ -n "$matches" ] || die "SHA256SUMS has no entry ending in $suffix"
[ "$(printf '%s\n' "$matches" | wc -l | tr -d ' ')" = 1 ] || die "SHA256SUMS has more than one entry ending in $suffix"
want="$(printf '%s' "$matches" | awk '{ print tolower($1) }')"
name="$(printf '%s' "$matches" | awk '{ print $2 }')"

echo "downloading $name"
fetch "$base/$name" "$work/$name"
got="$(sha256 "$work/$name")"
if [ "$got" != "$want" ]; then
  rm -f "$work/$name"
  die "checksum mismatch for $name (expected $want, got $got); nothing was installed"
fi
echo "verified $name (sha256 $got)"

if [ "${NUCLEOS_INSTALL_DRY_RUN:-}" = 1 ]; then
  echo "dry run: nothing installed"
  exit 0
fi

if [ "$kind" = linux ]; then
  bin_dir="${NUCLEOS_BIN_DIR:-$HOME/.local/bin}"
  apps_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
  mkdir -p "$bin_dir" "$apps_dir"
  dest="$bin_dir/NucleOS.AppImage"
  # Stage beside the target, then rename: a running copy keeps its inode and the swap is atomic.
  cp "$work/$name" "$dest.new" && chmod +x "$dest.new" && mv -f "$dest.new" "$dest"
  cat > "$apps_dir/nucleos.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=NucleOS
Exec="$dest"
Terminal=false
Categories=Utility;
EOF
  echo "installed $dest"
  echo "AppImages need FUSE: on Ubuntu 22.04 run  sudo apt install libfuse2"
else
  if [ -n "${NUCLEOS_APPLICATIONS_DIR:-}" ]; then
    apps="$NUCLEOS_APPLICATIONS_DIR"
  elif [ -w /Applications ]; then
    apps=/Applications
  else
    apps="$HOME/Applications"
  fi
  mkdir -p "$apps" "$work/extract"
  tar -xzf "$work/$name" -C "$work/extract"
  [ -d "$work/extract/NucleOS.app" ] || die "the archive does not contain NucleOS.app"
  rm -rf "$apps/NucleOS.app"
  mv "$work/extract/NucleOS.app" "$apps/NucleOS.app"
  echo "installed $apps/NucleOS.app"
fi
