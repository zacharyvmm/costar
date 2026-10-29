#pragma once

#include "costar_host.h"

namespace costar {

// The function table every firmware image calls through (see costar_host.h).
const costar_host& host_table();

}  // namespace costar
