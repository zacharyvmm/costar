/*
 * Local native-simulator interface shim for the cc-based Zephyr build.
 *
 * Zephyr's upstream nsi_cpu_if.h places these symbols in ELF sections such as
 * ".native_sim_if". Darwin's Mach-O assembler rejects those section names, and
 * this crate links the embedded image directly instead of using the native
 * simulator runner's section discovery. Keep the public prototypes visible, but
 * do not force them into custom sections.
 */

#ifndef COSTAR_CONFIG_NSI_CPU_IF_H_
#define COSTAR_CONFIG_NSI_CPU_IF_H_

#ifdef __cplusplus
extern "C" {
#endif

#include "nsi_cpu_if_internal.h"

#define NATIVE_SIMULATOR_IF_SECT(sect) __attribute__((visibility("default")))
#define NATIVE_SIMULATOR_IF NATIVE_SIMULATOR_IF_SECT(".native_sim_if")
#define NATIVE_SIMULATOR_IF_DATA NATIVE_SIMULATOR_IF_SECT(".native_sim_if.data")
#define NATIVE_SIMULATOR_IF_TEXT NATIVE_SIMULATOR_IF_SECT(".native_sim_if.text")

NATIVE_SIMULATOR_IF void nsif_cpu0_pre_cmdline_hooks(void);
NATIVE_SIMULATOR_IF void nsif_cpu0_pre_hw_init_hooks(void);
NATIVE_SIMULATOR_IF void nsif_cpu0_boot(void);
NATIVE_SIMULATOR_IF int nsif_cpu0_cleanup(void);
NATIVE_SIMULATOR_IF void nsif_cpu0_irq_raised(void);
NATIVE_SIMULATOR_IF void nsif_cpu0_irq_raised_from_sw(void);
NATIVE_SIMULATOR_IF int nsif_cpu0_test_hook(void *p);

F_TRAMP_LIST(NATIVE_SIMULATOR_IF void nsif_cpu, _pre_cmdline_hooks(void))
F_TRAMP_LIST(NATIVE_SIMULATOR_IF void nsif_cpu, _pre_hw_init_hooks(void))
F_TRAMP_LIST(NATIVE_SIMULATOR_IF void nsif_cpu, _boot(void))
F_TRAMP_LIST(NATIVE_SIMULATOR_IF int nsif_cpu, _cleanup(void))
F_TRAMP_LIST(NATIVE_SIMULATOR_IF void nsif_cpu, _irq_raised(void))
F_TRAMP_LIST(NATIVE_SIMULATOR_IF void nsif_cpu, _irq_raised_from_sw(void))
F_TRAMP_LIST(NATIVE_SIMULATOR_IF int nsif_cpu, _test_hook(void *p))

#ifdef __cplusplus
}
#endif

#endif /* COSTAR_CONFIG_NSI_CPU_IF_H_ */
