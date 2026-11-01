#!/bin/bash
# Installs a costar release tarball: the GitHub Action's (action.yml) install
# step.  Settings come from the environment:
#
#   COSTAR_VERSION     the release to download, like 0.2.0 or 0.3.0-rc.1
#                      (default: this checkout's project(costar VERSION), so
#                      the action at v0.2.0 installs costar 0.2.0; a
#                      pre-release must be named)
#   COSTAR_TARBALL     a tarball to install instead (tools/release/package.sh's)
#   COSTAR_REPOSITORY  where releases are (default zacharyvmm/costar)
#   COSTAR_GRPC        true: also install gRPC, for costar-grpc and
#                      find_package(costar COMPONENTS grpc)
#   COSTAR_PREFIX      where to install (default $RUNNER_TEMP/costar)
#
# It installs Boost.Context (and gRPC) with apt, unless already there, and
# extracts the tarball into the prefix.  Under GitHub Actions it puts the
# prefix's bin/ on the PATH, prepends the prefix to CMAKE_PREFIX_PATH, and
# sets the step outputs `prefix` and `version`; elsewhere it prints what to set.
set -euo pipefail

fail() {
  echo "::error::costar: $1" >&2
  exit 1
}

root=$(cd "$(dirname "$0")/../.." && pwd)
repository=${COSTAR_REPOSITORY:-zacharyvmm/costar}
prefix=${COSTAR_PREFIX:-${RUNNER_TEMP:-/tmp}/costar}

[ "$(uname -s)" = Linux ] || fail "costar runs on Linux; on $(uname -s), use its container image (see README.md)"

# The binaries are built on Ubuntu 24.04 (glibc 2.39, Boost 1.83).
os=unknown
if [ -r /etc/os-release ]; then
  # shellcheck source=/dev/null
  os=$(. /etc/os-release && echo "$ID-$VERSION_ID")
fi
if [ "$os" != ubuntu-24.04 ]; then
  echo "::warning::costar: releases are built on Ubuntu 24.04, and this is $os; the binaries may not run here" >&2
fi

# Packages: Boost.Context, which the costar program and library link to,
# and optionally gRPC.
packages=(libboost-context-dev)
if [ "${COSTAR_GRPC:-false}" = true ]; then
  packages+=(libgrpc++-dev libprotobuf-dev protobuf-compiler protobuf-compiler-grpc)
fi
missing=()
for package in "${packages[@]}"; do
  dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q "install ok installed" || missing+=("$package")
done
if [ ${#missing[@]} -gt 0 ]; then
  command -v apt-get >/dev/null || fail "install ${missing[*]} (or their equivalents): there is no apt-get"
  sudo=()
  [ "$(id -u)" = 0 ] || sudo=(sudo)
  "${sudo[@]}" apt-get update -q
  "${sudo[@]}" env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends "${missing[@]}"
fi

# The tarball: given, or this version's release.
download=
if [ -n "${COSTAR_TARBALL:-}" ]; then
  tarball=$COSTAR_TARBALL
  [ -f "$tarball" ] || fail "no tarball $tarball"
  # costar-<version>-<os ID>-<os VERSION_ID>-<arch>.tar.gz; <version> may be 0.3.0-rc.1.
  version=$(basename "$tarball" | sed -n 's/^costar-\(.*\)-[^-]*-[^-]*-[^-]*\.tar\.gz$/\1/p')
else
  version=${COSTAR_VERSION:-}
  if [ -z "$version" ]; then
    version=$(sed -n 's/^project(costar VERSION \([^ )]*\).*/\1/p' "$root/CMakeLists.txt")
  fi
  version=${version#v}
  arch=$(uname -m)
  [ "$arch" = x86_64 ] || fail "releases are built for x86_64 only, not $arch"
  name=costar-$version-ubuntu-24.04-$arch.tar.gz
  download=$(mktemp -d)
  tarball=$download/$name
  url=https://github.com/$repository/releases/download/v$version/$name
  echo "Downloading $url"
  curl -fsSL --retry 3 -o "$tarball" "$url" ||
    fail "can't download $url: is there a release v$version with that file? (set the action's version input)"
fi

rm -rf "$prefix"
mkdir -p "$prefix"
tar -xzf "$tarball" -C "$prefix" --strip-components=1
[ -z "$download" ] || rm -rf "$download"
[ -x "$prefix/bin/costar" ] || fail "$tarball has no bin/costar"
echo "Installed costar $version in $prefix"

if [ -n "${GITHUB_ENV:-}" ]; then
  echo "$prefix/bin" >>"$GITHUB_PATH"
  echo "CMAKE_PREFIX_PATH=$prefix${CMAKE_PREFIX_PATH:+:$CMAKE_PREFIX_PATH}" >>"$GITHUB_ENV"
  {
    echo "prefix=$prefix"
    echo "version=$version"
  } >>"$GITHUB_OUTPUT"
else
  echo "export PATH=$prefix/bin:\$PATH CMAKE_PREFIX_PATH=$prefix\${CMAKE_PREFIX_PATH:+:\$CMAKE_PREFIX_PATH}"
fi
