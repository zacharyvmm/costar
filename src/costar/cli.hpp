#pragma once

#include "costar/scenario.hpp"

namespace costar::cli {

// The `costar` command line:
//   costar run  <scenario.toml> [--images DIR]... [--set KEY=VALUE]... [--until-ms N] [--trace FILE]
//               [--realtime] [--timeout-s N]
//               [--console MACHINE.DEVICE] [--format text|jsonl] [--perfetto FILE]
//               [--machine NAME]... [--label LABEL]... [--exclude-label LABEL]...
//   costar test <scenario.toml | DIR>... [--images DIR]... [--set KEY=VALUE]... [--update] [--timeout-s N]
//               [--artifacts DIR [--all-artifacts]] [--junit FILE]
// Programs with their own plants call this with those plants registered.
int main(int argc, char** argv, const PlantFactories& plants);

}  // namespace costar::cli
