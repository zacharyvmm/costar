#ifndef COSTAR_ZEPHYR_PORT_ALLOCA_H
#define COSTAR_ZEPHYR_PORT_ALLOCA_H

#if defined(_MSC_VER)
#include <malloc.h>
#ifndef alloca
#define alloca _alloca
#endif
#elif defined(__has_builtin)
#if __has_builtin(__builtin_alloca)
#ifndef alloca
#define alloca __builtin_alloca
#endif
#else
#include_next <alloca.h>
#endif
#else
#include_next <alloca.h>
#endif

#endif /* COSTAR_ZEPHYR_PORT_ALLOCA_H */
