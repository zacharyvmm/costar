// The costar-grpc server, with the example plants built in.
//   costar-grpc [--listen ADDRESS]      (default 127.0.0.1:9321)
#include <iostream>
#include <string>

#include "../costar/thermal_plant.hpp"
#include "costar_grpc/server.hpp"

int main(int argc, char** argv) {
  std::string address = "127.0.0.1:9321";
  for (int i = 1; i < argc; ++i) {
    const std::string arg = argv[i];
    if (arg == "--listen" && i + 1 < argc) {
      address = argv[++i];
    } else {
      std::cerr << "usage: costar-grpc [--listen ADDRESS]\n";
      return 2;
    }
  }
  return costar::grpc_server::serve(address, {{"thermal", costar::make_thermal_plant}});
}
