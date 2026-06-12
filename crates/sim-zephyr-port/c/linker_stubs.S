/*
 * linker_stubs.S - Linker section markers for the Zephyr cc crate build.
 *
 * Provides the linker-defined symbols that would normally come from Zephyr's
 * custom linker script. All symbols alias a single zero byte so section
 * iteration loops execute zero times.
 *
 * Portable across ELF (Linux) and Mach-O (macOS). Mach-O prefixes C symbols
 * with an extra underscore, so a C symbol named __init_start must be emitted
 * as ___init_start in this assembly file.
 */

#ifdef __APPLE__
#define ZEPHYR_ASM_SYM(symbol) _##symbol
#else
#define ZEPHYR_ASM_SYM(symbol) symbol
#endif

.data
.align 8

/* Zephyr init section markers. */
.global ZEPHYR_ASM_SYM(__init_start)
ZEPHYR_ASM_SYM(__init_start):
.global ZEPHYR_ASM_SYM(__init_EARLY_start)
ZEPHYR_ASM_SYM(__init_EARLY_start):
.global ZEPHYR_ASM_SYM(__init_PRE_KERNEL_1_start)
ZEPHYR_ASM_SYM(__init_PRE_KERNEL_1_start):
.global ZEPHYR_ASM_SYM(__init_PRE_KERNEL_2_start)
ZEPHYR_ASM_SYM(__init_PRE_KERNEL_2_start):
.global ZEPHYR_ASM_SYM(__init_POST_KERNEL_start)
ZEPHYR_ASM_SYM(__init_POST_KERNEL_start):
.global ZEPHYR_ASM_SYM(__init_APPLICATION_start)
ZEPHYR_ASM_SYM(__init_APPLICATION_start):
.global ZEPHYR_ASM_SYM(__init_SMP_start)
ZEPHYR_ASM_SYM(__init_SMP_start):
.global ZEPHYR_ASM_SYM(__init_end)
ZEPHYR_ASM_SYM(__init_end):

/* Device list markers. */
.global ZEPHYR_ASM_SYM(_device_list_start)
ZEPHYR_ASM_SYM(_device_list_start):
.global ZEPHYR_ASM_SYM(_device_list_end)
ZEPHYR_ASM_SYM(_device_list_end):

/* Static thread data markers. */
.global ZEPHYR_ASM_SYM(__static_thread_data_list_start)
ZEPHYR_ASM_SYM(__static_thread_data_list_start):
.global ZEPHYR_ASM_SYM(__static_thread_data_list_end)
ZEPHYR_ASM_SYM(__static_thread_data_list_end):

/* C++ / init array markers. */
.global ZEPHYR_ASM_SYM(__ZEPHYR_CTOR_LIST__)
ZEPHYR_ASM_SYM(__ZEPHYR_CTOR_LIST__):
.global ZEPHYR_ASM_SYM(__ZEPHYR_CTOR_END__)
ZEPHYR_ASM_SYM(__ZEPHYR_CTOR_END__):
.global ZEPHYR_ASM_SYM(__zephyr_init_array_start)
ZEPHYR_ASM_SYM(__zephyr_init_array_start):
.global ZEPHYR_ASM_SYM(__zephyr_init_array_end)
ZEPHYR_ASM_SYM(__zephyr_init_array_end):

/* Native simulator task section markers. */
.global ZEPHYR_ASM_SYM(__native_tasks_start)
ZEPHYR_ASM_SYM(__native_tasks_start):
.global ZEPHYR_ASM_SYM(__native_PRE_BOOT_1_tasks_start)
ZEPHYR_ASM_SYM(__native_PRE_BOOT_1_tasks_start):
.global ZEPHYR_ASM_SYM(__native_PRE_BOOT_2_tasks_start)
ZEPHYR_ASM_SYM(__native_PRE_BOOT_2_tasks_start):
.global ZEPHYR_ASM_SYM(__native_PRE_BOOT_3_tasks_start)
ZEPHYR_ASM_SYM(__native_PRE_BOOT_3_tasks_start):
.global ZEPHYR_ASM_SYM(__native_FIRST_SLEEP_tasks_start)
ZEPHYR_ASM_SYM(__native_FIRST_SLEEP_tasks_start):
.global ZEPHYR_ASM_SYM(__native_ON_EXIT_tasks_start)
ZEPHYR_ASM_SYM(__native_ON_EXIT_tasks_start):
.global ZEPHYR_ASM_SYM(__native_tasks_end)
ZEPHYR_ASM_SYM(__native_tasks_end):

zephyr_init_zero:
    .zero 8
