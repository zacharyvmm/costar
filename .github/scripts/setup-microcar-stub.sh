#!/usr/bin/env bash
#
# Provide the microcar-plant path-dependency sibling for CI.
#
# costar/crates/sim-runner declares an OPTIONAL path dependency:
#
#     microcar-plant = { path = "../../../microcar/plant", optional = true }
#
# behind the (default-off) `microcar` feature. Cargo requires that manifest to
# exist to resolve the workspace at all, even though CI never builds
# `--features microcar` (the plant code is fully `#[cfg(feature = "microcar")]`
# gated with graceful fallbacks). The real crate lives in the *private* repo
# zacharyvmm/microcar, which the costar-scoped GITHUB_TOKEN cannot check out
# (404). Instead of a cross-repo private checkout we deterministically create a
# minimal stub crate that satisfies path-dep resolution. It is never compiled
# because no CI job enables the `microcar` feature.
#
# Run from the costar checkout root ($GITHUB_WORKSPACE). Creates the sibling at
# ../microcar/plant so ../../../microcar/plant (from crates/sim-runner) resolves.
set -euo pipefail

plant_dir="../microcar/plant"
mkdir -p "${plant_dir}/src"

# Mirror the real crate's identity + its single real dependency (sim-world,
# which points back into this costar checkout) so Cargo.lock stays consistent.
{
  echo '[package]'
  echo 'name = "microcar-plant"'
  echo 'version = "0.1.0"'
  echo 'edition = "2021"'
  echo 'description = "CI stub for microcar-plant path-dep resolution (real crate: zacharyvmm/microcar)"'
  echo ''
  echo '[dependencies]'
  echo 'sim-world = { path = "../../costar/crates/sim-world" }'
} > "${plant_dir}/Cargo.toml"

echo '//! CI stub for microcar-plant. Never compiled (the `microcar` feature is' > "${plant_dir}/src/lib.rs"
echo '//! never enabled in CI). The real crate lives in zacharyvmm/microcar.' >> "${plant_dir}/src/lib.rs"

echo "Created microcar-plant stub at $(cd "${plant_dir}" && pwd)"
