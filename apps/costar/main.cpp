// The `costar` command, with the example plants built in.  Programs with their
// own plants write the same few lines with their factories added.
#include "costar/cli.hpp"
#include "thermal_plant.hpp"

int main(int argc, char** argv) {
  return costar::cli::main(argc, argv, {{"thermal", costar::make_thermal_plant}});
}
