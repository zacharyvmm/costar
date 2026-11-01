#!/bin/sh
# Packages a costar build as a release tarball: `cmake --install` into
# costar-<version>-<os>-<arch>/, tarred and gzipped.  Prints the tarball's path.
#
#   tools/release/package.sh BUILD_DIR OUT_DIR [VERSION]
#
# <version> is the build's project(costar VERSION), or VERSION: that with a
# pre-release suffix (0.3.0-rc.1), as check-version.sh prints for a pre-release
# tag; <os> is this system's
# ID-VERSION_ID (ubuntu-24.04), since the binaries need its glibc and
# Boost.Context; <arch> is uname -m.  The release workflow and CI use it, and
# the GitHub Action (action.yml) installs what it makes.
set -eu

if [ $# -lt 2 ] || [ $# -gt 3 ]; then
  echo "usage: $0 BUILD_DIR OUT_DIR [VERSION]" >&2
  exit 2
fi
build=$1
out=$2

version=$(sed -n 's/^CMAKE_PROJECT_VERSION:STATIC=//p' "$build/CMakeCache.txt")
if [ -z "$version" ]; then
  echo "$0: no project version in $build/CMakeCache.txt" >&2
  exit 1
fi
if [ $# -eq 3 ]; then
  case $3 in
    "$version" | "$version"-*) version=$3 ;;
    *)
      echo "$0: version $3 isn't the build's $version (or it with a pre-release suffix)" >&2
      exit 1
      ;;
  esac
fi
# shellcheck source=/dev/null
. /etc/os-release
name=costar-$version-$ID-$VERSION_ID-$(uname -m)

mkdir -p "$out"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cmake --install "$build" --prefix "$stage/$name" >/dev/null
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"
