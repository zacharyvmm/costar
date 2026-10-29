#pragma once

#include "costar/scenario.hpp"

namespace costar::cli {

// The `costar` command line:
//   costar run  <scenario.toml> [--images DIR]... [--until-ms N] [--trace FILE]
//   costar test <scenario.toml | DIR>... [--images DIR]... [--update]
// Programs with their own plants call this with those plants registered.
int main(int argc, char** argv, const PlantFactories& plants);

}  // namespace costar::cli
