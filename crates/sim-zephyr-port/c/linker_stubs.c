/*
 * linker_stubs.c — Zero-initialized linker section markers for Zephyr.
 *
 * All init-section start/end symbols are defined as weak aliases to
 * a single zero byte.  When the kernel iterates over init sections
 * (for (p = &__init_EARLY_start; p < &__init_end; p++)),
 * &start == &end, so the loop body never executes.
 */

#define ALIAS_SYMBOL(new_name, target) \
    __asm__(".global " #new_name "\n" #new_name " = " #target "\n")

/* A single byte of zero storage — all markers alias it. */
__attribute__((used, visibility("hidden")))
const char __zephyr_init_zero = 0;

/* Init level start/end markers. */
ALIAS_SYMBOL(__init_EARLY_start, __zephyr_init_zero);
ALIAS_SYMBOL(__init_PRE_KERNEL_1_start, __zephyr_init_zero);
ALIAS_SYMBOL(__init_PRE_KERNEL_2_start, __zephyr_init_zero);
ALIAS_SYMBOL(__init_POST_KERNEL_start, __zephyr_init_zero);
ALIAS_SYMBOL(__init_APPLICATION_start, __zephyr_init_zero);
ALIAS_SYMBOL(__init_end, __zephyr_init_zero);

/* Device list markers. */
ALIAS_SYMBOL(_device_list_start, __zephyr_init_zero);
ALIAS_SYMBOL(_device_list_end, __zephyr_init_zero);

/* Static thread data markers. */
ALIAS_SYMBOL(__static_thread_data_list_start, __zephyr_init_zero);
ALIAS_SYMBOL(__static_thread_data_list_end, __zephyr_init_zero);

/* C++ / init array markers. */
ALIAS_SYMBOL(__ZEPHYR_CTOR_LIST__, __zephyr_init_zero);
ALIAS_SYMBOL(__zephyr_init_array_start, __zephyr_init_zero);
ALIAS_SYMBOL(__zephyr_init_array_end, __zephyr_init_zero);
