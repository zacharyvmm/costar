#pragma once

#include <cstdint>

#include "costar.h"

namespace costar {

// Virtual time in nanoseconds.
using Time = costar_time;

inline constexpr Time kNever = COSTAR_NEVER;

constexpr Time milliseconds(uint64_t ms) { return ms * 1'000'000; }

}  // namespace costar
