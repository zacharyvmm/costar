# Releasing costar

A release is a `v<version>` tag. Pushing it runs
[`.github/workflows/release.yml`](../.github/workflows/release.yml), which
publishes:
- the GitHub release `v<version>`, with:
  - `costar-<version>-ubuntu-24.04-x86_64.tar.gz`, the install tarball that
    the action ([`action.yml`](../action.yml)) installs;
  - the Python client's sdist and wheel;
- the image `ghcr.io/zacharyvmm/costar:<version>`, also tagged
  `<major>.<minor>` and `latest`.

The action needs no publishing of its own: `uses: zacharyvmm/costar@v<version>`
is that tag of this repository, and it installs that tag's tarball.

## Versions

costar is 0.x. A release with new features, or a change to the host ABI
(`include/costar_host.h`), the scenario format or the CMake functions, bumps
the minor version (0.2.0 → 0.3.0). One with only fixes bumps the patch
(0.2.0 → 0.2.1). The installed package is `SameMinorVersion`, so projects
asking for `find_package(costar 0.2)` accept 0.2.x but not 0.3.

## Steps

1. **Bump the version** on `main`, in one commit:
   - `project(costar VERSION ...)` in [`CMakeLists.txt`](../CMakeLists.txt);
   - `version` in [`python/pyproject.toml`](../python/pyproject.toml);
   - after a minor bump, `find_package(costar 0.N ...)` in
     [`examples/consumer`](../examples/consumer/CMakeLists.txt) (costar's
     tests fail until it matches), the README and
     [ARCHITECTURE.md](ARCHITECTURE.md);
   - the versions in the README's examples (`@v<version>`, the image tags,
     the tarball and wheel URLs).

   `git grep -n` for the old version finds them.
   [`tools/release/check-version.sh`](../tools/release/check-version.sh)
   checks that the first two agree, and that a tag matches them.
2. **Dry run.** In GitHub, Actions → Release → Run workflow, on `main`. It
   checks the versions, builds and tests in Release, packages the tarball
   and checks that `examples/consumer` builds against it, builds the
   Python client, and builds the image (running costar's tests in it). It
   publishes nothing; the tarball and the Python packages are the run's
   `release-assets` artifact.
3. **Tag and push:**
   ```bash
   git tag -a v0.2.0 -m "costar 0.2.0"
   git push origin v0.2.0
   ```
   If the tag doesn't match `project(costar VERSION)`, the first job fails
   and says so; nothing is published.
4. **Check the release:**
   - the release page has the tarball, the sdist and the wheel, and its
     generated notes. Edit the notes to say what changed for users (new
     scenario keys, CLI flags, ABI changes that need images rebuilt);
   - `docker pull ghcr.io/zacharyvmm/costar:0.2.0`;
   - a workflow with `uses: zacharyvmm/costar@v0.2.0` installs it.
5. **The first release only:** the image's package on GHCR is private at
   first. In its package settings, make it public, and connect it to this
   repository.

A tag with a pre-release suffix is a pre-release of the version in
`CMakeLists.txt` and `pyproject.toml`, which stay `0.3.0` (CMake versions
can't have a suffix). The suffix is `alpha`, `beta` or `rc`, a dot, and a
number without leading zeros (`v0.3.0-alpha.1`, `v0.3.0-beta.2`,
`v0.3.0-rc.10`). Semantic versioning and Python's versions (PEP 440) both
treat these as pre-releases, order them the same way (`rc.2` before
`rc.10`), and map them one to one. Other suffixes are refused.
- the GitHub release is `v0.3.0-rc.1`, marked as a pre-release;
- the tarball is `costar-0.3.0-rc.1-ubuntu-24.04-x86_64.tar.gz`, and the
  wheel's version is `0.3.0rc1` (pip's spelling: `-beta.2` is `b2`);
- the image gets only the `0.3.0-rc.1` tag, not `0.3` or `latest`;
- the action installs it when asked by name:
  `uses: zacharyvmm/costar@v0.3.0-rc.1` with `version: 0.3.0-rc.1`. (Without
  `version`, it looks for the release `v0.3.0`.)

The installed CMake package says `0.3.0`.

## When it fails

Nothing is published until the tarball, the Python packages and the image
have been built and tested; the image is pushed before the GitHub release is
made. To redo a release, fix `main`, then move the tag:

```bash
git push --delete origin v0.2.0 && git tag -d v0.2.0
git tag -a v0.2.0 -m "costar 0.2.0" && git push origin v0.2.0
```

Delete the GitHub release first if it was made. A rerun pushes the image's
tags again, over the old ones. Once a release has been used, don't move its
tag: make a patch release.

## Not published

- **PyPI.** The name `costar` there is another project's, so the client is
  installed from the release's wheel, or from `python/`.
- **Other platforms.** The tarball is for Ubuntu 24.04 on x86_64 (its
  glibc, Boost.Context 1.83). Elsewhere, use the image or build from source.
