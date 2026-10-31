#pragma once

#include "costar/scenario.hpp"

namespace costar::cli {

// The `costar` command line:
//   costar run  <scenario.toml> [--images DIR]... [--until-ms N] [--trace FILE] [--realtime] [--timeout-s N]
//   costar test <scenario.toml | DIR>... [--images DIR]... [--update] [--timeout-s N]
// Programs with their own plants call this with those plants registered.
int main(int argc, char** argv, const PlantFactories& plants);

}  // namespace costar::cli
