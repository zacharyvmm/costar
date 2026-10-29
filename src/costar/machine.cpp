#include "costar/machine.hpp"

#include "costar/abi.hpp"

namespace costar {

namespace {
void run_reset_vector(void* reset) { reinterpret_cast<costar_image_reset_fn>(reset)(); }
}  // namespace

Machine::Machine(uint32_t id, const std::filesystem::path& image, const Time& now, Trace& trace)
    : id_(id), now_(now), trace_(trace), image_(image), cpu_(now) {
  image_.attach(host_table(), this);
  cpu_.boot(run_reset_vector, reinterpret_cast<void*>(image_.reset()));
}

void Machine::trace(const char* label, uint32_t value) { trace_.record({now_, id_, label, value}); }

void Machine::log(std::string_view text) {
  if (text.ends_with('\n')) text.remove_suffix(1);
  trace_.record({now_, id_, "log", 0, std::string(text)});
}

void Machine::stop(int status) {
  trace_.record({now_, id_, "stop", static_cast<uint32_t>(status)});
  cpu_.stop();
}

}  // namespace costar
