# Zephyr cc Crate Compilation — Arch Layer & build.rs Design

Status: **Design Phase** (not yet implemented)
Target: Replace `west build` with direct cc crate compilation of Zephyr kernel sources

---

## 1. Architecture Overview

The goal is to compile the real Zephyr kernel directly through the `cc` crate
in `sim-zephyr-port/build.rs`, eliminating the `west build` dependency for basic
kernel functionality.

The Zephyr kernel has a well-defined architecture port interface. By providing a
custom `arch/sim/` layer that maps Zephyr's arch hooks to our existing corosensei
fiber runtime, the kernel core runs unmodified — just like our FreeRTOS port.

```
┌───────────────────────────────────────────────────────────┐
│                    Zephyr Application                      │
│  (hello_world.c, app code using k_thread_create, k_sleep) │
├───────────────────────────────────────────────────────────┤
│               Zephyr Kernel Core (unmodified)              │
│  kernel/     — init, sched, thread, timeout, workq, ...   │
│  include/    — kernel.h, kernel_structs.h, ...            │
├───────────────────────────────────────────────────────────┤
│           NEW: arch/sim/  (our replacement layer)          │
│  sim_arch.c  — arch_switch, arch_irq_lock, arch_new_...   │
│  nsi_shim.c  — nsi_vprint_*, nsi_simu_time, nsi_exit     │
│  config/     — pre-generated autoconf.h, offsets.h, ...   │
├───────────────────────────────────────────────────────────┤
│           Native Simulator Interface (nct/nce/hw)          │
│  nct_*   — thread emulator (provided by sim-runner Rust)  │
│  nce_*   — CPU emulator (provided by sim-runner Rust)     │
│  hw_*    — HW model stubs (provided by sim-runner Rust)   │
├───────────────────────────────────────────────────────────┤
│               Rust Fiber Runtime (existing)                │
│  sim-ffi    — sim_now_ticks, sim_port_yield, sim_enter_...│
│  sim-fiber  — Fiber, YieldReason, ResumeReason            │
│  sim-runner — drain loop, nct/nce/hw #[no_mangle] exports │
└───────────────────────────────────────────────────────────┘
```

---

## 2. Arch Layer File Inventory

### 2.1 New Files (to be created in `crates/sim-zephyr-port/c/`)

| File | Purpose | Est. Lines |
|------|---------|------------|
| `sim_arch.c` | Main arch implementation — all arch_*, posix_* functions | ~300 |
| `include/kernel_arch_func.h` | arch_swap declaration, arch_thread_return_value_set, arch_is_in_isr, arch_kernel_init | ~40 |
| `include/kernel_arch_data.h` | callee_saved extension fields (thread_status pointer) | ~20 |
| `include/asm_inline.h` | Dispatcher that includes asm_inline_gcc.h | ~20 |
| `include/asm_inline_gcc.h` | arch_irq_lock/unlock inlines → sim_enter/exit_critical | ~25 |
| `include/arch.h` | Main arch header — arch_k_cycle_get_32/64, arch_nop, arch_cpu_irqs_are_enabled, includes all sub-headers | ~60 |
| `include/sim_core.h` | sim_thread_status_t typedef (replaces posix_thread_status_t), posix_* function declarations | ~50 |
| `include/sim_soc_if.h` | posix_irq_lock/unlock, posix_halt_cpu, posix_irq_enable/disable declarations | ~40 |
| `include/offsets_short_arch.h` | Stub — no offset constants needed (no inline asm for switch) | ~10 |
| `include/thread.h` | ARCH_THREAD_STACK_RESERVED, ARCH_THREAD_STACK_DEFINE macros | ~25 |

**Total: ~10 new header files, ~590 lines**

### 2.2 Existing Files (already in the crate)

| File | Changes needed | Lines (existing) |
|------|---------------|-------------------|
| `c/nsi_shim.c` | Already provides nsi_vprint_*, nsi_simu_time, nsi_hws_get_time, nsi_exit, nsi_* stubs | 76 |
| `c/zephyr_arch.c` | **Replace** — currently a standalone fake; replace with real sim_arch.c | (removed) |
| `c/zephyr_arch.h` | **Replace** — currently standalone declarations; replace with full arch headers | (removed) |
| `c/sim_zephyr_abi.h` | Keep for Zephyr-specific ABI (sim_zephyr_register_thread, etc.) | 84 |
| `c/zephyr_glue.c` | Keep for convenience wrappers until we have real kernel glue | 28 |

