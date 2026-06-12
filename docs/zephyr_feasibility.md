# Zephyr Feasibility Study

Status: **Incomplete** (Phase 12, per HANDOFF.md §16)
Target: Assess feasibility of adding a Zephyr adapter to the Universal RTOS Native Simulator.

## 1. Can We Support Zephyr by Adding a Custom Architecture/Board?

**Yes, with significant effort.**

Zephyr has a well-defined architecture-port interface (`arch/`) and board
layer (`boards/`).  Each architecture provides:

| Interface | Purpose |
|-----------|---------|
| `arch_switch()` | Thread context switch |
| `arch_irq_lock()` / `arch_irq_unlock()` | Interrupt masking |
| `arch_k_cycle_get_32()` | Cycle counter |
| `arch_system_halt()` | System shutdown |
| `z_arm_*` / `z_riscv_*` / etc. | Arch-specific interrupt handling |
| `__stack` / `__esf` | Stack frame / exception stack frame structs |

A custom `arch/sim/` could be added that:

- Implements `arch_switch()` via `corosensei` fiber yield/resume (like our FreeRTOS port does via `vPortYield`)
- Implements IRQ lock/unlock via our `sim_enter_critical()` / `sim_exit_critical()`
- Stubs out cycle counter with virtual time
- Stubs out exception handling (no real CPU exceptions in simulation)

The board layer (`boards/sim/`) would define:

- A minimal devicetree (no real hardware peripherals)
- Memory layout (flat, host-sized)
- Console via our virtual UART
- Timer via our virtual timer

**Challenge**: Zephyr's build system (CMake + Kconfig + devicetree) is far more
complex than FreeRTOS's 5-10 C files compiled via `cc`.  A custom board requires
Kconfig fragments, DTS files, and CMake integration.

**Verdict**: Feasible.  The arch/board interfaces are well-documented and have
precedent (`native_sim`, `native_posix`, `qemu_cortex_m3`, etc.).

---

## 2. Which Zephyr Build Outputs Must Be Consumed?

Zephyr's build produces many artifacts.  The ones relevant to our simulator:

| Artifact | Content | Required? |
|----------|---------|-----------|
| `zephyr.elf` / `libzephyr.a` | Compiled kernel + app object code | **Yes** — the guest payload |
| `autoconf.h` | `#define` from Kconfig (`CONFIG_*`) | **Yes** — kernel config |
| `devicetree_generated.h` | DTS → C macros (`DT_N_S_*`) | **Yes** — device model |
| `driver_validation_h_target` | Driver init lists (`SYS_INIT`) | **Yes** — driver ordering |
| `syscall_dispatch.c` | Userspace syscall table | If `CONFIG_USERSPACE=y` |
| `kobject_hash.gperf` | Kernel object permissions | If `CONFIG_USERSPACE=y` |
| `include/generated/` | Board-specific generated headers | **Yes** — pinmux, IRQ numbers |

The critical observation: **Zephyr cannot be compiled as a simple `cc::Build`
invocation.**  It requires:

1. `west build` (or CMake directly) with Zephyr's CMake modules
2. Kconfig processing to produce `autoconf.h`
3. Devicetree processing to produce `devicetree_generated.h`
4. Python scripts (`kconfiglib`, `dtlib`, `edtlib`) — these are part of Zephyr's
   own build toolchain, NOT our simulator engine

The HANDOFF.md non-goal "Avoid Python code-generation hacks in the simulator
engine" applies here: the *simulator engine* must not depend on Python codegen,
but the *Zephyr build itself* unavoidably uses Python tooling.  The distinction
is important — we consume Zephyr's build outputs, we do not embed Zephyr's
build system into our engine.

**Recommended approach**: Treat Zephyr as an *external build dependency*.
The user builds their Zephyr application with `west build -b sim`, producing
a static library.  Our `build.rs` links against it, and our C ABI header
(`sim_abi.h`) provides the bridge functions Zephyr's arch port calls.

---

## 3. How Much of native_sim Can Be Reused Conceptually?

Zephyr's `native_sim` (and its predecessor `native_posix`) runs Zephyr as a
native POSIX process.  Key components:

