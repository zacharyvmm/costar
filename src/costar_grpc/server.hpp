#pragma once

#include <memory>
#include <string>

#include <grpcpp/grpcpp.h>

#include "costar/scenario.hpp"
#include "costar/v1/costar.grpc.pb.h"

namespace costar::grpc_server {

// The costar.v1.Costar service: a registry of sessions, and the translation
// between protobuf messages and Session calls.
std::unique_ptr<v1::Costar::Service> make_service(PlantFactories plants);

// Serve the API at `address` (for example "127.0.0.1:9321") until the process
// ends.  Returns non-zero if the server could not start.
int serve(const std::string& address, PlantFactories plants);

}  // namespace costar::grpc_server