### 2.3 Config Headers (pre-generated)

| File | Source | Est. Lines |
|------|--------|------------|
| `config/autoconf.h` | Pre-generated from a minimal `west build -b sim` run | ~150 |
| `config/devicetree_generated.h` | Pre-generated from the sim.dts devicetree | ~200 |
| `config/offsets.h` | Pre-generated from gen_offset_header.py for struct k_thread | ~30 |

**Total: 3 pre-generated config headers, ~380 lines**

### 2.4 Zephyr Kernel Source Files (vendored or path-referenced)

For a minimal hello-world build, we need these Zephyr kernel .c files
(identified from `kernel/CMakeLists.txt` and `arch/posix/core/CMakeLists.txt`):

**kernel/** (core):
- `init.c` — kernel initialization (z_cstart, prepare_multithreading, switch_to_main)
- `sched.c` — scheduler (z_reschedule, z_ready_thread, z_move_thread_to_end_of_prio_q)
- `thread.c` — thread lifecycle (z_thread_create, z_thread_entry, z_thread_single_abort)
- `timeout.c` — timeout/delay (z_add_timeout, z_clock_announce, z_clock_set_timeout)
- `timer.c` — kernel timers (k_timer_start, k_timer_stop, k_timer_status_sync)
- `queue.c` — ready queue operations
- `work.c` — workqueue (optional, skip for PoC)
- `idle.c` — idle thread
- `device.c` — device model (optional, skip for PoC if no drivers)
- `errno.c` — errno support
- `version.c` — version string
- `banner.c` — boot banner (optional)

**arch/sim/** (our layer, list above):
- sim_arch.c

**subsys/tracing/** (may be needed):
- `tracing_none.c` — no-op tracing when CONFIG_TRACING=n

**Estimated Zephyr .c files for PoC: ~12 kernel files + 1 arch file = ~13 C sources**

Total vendor source lines: ~8,000-12,000 (kernel core is compact but dense)

---

## 3. Arch Hook → Rust Symbol Mapping

### 3.1 Core Arch Interface (called by Zephyr kernel)

| Zephyr Arch Function | Our Implementation | Rust Symbol | Notes |
|---------------------|-------------------|-------------|-------|
| `arch_switch(unsigned int key)` | Save key in `_current->callee_saved.key`, call `z_current_thread_set(ready_q.cache)`, call `nct_swap_threads()`, on resume `arch_irq_unlock(key)`, return `_current->callee_saved.retval` | `nct_swap_threads` → `sim_fiber::suspend_active_fiber(YieldReason::RtosPortYield)` | The yield suspends the single fiber; scheduler resumes it with the new thread already set as current |
| `arch_irq_lock(void) → unsigned int` | `sim_enter_critical(); return 0;` | `sim_enter_critical` | TLS nesting counter; key not used |
| `arch_irq_unlock(unsigned int key)` | `sim_exit_critical();` | `sim_exit_critical` | Delivers deferred IRQs when nesting hits zero |
| `arch_k_cycle_get_32(void) → uint32_t` | `return (uint32_t)sim_now_ticks();` | `sim_now_ticks` | Atomic relaxed read |
| `arch_k_cycle_get_64(void) → uint64_t` | `return sim_now_ticks();` | `sim_now_ticks` | Direct passthrough |
| `arch_new_thread(thread, stack, stack_ptr, entry, p1, p2, p3)` | Alloc `sim_thread_status_t` on stack, store entry/args, call `posix_new_thread()` → `nct_new_thread()`, store `thread_status` in `thread->callee_saved.thread_status` | `nct_new_thread` → stores payload pointer | Thread status stored in callee_saved for later use by swap |
| `arch_switch_to_main_thread(main_thread, stack_ptr, _main)` | Call `z_current_thread_set(ready_q.cache)`, call `nct_first_thread_start()` | `nct_first_thread_start` | Never returns — enters Zephyr's main thread |
| `arch_system_halt(unsigned int reason)` | Trace fatal + `nsi_exit(0)` | `nsi_exit` (C) or just `exit(0)` | Could also call `sim_trace_u32("fatal", reason)` |
| `arch_cpu_idle(void)` | `nct_swap_threads(...)` or just `sim_port_yield()` | `sim_port_yield` | Idle yields to scheduler; scheduler advances time |
| `arch_cpu_atomic_idle(unsigned int key)` | `arch_irq_unlock(key); arch_cpu_idle();` | — | Combine unlock + idle |
| `arch_irq_enable(unsigned int irq)` | No-op (virtual IRQ controlled by sim_irq_raise/clear) | — | Stub |
| `arch_irq_disable(unsigned int irq)` | No-op | — | Stub |
| `arch_irq_is_enabled(unsigned int irq)` | Return 1 | — | All IRQs always "enabled" in virtual model |
| `arch_thread_name_set(thread, str)` | Call `nct_thread_name_set()` | `nct_thread_name_set` | Delegates to Rust |
| `arch_is_in_isr(void) → bool` | `return _kernel.cpus[0].nested != 0;` | — | Pure Zephyr kernel state read |
| `arch_kernel_init(void)` | No-op (or call soc_per_core_init_hook if needed) | — | Hook point for board init |
| `arch_thread_return_value_set(thread, value)` | `thread->callee_saved.retval = value;` | — | Inline, pure C |
| `sys_clock_cycle_get_32(void) → uint32_t` | `return (uint32_t)sim_now_ticks();` | `sim_now_ticks` | Forwarded by arch_k_cycle_get_32 |
| `sys_clock_cycle_get_64(void) → uint64_t` | `return sim_now_ticks();` | `sim_now_ticks` | Forwarded by arch_k_cycle_get_64 |

### 3.2 Native Simulator Interface (called by our arch layer → Rust)

| C Function | Our Implementation | Rust Symbol (in zephyr_glue.rs) |
|-----------|-------------------|----------------------------------|
| `posix_arch_init()` | `te_state = nct_init(posix_arch_thread_entry);` | `nct_init` |
| `posix_arch_clean_up()` | `nct_clean_up(te_state);` | `nct_clean_up` |
| `posix_swap(next, this)` | `nct_swap_threads(te_state, next);` | `nct_swap_threads` |
| `posix_new_thread(payload)` | `return nct_new_thread(te_state, payload);` | `nct_new_thread` |
| `posix_main_thread_start(next)` | `nct_first_thread_start(te_state, next);` | `nct_first_thread_start` |
| `posix_abort_thread(idx)` | `nct_abort_thread(te_state, idx);` | `nct_abort_thread` |
| `posix_arch_thread_entry(ts)` | `posix_irq_full_unlock(); z_thread_entry(ts->entry_point, ts->arg1, ts->arg2, ts->arg3);` | (pure C — no Rust call) |
| `posix_irq_lock() → unsigned int` | `sim_enter_critical(); return 0;` | `sim_enter_critical` |
| `posix_irq_unlock(key)` | `sim_exit_critical();` | `sim_exit_critical` |
| `posix_irq_full_unlock()` | Loop `sim_exit_critical()` until nesting = 0 | `sim_exit_critical` |
| `posix_halt_cpu()` | `nce_halt_cpu(te_state);` | `nce_halt_cpu` |
| `posix_atomic_halt_cpu(key)` | `posix_irq_unlock(key); posix_halt_cpu();` | — |
| `posix_boot_cpu()` | `nce_init(); nce_boot_cpu(..., z_cstart);` | `nce_init`, `nce_boot_cpu` |

### 3.3 NSI Infrastructure (provided in nsi_shim.c)

| Function | Implementation | Notes |
|----------|---------------|-------|
| `nsi_vprint_trace(fmt, vargs)` | `vfprintf(stdout, fmt, vargs); fflush(stdout);` | Already exists |
| `nsi_vprint_warning(fmt, vargs)` | `fprintf(stderr, "WARNING: "); vfprintf(...)` | Already exists |
| `nsi_vprint_error_and_exit(fmt, vargs)` | `fprintf(stderr, "ERROR: "); vfprintf(...); exit(0);` | Already exists |
| `nsi_simu_time` | `uint64_t` global — set by Rust before each fiber resume | Already exists |
| `nsi_hws_get_time()` | `return nsi_simu_time;` | Already exists |
| `nsi_exit(code)` | `exit(code);` | Already exists |
| `nsi_trace_over_tty(fn)` | `return 0;` | Already exists |
| `nsi_add_command_line_opts()` | No-op | Already exists |
| `nsi_get_cmd_line_args()` | `return NULL;` | Already exists |

---

## 4. build.rs Design

### 4.1 Source File Strategy

**Option A (recommended): Reference Zephyr source tree via env var**

```rust
let zephyr_base = std::env::var("ZEPHYR_BASE")
    .unwrap_or_else(|_| "../../zephyr".to_string());
```

- Pros: No vendoring, stays in sync with user's Zephyr install
- Cons: Requires Zephyr clone on disk, path management

**Option B: Git submodule**

Add Zephyr as a submodule at `crates/sim-zephyr-port/zephyr/` and reference it.

- Pros: Pinned version, self-contained
- Cons: ~200MB repo clone, maintenance burden

**Option C: Curated subset (vendored files)**

Copy the exact kernel .c/.h files needed into `crates/sim-zephyr-port/zephyr_kernel/`.

- Pros: Minimal, no external deps for core build
- Cons: Manual sync with upstream Zephyr, legal (Apache 2.0 requires attribution)

**Recommendation**: Start with **Option A** using `ZEPHYR_BASE` env var, with a fallback to a curated vendored subset for CI. The build.rs warns if ZEPHYR_BASE is not set and falls back to the standalone test.

### 4.2 Include Paths

```
build.include("c/include")              // Our arch headers (kernel_arch_func.h, arch.h, ...)
    .include("c")                        // sim_zephyr_abi.h, nsi_shim declarations
    .include("config")                   // Pre-generated: autoconf.h, devicetree_generated.h, offsets.h
    .include("../sim-ffi/include")       // sim_abi.h
    .include("{ZEPHYR_BASE}/include")    // Zephyr public headers (zephyr/kernel.h, ...)
    .include("{ZEPHYR_BASE}/arch/sim/include") // Our sim arch headers (installed into Zephyr tree)
    .include("{ZEPHYR_BASE}/kernel/include")   // Kernel private headers
    .include("{ZEPHYR_BASE}/include/generated") // Generated headers from west build (when available)
```

For the cc crate path, the critical include order is:
1. Our `c/include/` first (overrides Zephyr's arch/posix headers)
2. Our `config/` second (pre-generated Kconfig+DTS output)
3. Zephyr public headers
4. Zephyr kernel private headers

### 4.3 Compile Defines

```rust
build.define("CONFIG_ARCH_SIM", Some("1"))
    .define("CONFIG_ARCH", Some("\"sim\""))
    .define("CONFIG_SIMULATION_HOST_MODE", Some("1"))
    .define("CONFIG_64BIT", Some("1"))           // Always 64-bit host
    .define("CONFIG_SMP", None)                  // Unset — single CPU
    .define("CONFIG_MP_NUM_CPUS", Some("1"))
    .define("CONFIG_USERSPACE", None)            // Disabled for PoC
    .define("CONFIG_TRACING", None)              // Disabled for PoC
    .define("CONFIG_INSTRUMENT_THREAD_SWITCHING", None)
    .define("CONFIG_PM", None)                   // No power management
    .define("CONFIG_FPU", None)                  // No FPU context switching
    .define("CONFIG_FPU_SHARING", None)
    .define("CONFIG_ARCH_HAS_CUSTOM_SWAP_TO_MAIN", Some("1"))
    .define("CONFIG_ARCH_HAS_THREAD_ABORT", Some("1"))
    .define("CONFIG_ARCH_HAS_CUSTOM_BUSY_WAIT", Some("1"))
    .define("CONFIG_DYNAMIC_INTERRUPTS", None)
    .define("CONFIG_IRQ_OFFLOAD", None)
    .define("CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC", Some("1000000"))
    .define("CONFIG_SYS_CLOCK_TICKS_PER_SEC", Some("10000"))
    .define("CONFIG_ARCH_POSIX", None)          // MUST unset — we're ARCH_SIM, not POSIX
    .define("CONFIG_BOARD_SIM", Some("1"))
    .define("CONFIG_NATIVE_APPLICATION", Some("1"))
    .define("CONFIG_BUILD_OUTPUT_STATIC_LIBRARY", Some("1"));
```

Most of these come from the pre-generated `autoconf.h`, but the build.rs
provides the critical ones as a fallback and for cc crate caching.

### 4.4 Compiler Flags

```rust
if cfg!(any(target_os = "linux", target_os = "macos")) {
    build.flag_if_supported("-Wall")
        .flag_if_supported("-Wextra")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-sign-compare")
        .flag_if_supported("-Wno-missing-field-initializers")
        .flag_if_supported("-Wno-unused-function")
        .flag_if_supported("-fno-omit-frame-pointer")
        .flag_if_supported("-imacros")           // For Zephyr's generated headers
        .flag_if_supported("-Wno-address-of-packed-member");
}
```

### 4.5 Feature Flags

Add Cargo features to `sim-zephyr-port/Cargo.toml`:

```toml
[features]
default = ["standalone"]
standalone = []          # Use standalone_test.c (fake Zephyr API, no kernel sources)
real-kernel = []         # Compile real Zephyr kernel sources (requires ZEPHYR_BASE)
```

The `real-kernel` feature gates the kernel source compilation. When enabled,
the build.rs looks for `ZEPHYR_BASE` and compiles the real kernel. When
disabled (default), it compiles only the standalone test.

### 4.6 Complete build.rs Pseudocode

```rust
fn main() {
    // Always compile the arch layer and NSI shim
    let mut build = cc::Build::new();

    // ── Arch layer (always compiled) ──────────────────────
    build.file("c/sim_arch.c")
        .file("c/nsi_shim.c")
        .include("c/include")       // Our arch headers
        .include("c")               // sim_zephyr_abi.h
        .include("config")          // Pre-generated config
        .include("../sim-ffi/include"); // sim_abi.h

    build.define("SIMULATION_HOST_MODE", Some("1"));

    // ── Feature gate: real Zephyr kernel ─────────────────
    #[cfg(feature = "real-kernel")]
    {
        let zephyr_base = std::env::var("ZEPHYR_BASE")
            .expect("ZEPHYR_BASE must be set when feature 'real-kernel' is enabled");

        build.include(format!("{}/include", zephyr_base))
            .include(format!("{}/arch/sim/include", zephyr_base))
            .include(format!("{}/kernel/include", zephyr_base));

        // Kernel source files
        let kernel_files = [
            "kernel/init.c", "kernel/sched.c", "kernel/thread.c",
            "kernel/timeout.c", "kernel/timer.c", "kernel/queue.c",
            "kernel/idle.c", "kernel/device.c", "kernel/errno.c",
            "kernel/version.c", "kernel/banner.c", "kernel/work.c",
            "subsys/tracing/tracing_none.c",
        ];
        for f in &kernel_files {
            build.file(format!("{}/{}", zephyr_base, f));
        }

        // App entry point
        build.file(format!("{}/app/hello_world.c", zephyr_base));

        // Config from west build (if available)
        let generated = format!("{}/build/zephyr/include/generated", zephyr_base);
        if std::path::Path::new(&generated).exists() {
            build.include(&generated);
        }
    }

    // ── Standalone test (default feature) ────────────────
    #[cfg(not(feature = "real-kernel"))]
    {
        build.file("c/zephyr_glue.c")
            .file("../../c_firmware/zephyr_app/standalone_test.c");
    }

    // ── Compile ──────────────────────────────────────────
    build.compile("embedded_zephyr_payload");
}
```

---

## 5. Symbol Provider Architecture

The symbols needed by the compiled Zephyr C code are provided at link time:

| Source | Symbol Category |
|--------|----------------|
| `sim-ffi` (Rust, `#[no_mangle]`) | `sim_now_ticks`, `sim_port_yield`, `sim_enter_critical`, `sim_exit_critical`, `sim_task_delay_until`, `sim_trace_u32`, `sim_irq_*`, `sim_zephyr_*` |
| `sim-runner/src/zephyr_glue.rs` (Rust, `#[no_mangle]`) | `nct_init`, `nct_new_thread`, `nct_swap_threads`, `nct_first_thread_start`, `nct_abort_thread`, `nct_get_unique_thread_id`, `nct_thread_name_set`, `nct_clean_up`, `nce_init`, `nce_boot_cpu`, `nce_halt_cpu`, `nce_wake_cpu`, `nce_is_cpu_running`, `nce_terminate`, `hw_irq_ctrl_*`, `hwtimer_*` |
| `sim-zephyr-port/c/nsi_shim.c` (C, compiled by cc) | `nsi_vprint_trace`, `nsi_vprint_warning`, `nsi_vprint_error_and_exit`, `nsi_simu_time`, `nsi_hws_get_time`, `nsi_exit`, `nsi_trace_over_tty`, `nsi_add_command_line_opts`, `nsi_get_cmd_line_args` |
| `sim-zephyr-port/c/sim_arch.c` (C, compiled by cc) | `arch_switch`, `arch_irq_lock`, `arch_irq_unlock`, `arch_new_thread`, `arch_switch_to_main_thread`, `arch_k_cycle_get_32`, `arch_system_halt`, `posix_arch_init`, `posix_swap`, `posix_arch_thread_entry`, `posix_irq_lock`, `posix_halt_cpu`, `sys_clock_cycle_get_32`, etc. |

No new Rust `#[no_mangle]` exports are needed — the existing set from Phase 13
(Zephyr standalone test) and the Phase 16 zephyr_linked integration already
provide all required symbols. The `nct_*`/`nce_*`/`hw_*` symbols in
`sim-runner/src/zephyr_glue.rs` exactly match the native_simulator interface.

---

## 6. Config Header Strategy

### 6.1 Three Tiers of Config

**Tier 1: Pre-generated (checked into repo)**
- `config/autoconf.h` — Minimal CONFIG_* defines
- `config/devicetree_generated.h` — Minimal DT macros for UART + timer
- `config/offsets.h` — Struct k_thread field offsets

Generated once from `west build -b sim` with a minimal config, then frozen.
Works for the basic kernel (no drivers beyond UART/timer).

**Tier 2: Build-time generation**
- Run `kconfiglib` + `dtlib` + `edtlib` Python scripts during `build.rs`
- Requires Python and Zephyr's scripts/ directory
- Complex, but handles arbitrary Kconfig/DTS configurations

**Tier 3: External west build (current approach)**
- User runs `west build` to produce `build/zephyr/include/generated/`
- `build.rs` includes that directory
- Full Zephyr build system fidelity

**Recommendation**: Tier 1 for the PoC, Tier 3 as the escape hatch for
complex configurations. Tier 2 is deferred due to HANDOFF.md non-goal 6
("No Python/codegen hacks in the simulator engine").

### 6.2 Minimal autoconf.h Example

```c
// Pre-generated minimal config for sim arch
#define CONFIG_ARCH_SIM 1
#define CONFIG_ARCH "sim"
#define CONFIG_BOARD_SIM 1
#define CONFIG_SIMULATION_HOST_MODE 1
#define CONFIG_64BIT 1
#define CONFIG_MP_NUM_CPUS 1
#define CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC 1000000
#define CONFIG_SYS_CLOCK_TICKS_PER_SEC 10000
#define CONFIG_ARCH_HAS_CUSTOM_SWAP_TO_MAIN 1
#define CONFIG_ARCH_HAS_THREAD_ABORT 1
#define CONFIG_ARCH_HAS_CUSTOM_BUSY_WAIT 1
#define CONFIG_BUILD_OUTPUT_STATIC_LIBRARY 1
#define CONFIG_NATIVE_APPLICATION 1
#define CONFIG_MAIN_THREAD_PRIORITY 0
#define CONFIG_MAIN_STACK_SIZE 4096
#define CONFIG_IDLE_STACK_SIZE 2048
#define CONFIG_ISR_STACK_SIZE 4096
#define CONFIG_MAX_THREAD_BYTES 3
#define CONFIG_NUM_COOP_PRIORITIES 16
#define CONFIG_NUM_PREEMPT_PRIORITIES 15
#define CONFIG_NUM_METAIRQ_PRIORITIES 0
#define CONFIG_TIMESLICING 0
// ... ~30 more CONFIG_* for basic kernel operation
```

---

## 7. Known Risks and Mitigations

| # | Risk | Severity | Mitigation |
|---|------|----------|------------|
| 1 | **Config header mismatch**: Zephyr kernel uses `#ifdef CONFIG_*` extensively. Missing a required option causes silent miscompilation or link errors. | High | Pre-generate from a known-good `west build` run. Add `#error` checks in our wrappers for critical configs. |
| 2 | **Source file explosion**: The Zephyr kernel has 500+ .c files. Identifying the minimal set for hello-world is manual and error-prone. | Medium | Start from `kernel/CMakeLists.txt` zephyr_sources list. Iteratively add files based on linker errors. |
| 3 | **Linker section dependencies**: Zephyr uses linker sections for `SYS_INIT`, `_sw_isr_table`, `kobject_hash`. Without proper linker support, these features fail silently. | Medium | Disable CONFIG_USERSPACE (eliminates kobject_hash). Provide static arrays for `_sw_isr_table`. Skip SYS_INIT for PoC by disabling all drivers. |
| 4 | **callee_saved layout compatibility**: Our `sim_thread_status_t` must match the layout expected by `arch_switch` and `arch_new_thread`. | Low | Use identical struct layout to `posix_thread_status_t`. Add compile-time assert on struct size. |
| 5 | **Missing kernel init sequence**: Zephyr's `z_cstart` → `prepare_multithreading` → `switch_to_main_thread` sequence must be preserved exactly. | High | Our arch layer must provide the same init hooks as posix arch. Test with known-good hello_world. |
| 6 | **Zephyr version drift**: If we pre-generate config headers, they may become stale when Zephyr upstream changes CONFIG_* semantics. | Medium | Pin Zephyr version via submodule or documented version requirement. Re-generate config headers on Zephyr version bumps. |
| 7 | **Double definition of nct_*/nce_* symbols**: sim-runner provides these as `#[no_mangle]`, and the C code declares them `extern`. If we also compile nct_*.c from Zephyr's native_simulator, they conflict. | Low | Only compile our `sim_arch.c`, NOT Zephyr's `posix_core_nsi.c`. Our file calls the Rust nct_* symbols directly. |
| 8 | **Include path shadowing**: Zephyr's `arch/posix/include/` has headers with the same names as our `arch/sim/include/`. The include order must put ours first. | Medium | In build.rs, put `c/include` first in the include chain. Use `-include` or wrapper headers to intercept `#include <arch/posix/...>` includes. Some Zephyr sources may hardcode `#include <arch/posix/posix_soc_if.h>` — we need to patch those or provide compatibility redirects. |

