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
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_broader_api.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/tasks.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/queue.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/list.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/timers.c");
    println!("cargo:rerun-if-changed=../../c_firmware/freertos/event_groups.c");

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
        .file("c/sim_kernel_bridge.c");

    // sim_coverage.c defines __sanitizer_cov_trace_pc_guard callbacks
    // for Tier 3 edge instrumentation.  This is Clang-only and uses
    // __thread which MSVC doesn't support in C mode — skip on MSVC.
    if cfg!(not(target_env = "msvc")) {
        build.file("c/sim_coverage.c");
    }

    // ── Guest firmware (FreeRTOS kernel + app) ────────────────────
    build
        .file("../../c_firmware/app/main.c")
        .file("../../c_firmware/app/tight_loop_demo.c")
        .file("../../c_firmware/app/main_broader_api.c")
        .file("../../c_firmware/freertos/tasks.c")
        .file("../../c_firmware/freertos/queue.c")
        .file("../../c_firmware/freertos/list.c")
        .file("../../c_firmware/freertos/timers.c")
        .file("../../c_firmware/freertos/event_groups.c");

    // main_interactive.c uses POSIX socketpair / fcntl — skip on Windows.
    if cfg!(not(windows)) {
        build.file("../../c_firmware/app/main_interactive.c");
    }

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
        if std::env::var("SIM_INSTRUMENT_FUNCTIONS").as_deref() == Ok("1") {
            build.flag_if_supported("-finstrument-functions");
        }

        // Tier 3 edge-level instrumentation — opt-in via env var.
        if std::env::var("SIM_INSTRUMENT_EDGES").as_deref() == Ok("1") {
            let has_clang = std::process::Command::new("clang")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok();

            if has_clang {
                build.compiler("clang");
                build.flag_if_supported("-fsanitize-coverage=trace-pc-guard");
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
