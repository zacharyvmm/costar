/* configs.c — Portable config check stubs for cc crate Zephyr build.
 *
 * The original configs.c (from west build) uses GEN_ABSOLUTE_SYM_KCONFIG
 * macros that emit ELF-specific .type directives incompatible with Mach-O.
 * These symbols are only used as build-time integrity checks to detect
 * stale autoconf.h — they're not referenced by any kernel code at runtime.
 *
 * Replaced with empty file for cross-platform compatibility.
 */