---

## 8. Implementation Sequence

### Phase A: Arch Layer Standalone (no kernel sources)
1. Create `c/sim_arch.c` with all arch_*/posix_* functions
2. Create `c/include/` with all 8 header files
3. Create `config/` with pre-generated autoconf.h, devicetree_generated.h, offsets.h
4. Compile `sim_arch.c` + `nsi_shim.c` and verify it compiles clean
5. Link against sim-runner and verify no undefined symbols

### Phase B: Single Kernel File Integration
1. Add ZEPHYR_BASE support to build.rs
2. Compile one kernel file at a time (start with `kernel/init.c`)
3. Fix include path issues, missing CONFIG_* defines
4. Iterate until all required kernel .c files compile

### Phase C: Hello World Boot
1. Write a minimal Zephyr hello_world.c application
2. Link everything together
3. Implement posix_boot_cpu() → nce_boot_cpu(z_cstart) setup in Rust
4. Run and verify the kernel boots, reaches main(), prints via printk

### Phase D: Thread + Sleep
1. Enable k_thread_create and k_sleep
2. Verify arch_switch works through the fiber runtime
3. Verify k_sleep advances virtual time correctly

### Phase E: Polish
1. CI integration (golden trace test)
2. Documentation
3. Remove west build dependency for basic use cases

---

## 9. Summary

| Metric | Value |
|--------|-------|
| New C source files | 1 (`sim_arch.c`, ~300 lines) |
| New header files | 8 (`c/include/*.h`, ~285 lines total) |
| Pre-generated config files | 3 (`config/*.h`, ~380 lines total) |
| Existing files to modify | 3 (`build.rs`, `Cargo.toml`, remove `zephyr_arch.c`) |
| Zephyr kernel .c files to compile | ~12 (kernel core subset) |
| New Rust exports needed | 0 (all symbols already exist) |
| Total est. new lines | ~1,000 (arch layer + headers + config) |
| Total est. modified lines | ~150 (build.rs rewrite) |
| Risk level | Medium — config header dependency is the main challenge |
| PoC effort estimate | ~5-7 days (arch layer + kernel integration + debug) |
