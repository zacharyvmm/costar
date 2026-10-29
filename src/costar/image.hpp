#pragma once

#include <filesystem>

#include "costar_host.h"

namespace costar {

// One loaded copy of a firmware image (a shared object).
//
// Every Image loads a private copy of the file, so two machines running the
// same firmware never share globals: each gets its own RTOS kernel.
class Image {
 public:
  explicit Image(const std::filesystem::path& file);
  ~Image();
  Image(const Image&) = delete;
  Image& operator=(const Image&) = delete;

  // Point this copy's costar.h calls at `machine`.
  void attach(const costar_host& host, void* machine);
  // The image's reset vector (its main()).
  int (*entry() const)();

 private:
  void* symbol(const char* name) const;

  std::filesystem::path file_;
  void* handle_ = nullptr;
};

}  // namespace costar
