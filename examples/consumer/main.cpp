// This project's costar command: costar's, plus a plant of its own.
#include <cstdint>
#include <memory>
#include <string>

#include "costar/cli.hpp"
#include "costar/world.hpp"

namespace {

// Writes 1, 2, 3, ... to register 0 of the target device, one per step.
class Ramp : public costar::Plant {
 public:
  Ramp(uint32_t machine, std::string device) : machine_(machine), device_(std::move(device)) {}
  void step(costar::Time, costar::World& world) override {
    world.machine(machine_).set_register(device_, 0, ++value_);
  }

 private:
  uint32_t machine_;
  std::string device_;
  uint32_t value_ = 0;
};

// [[plant]] type = "ramp", target = "<machine>.<device>"
std::unique_ptr<costar::Plant> make_ramp(const costar::PlantParams& params, const costar::Scenario& scenario) {
  const std::string target = std::get<std::string>(params.at("target"));
  const size_t dot = target.find('.');
  return std::make_unique<Ramp>(scenario.machine_id(target.substr(0, dot)), target.substr(dot + 1));
}

}  // namespace

int main(int argc, char** argv) { return costar::cli::main(argc, argv, {{"ramp", make_ramp}}); }
