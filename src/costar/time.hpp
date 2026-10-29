#pragma once

#include <cstdint>

#include "costar.h"

namespace costar {

// Virtual time in nanoseconds.
using Time = costar_time;

inline constexpr Time kNever = COSTAR_NEVER;

constexpr Time microseconds(uint64_t us) { return us * 1'000; }
constexpr Time milliseconds(uint64_t ms) { return ms * 1'000'000; }

// a + b, or kNever if that is not representable.
constexpr Time time_add(Time a, Time b) { return b >= kNever - a ? kNever : a + b; }

}  // namespace costar
