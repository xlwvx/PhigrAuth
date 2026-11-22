#!/usr/bin/env bash
# Regenerate the source tarball, refresh PKGBUILD checksums and run makepkg.
#
#   ./package.sh                     build and install with pacman
#   PKG_NO_INSTALL=1 ./package.sh    only build the package
#   ./package.sh --nocheck           pass extra flags to makepkg
#
# A source tree without packaging artifacts is packed into
# phigrauth-$pkgver.tar.gz first.  Re-run this script after changing sources.
set -euo pipefail

PKG_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$PKG_DIR/.." && pwd)
cd "$PKG_DIR"

pkgver=$(sed -n 's/^pkgver=//p' PKGBUILD)
tarball="phigrauth-$pkgver.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/phigrauth-$pkgver"
cp -a \
  "$ROOT/Cargo.toml" \
  "$ROOT/Cargo.lock" \
  "$ROOT/LICENSE" \
  "$ROOT/README.md" \
  "$ROOT/config.example.toml" \
  "$ROOT/src" \
  "$ROOT/assets" \
  "$ROOT/docs" \
  "$ROOT/tools" \
  "$tmp/phigrauth-$pkgver/"

tar -C "$tmp" -czf "$tarball" "phigrauth-$pkgver"

if command -v updpkgsums >/dev/null 2>&1; then
  updpkgsums
else
  sum=$(sha256sum "$tarball" | cut -d' ' -f1)
  sed -i "s/^sha256sums=.*/sha256sums=('$sum')/" PKGBUILD
fi

makepkg_args=(-s -f -C)
if [[ ${PKG_NO_INSTALL:-0} != 1 ]]; then
  makepkg_args+=(-i)
fi
makepkg "${makepkg_args[@]}" "$@"