| Component | native_sim approach | Our approach | Reusable? |
|-----------|--------------------|--------------|-----------|
| Thread model | `pthread_create` per Zephyr thread | `corosensei` stackful fibers | **No** — by design |
| Scheduler | POSIX `pthread_cond_wait` + mutexes | Deterministic event loop | **No** |
| Time | `clock_gettime()` / `setitimer()` | Virtual `Tick` counter | **Conceptually** |
| Interrupts | POSIX signals (`SIGALRM`, etc.) | Virtual IRQ controller | **Conceptually** |
| Peripherals | POSIX pipes/sockets + shared memory | Virtual device models | **Conceptually** |
| Build system | CMake + Kconfig + devicetree | CMake + Kconfig + devicetree | **Yes** (external) |

The **conceptual model** of native_sim is reusable:

- Application code runs as native host code (no CPU emulation)
- Peripherals are replaced with host-friendly models
- A "HW models" layer provides simulated UART, timer, etc.
- The build system is Zephyr's native build system

The **runtime model** is NOT reusable because:

- native_sim uses host OS threads → violates our determinism-first principle
- native_sim uses wall-clock time → violates virtual-time requirement
- native_sim's interrupt model uses POSIX signals → non-deterministic
- native_sim's networking uses host TAP/TUN → non-deterministic

Our replacement: swap native_sim's `posix_arch.c` (pthread + signal) for
`sim_arch.c` (corosensei + virtual IRQ).  The rest of the Zephyr kernel
(scheduler, objects, drivers) runs unmodified — just like our FreeRTOS port.

---

## 4. Which Parts Rely on Linux/POSIX Assumptions?

native_sim's architecture layer (`arch/posix/`) assumes:

| POSIX dependency | Used for | Our replacement |
|------------------|----------|-----------------|
| `pthread_create` | Zephyr thread → host thread | `corosensei` fiber |
| `pthread_cond_wait` | Thread blocking/signaling | Virtual event scheduling |
| `pthread_mutex_lock` | Critical section emulation | `sim_enter_critical()` |
| `clock_gettime()` | Tick timer | `sim_now_ticks()` + virtual timer |
| `SIGALRM` / `timer_create` | Tick interrupt | Virtual timer IRQ |
| `sigaction` / `sigprocmask` | IRQ masking | `sim_enter_critical()` counter |
| `select()` / `poll()` | Host I/O (UART, net) | Virtual devices or host-poller |
| `mmap` + `mprotect` | Stack guard pages | Optional: `corosensei` stack guard |
| `getpid()` / `kill()` | Process-level signals | N/A (single process) |
| `/tmp/` filesystem | Semaphore backing store | In-process data structure |
| `dlopen()` / `dlsym()` | Dynamic driver loading | `inventory` registry |

**Critical finding**: All POSIX dependencies are in the `arch/posix/` layer.
The Zephyr kernel core (`kernel/`, `subsys/`, `drivers/`) does NOT depend on
POSIX.  By providing a custom `arch/sim/` that maps these to our simulator ABI,
the rest of Zephyr runs unmodified.

The `native_sim` board also uses:

- POSIX command-line argument parsing → we would keep this for config
- Environment variables for simulator config → we would keep this
- `atexit()` handlers for cleanup → keep (host process exits normally)

These are acceptable — they don't affect determinism.

---

## 5. Can Zephyr Thread Switching Be Mapped to corosensei?

**Yes.**  The mapping is structurally identical to our FreeRTOS port.

Zephyr's `arch_switch()` is the single context-switch primitive.  It:

1. Saves the current thread's callee-saved registers onto its stack
2. Switches to the new thread's stack
3. Restores the new thread's callee-saved registers
4. Returns into the new thread's context

In our simulator, `arch_switch()` becomes:

```c
void arch_switch(void *switch_to, void **switch_from) {
    // Save current fiber's return point
    *switch_from = /* current thread */;
    // Set the new thread as current
    sim_set_current_thread(switch_to);
    // Yield to the scheduler
    sim_port_yield();
    // When resumed, restore new thread's context
}
```

The challenge: Zephyr's `arch_switch()` uses inline assembly to save/restore
callee-saved registers.  In our simulator, we don't need to save/restore
registers at all — the host CPU's registers are preserved across
`corosensei::suspend()`/`resume()` because we're using native stack switching,
not context-switching between separate stacks on the same host thread.

