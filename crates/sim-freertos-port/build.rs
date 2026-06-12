//! Build script for sim-freertos-port.
//!
//! Compiles the C port layer and the guest firmware through the `cc`
//! crate.  The compiled object is linked into the final sim-runner binary.

fn main() {
    // Re-run if any C source or header changes.
    println!("cargo:rerun-if-changed=c/port.c");
    println!("cargo:rerun-if-changed=c/sim_hooks.c");
    println!("cargo:rerun-if-changed=c/sim_kernel_bridge.c");
    println!("cargo:rerun-if-changed=c/sim_coverage.c");
    println!("cargo:rerun-if-changed=c/portmacro.h");
    println!("cargo:rerun-if-changed=../sim-ffi/include/sim_abi.h");

    // Firmware sources
    println!("cargo:rerun-if-changed=../../c_firmware/app/main.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_interactive.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/tight_loop_demo.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/tasks.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/queue.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/list.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/timers.c");

    // Firmware headers
    for header in &[
        "FreeRTOS.h",
        "FreeRTOSConfig.h",
        "task.h",
        "queue.h",
        "list.h",
        "timers.h",
        "projdefs.h",
        "portable.h",
        "stack_macros.h",
        "StackMacros.h",
        "mpu_wrappers.h",
    ] {
        println!(
            "cargo:rerun-if-changed=../../c_firmware/freertos/include/{}",
            header
        );
    }

    let mut build = cc::Build::new();

    // ── Port layer ────────────────────────────────────────────────
    build
        .file("c/port.c")
        .file("c/sim_hooks.c")
        .file("c/sim_kernel_bridge.c")
        // sim_coverage.c is always compiled — it defines weak-ish
        // __sanitizer_cov_trace_pc_guard callbacks that are only
        // linked when -fsanitize-coverage is active.  Dead code
        // otherwise (two small no-op functions).
        .file("c/sim_coverage.c");

    // ── Guest firmware (FreeRTOS kernel + app) ────────────────────
    build
        .file("../../c_firmware/app/main.c")
        .file("../../c_firmware/app/main_interactive.c")
        .file("../../c_firmware/app/tight_loop_demo.c")
        .file("../../c_firmware/freertos/tasks.c")
        .file("../../c_firmware/freertos/queue.c")
        .file("../../c_firmware/freertos/list.c")
        .file("../../c_firmware/freertos/timers.c");

    // ── Include paths ─────────────────────────────────────────────
    build
        .include("c") // portmacro.h
        .include("../sim-ffi/include") // sim_abi.h
        .include("../../c_firmware/freertos/include"); // FreeRTOS.h, FreeRTOSConfig.h, etc.

    // ── Defines ───────────────────────────────────────────────────
    build
        .define("SIMULATION_HOST_MODE", Some("1"))
        .define("FREERTOS_PORT_SIM", Some("1"));

    // ── Platform-specific flags ───────────────────────────────────
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        build.flag_if_supported("-Wall");
        build.flag_if_supported("-Wextra");
        build.flag_if_supported("-Wno-unused-parameter");
        build.flag_if_supported("-Wno-sign-compare");
        build.flag_if_supported("-Wno-missing-field-initializers");
        build.flag_if_supported("-fno-omit-frame-pointer");

        // Tier 1 function-entry instrumentation — opt-in via env var.
        // When SIM_INSTRUMENT_FUNCTIONS=1, the compiler emits calls to
        // __cyg_profile_func_enter at every C function entry, which
        // triggers sim_budget_poll for CPU-bound stall detection.
        if std::env::var("SIM_INSTRUMENT_FUNCTIONS").as_deref() == Ok("1") {
            build.flag_if_supported("-finstrument-functions");
        }

        // Tier 3 edge-level instrumentation — opt-in via env var.
        // When SIM_INSTRUMENT_EDGES=1, we switch to Clang and add
        // -fsanitize-coverage=trace-pc-guard to insert callbacks at
        // every basic-block edge.  After a throttle, these call
        // sim_budget_poll, which can preempt tight while(1){} loops
        // that contain no function calls.
        //
        // Requires Clang (GCC's -fsanitize-coverage doesn't support
        // trace-pc-guard).  If Clang is not found, edge instrumentation
        // is silently skipped with a cargo:warning.
        if std::env::var("SIM_INSTRUMENT_EDGES").as_deref() == Ok("1") {
            // Check if Clang is available.
            let has_clang = std::process::Command::new("clang")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok();

            if has_clang {
                build.compiler("clang");
                build.flag_if_supported("-fsanitize-coverage=trace-pc-guard");
                // Prevent the coverage pass from pruning "uninteresting"
                // edges (e.g., unconditional branches in tight loops).
                build.flag_if_supported("-fsanitize-coverage-ignorelist=/dev/null");
                println!("cargo:warning=Edge instrumentation enabled (Tier 3) — using Clang with -fsanitize-coverage=trace-pc-guard");
            } else {
                println!("cargo:warning=SIM_INSTRUMENT_EDGES=1 requires Clang but 'clang' not found — edge instrumentation disabled");
            }
        }
    }

    if cfg!(target_env = "msvc") {
        build.flag_if_supported("/W3");
    }

    // ── Compile ───────────────────────────────────────────────────
    build.compile("embedded_c_payload");
}
