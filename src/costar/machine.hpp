#pragma once

#include <cstdint>
#include <filesystem>
#include <string_view>

#include "costar/cpu.hpp"
#include "costar/image.hpp"
#include "costar/time.hpp"
#include "costar/trace.hpp"

namespace costar {

// One simulated board: a CPU running its own copy of a firmware image.
class Machine {
 public:
  Machine(uint32_t id, const std::filesystem::path& image, const Time& now, Trace& trace);
  Machine(const Machine&) = delete;
  Machine& operator=(const Machine&) = delete;

  Cpu& cpu() { return cpu_; }
  uint32_t id() const { return id_; }
  Time now() const { return now_; }
  void trace(const char* label, uint32_t value);
  void log(std::string_view text);
  [[noreturn]] void stop(int status);

 private:
  uint32_t id_;
  const Time& now_;
  Trace& trace_;
  Image image_;  // Declared before cpu_ so the code outlives the threads running it.
  Cpu cpu_;
};

}  // namespace costar