**Key insight**: corosensei already saves/restores all registers via the
calling convention.  When fiber A calls `suspend()`, corosensei saves A's
stack pointer and switches to the scheduler's stack.  When the scheduler
`resume()`s fiber B, corosensei restores B's stack pointer and returns.
The register state is automatically correct — no explicit arch_switch needed.

This means our `arch/sim/switch.S` can be:

```asm
// arch_switch for simulator — no-op, delegating to fiber runtime
// void arch_switch(void *switch_to, void **switch_from)
arch_switch:
    ret
```

The actual context switch is handled by:
1. Zephyr scheduler selects next thread → updates `_kernel.current`
2. Rust scheduler calls `sim_set_current_thread()` to map TCB → fiber
3. Rust scheduler resumes the fiber via `corosensei::resume()`

This is the same pattern as our FreeRTOS port, where `pxPortInitialiseStack`
stores metadata and `sim_start_scheduler` handles fiber scheduling.

---

## 6. What Generated Headers/Config Artifacts Are Unavoidable?

Zephyr's build system generates several headers that the kernel source depends
on.  These cannot be eliminated without forking Zephyr's core:

| Artifact | Generator | Purpose | Unavoidable? |
|----------|-----------|---------|--------------|
| `autoconf.h` | Kconfig (`kconfiglib.py`) | `CONFIG_*` defines | **Yes** — kernel uses these everywhere |
| `devicetree_generated.h` | DTS (`dtlib.py` → `edtlib.py` → `gen_defines.py`) | `DT_N_S_*` macros for peripheral addresses, IRQ numbers, etc. | **Yes** — drivers depend on these |
| `driver_validation_h_target` | `gen_kobject_list.py` | `SYS_INIT()` driver init ordering | **Yes** for any board with drivers |
| `syscall_dispatch.c` | `gen_syscalls.py` | Userspace syscall table | Only if `CONFIG_USERSPACE=y` (we would disable) |
| `kobject_hash.gperf` | `gen_kobject_list.py` | Kernel object permissions | Only if `CONFIG_USERSPACE=y` |
| `offsets.h` | `gen_offset_header.py` | `struct k_thread` field offsets | **Yes** for `arch_switch()` — but we can stub |
| `include/generated/syscall_list.h` | `parse_syscalls.py` | Syscall IDs | Only if `CONFIG_USERSPACE=y` |
| `linker.cmd` | `linker_script/common-ram.ld` + board | Memory layout | **Yes** — but we can use a trivial flat layout |

The practical approach:

1. **Accept these tool dependencies for Zephyr builds.**  The HANDOFF.md
   non-goal 6 states: "'No Python/codegen hacks' applies to this simulator
   engine, not necessarily to upstream Zephyr's own build pipeline."

2. **Build Zephyr externally** using `west build -b sim`.
   The simulator's `build.rs` only links the resulting `.a` file.

3. **Disable CONFIG_USERSPACE** to eliminate syscall-related codegen.

4. **Use a minimal devicetree** with only the peripherals we model (UART, timer).

5. **Use a flat linker script** — the simulator process has the full host
   address space; we don't need MCU memory layout.

The result: the user runs `west build` once to produce the Zephyr artifacts,
then `cargo build` links them into the simulator.  The Python tooling runs in
the `west build` step, NOT in `cargo build`.

---

## 7. What Is the Smallest Zephyr "Hello Thread" Proof of Concept?

The smallest PoC has these components:

### Guest (Zephyr) side:

```c
// main.c — Zephyr application
#include <zephyr/kernel.h>

void thread_entry(void *a, void *b, void *c) {
    printk("Hello from Zephyr thread!\n");
    k_msleep(100);
    printk("Thread woke up!\n");
}

K_THREAD_DEFINE(my_thread, 1024,
                thread_entry, NULL, NULL, NULL,
                7, 0, 0);
```

### Simulator (Rust) side:

1. **`arch/sim/`** — new Zephyr architecture directory
   - `arch_switch()` → no-op (delegated to corosensei)
   - `arch_irq_lock()` → `sim_enter_critical()`
   - `arch_irq_unlock()` → `sim_exit_critical()`
   - `arch_k_cycle_get_32()` → `sim_now_ticks()`
   - `z_arm_fatal_error()` → trace fatal + exit

