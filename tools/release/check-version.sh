#!/bin/sh
# Checks that costar's versions agree, and prints the version.
#
#   tools/release/check-version.sh [--python] [TAG]
#
# project(costar VERSION) in CMakeLists.txt and the Python package's version
# in python/pyproject.toml must be the same.  With a TAG (a release's), it
# must be v<that version> (v0.2.0), or that and a pre-release suffix: alpha,
# beta or rc, a dot and a number without leading zeros (v0.2.0-rc.1,
# v0.2.0-beta.2).  The version printed is the tag's without the v; with
# --python, it's the Python package's spelling of it (PEP 440: 0.2.0rc1,
# 0.2.0b2).  The release workflow runs it first.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
cmake_version=$(sed -n 's/^project(costar VERSION \([^ )]*\).*/\1/p' "$root/CMakeLists.txt")
python_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/python/pyproject.toml")

fail() {
  echo "::error::$1" >&2  # an annotation in GitHub Actions, a message elsewhere
  exit 1
}
[ -n "$cmake_version" ] || fail "no project(costar VERSION ...) in CMakeLists.txt"
[ "$python_version" = "$cmake_version" ] ||
  fail "python/pyproject.toml has version $python_version, but CMakeLists.txt has $cmake_version"
python=false
if [ "${1:-}" = --python ]; then
  python=true
  shift
fi
if [ $# -eq 0 ]; then
  echo "$cmake_version"
  exit 0
fi
pre=""
case $1 in
  "v$cmake_version") ;;
  "v$cmake_version"-*)
    # Only pre-releases that semantic versioning and PEP 440 both order the
    # same way, and that map one to one: alpha < beta < rc, then by number.
    # The number is its own dot-separated identifier, so semantic versioning
    # compares it as a number (rc.2 < rc.10; "rc2" > "rc10" would compare as
    # text), and has no leading zeros (rc.01 would be rc1 to pip, like rc.1).
    pre=${1#"v$cmake_version"-}
    echo "$pre" | grep -Eqx '(alpha|beta|rc)\.(0|[1-9][0-9]*)' ||
      fail "tag $1 has a pre-release suffix other than alpha, beta or rc, a dot and a number: v$cmake_version-rc.1, say (docs/RELEASING.md)"
    ;;
  *)
    fail "tag $1 doesn't match project(costar VERSION $cmake_version) in CMakeLists.txt: tag v$cmake_version (or v$cmake_version-rc.1, say), or bump the version (docs/RELEASING.md)"
    ;;
esac
if $python && [ -n "$pre" ]; then
  # rc.1 -> rc1, beta.2 -> b2, alpha.3 -> a3.
  echo "$cmake_version$(echo "$pre" | sed -E 's/^alpha/a/; s/^beta/b/; s/\.//')"
else
  echo "${1#v}"
fi
