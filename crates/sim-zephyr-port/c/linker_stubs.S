# linker_stubs.S — Linker section markers for Zephyr cc crate build.
#
# Provides all the linker-defined symbols that would normally come
# from Zephyr's custom linker script.  All symbols alias a single
# zero byte so that init-section iteration loops execute zero times.

.section .zephyr_markers,"a",@progbits
.align 8
zephyr_init_zero:
    .zero 8

# All init section start/end markers at the same address.
.global __init_EARLY_start
.set __init_EARLY_start, zephyr_init_zero

.global __init_PRE_KERNEL_1_start
.set __init_PRE_KERNEL_1_start, zephyr_init_zero

.global __init_PRE_KERNEL_2_start
.set __init_PRE_KERNEL_2_start, zephyr_init_zero

.global __init_POST_KERNEL_start
.set __init_POST_KERNEL_start, zephyr_init_zero

.global __init_APPLICATION_start
.set __init_APPLICATION_start, zephyr_init_zero

.global __init_end
.set __init_end, zephyr_init_zero

# Device list markers
.global _device_list_start
.set _device_list_start, zephyr_init_zero

.global _device_list_end
.set _device_list_end, zephyr_init_zero

# Static thread data markers
.global __static_thread_data_list_start
.set __static_thread_data_list_start, zephyr_init_zero

.global __static_thread_data_list_end
.set __static_thread_data_list_end, zephyr_init_zero

# C++ / init array markers
.global __ZEPHYR_CTOR_LIST__
.set __ZEPHYR_CTOR_LIST__, zephyr_init_zero

.global __zephyr_init_array_start
.set __zephyr_init_array_start, zephyr_init_zero

.global __zephyr_init_array_end
.set __zephyr_init_array_end, zephyr_init_zero