2. **`boards/sim/`** — new Zephyr board
   - `board.cmake` — build flags
   - `Kconfig.defconfig` — board defaults
   - `sim.dts` — minimal devicetree (one UART, one timer)
   - `sim_defconfig` — `CONFIG_BOARD_SIM=y`, disable unnecessary subsystems

3. **`sim-ffi` additions** — new C ABI exports
   - `sim_set_current_thread(void *tcb)` — like `sim_set_current_task_by_id`
   - `sim_thread_create(...)` — register Zephyr thread as Rust fiber
   - `sim_sched_lock()` / `sim_sched_unlock()` — Zephyr scheduler lock

4. **`sim-runner` updates** — link Zephyr's `.a` instead of FreeRTOS's `.a`

### Build flow:

```bash
# Step 1: Build Zephyr as a static library
west build -b sim zephyr_app/ -- -DCONFIG_BUILD_OUTPUT_STATIC_LIBRARY=y
# Produces: build/zephyr/libzephyr.a + build/zephyr/include/generated/*.h

# Step 2: Build simulator (links libzephyr.a)
cd costar/
ZEPHYR_BUILD_DIR=../zephyr_app/build cargo build

# Step 3: Run
cargo run
# Output:
#   Hello from Zephyr thread!
#   Thread woke up!
#   === Simulation Trace (8 events) ===
```

### Estimated effort:

| Task | Effort | Risk |
|------|--------|------|
| Create `arch/sim/` with no-op `arch_switch` | ~2 days | Low — pattern established by FreeRTOS port |
| Create `boards/sim/` with minimal DTS | ~1 day | Low — many examples in Zephyr tree |
| Add Zephyr-specific C ABI exports | ~2 days | Low — mirror FreeRTOS bridge |
| `build.rs` linking Zephyr `.a` | ~1 day | Medium — header include paths need care |
| `K_THREAD_DEFINE` → fiber mapping | ~3 days | Medium — Zephyr's static thread init is more complex than FreeRTOS's `xTaskCreate` |
| `k_msleep` / `k_sleep` bridging | ~1 day | Low — maps to `sim_task_delay_until` |
| Thread priority scheduling | ~2 days | Medium — Zephyr has O(1) priority scheduler vs FreeRTOS's round-robin |
| IRQ model (multi-level interrupts) | ~3 days | High — Zephyr's IRQ model is more complex than FreeRTOS's critical sections |
| Integration testing | ~2 days | Medium |
| **Total PoC** | **~17 days** | |

### Comparison to FreeRTOS PoC:

The FreeRTOS MVP took ~10 phases to reach "two tasks with queue + delay".
Zephyr is inherently more complex due to:

- Build system complexity (CMake + Kconfig + DTS vs `cc` crate)
- Larger kernel (scheduler, object model, driver model, power management)
- Multi-level interrupt model (FreeRTOS has simpler critical sections)
- Static initialization model (`K_THREAD_DEFINE`, `SYS_INIT`, device PM)

However, our existing infrastructure (event queue, fiber runtime, virtual
devices, IRQ controller, trace system) is directly reusable.  The delta is
the arch port + board definition + Zephyr-specific bridge functions.

---

## Summary Verdict

**Zephyr support is feasible but is a substantial engineering effort (~3-4 weeks for a minimal PoC, ~2-3 months for feature parity with the FreeRTOS port).**

The key architectural insight: Zephyr's arch/board port interface is a superset
of FreeRTOS's port layer.  Our existing corosensei fiber runtime, event queue,
virtual time, IRQ controller, and device models are all reusable.  The delta is:

1. Zephyr's build system must run externally (accepted per HANDOFF §13.3)
2. A custom `arch/sim/` must replace POSIX threads with corosensei fibers
3. A minimal board definition must provide devicetree + Kconfig fragments
4. Zephyr's init sequence (`SYS_INIT`, `K_THREAD_DEFINE`) must be bridged
5. Zephyr's multi-level IRQ model needs a richer virtual IRQ controller

The FreeRTOS MVP proves the core concept works.  Zephyr is the natural next
target — but it should NOT be attempted until the FreeRTOS port is stable,
deterministic networking is proven, and the simulator's public API is mature.
