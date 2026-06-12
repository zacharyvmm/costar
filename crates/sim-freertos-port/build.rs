//! Build script for sim-freertos-port.
//!
//! Compiles the C port layer and the guest firmware through the `cc`
//! crate.  The compiled object is linked into the final sim-runner binary.

fn main() {
    // Re-run if any C source or header changes.
    println!("cargo:rerun-if-changed=c/port.c");
    println!("cargo:rerun-if-changed=c/sim_hooks.c");
    println!("cargo:rerun-if-changed=c/sim_kernel_bridge.c");
    println!("cargo:rerun-if-changed=c/portmacro.h");
    println!("cargo:rerun-if-changed=../sim-ffi/include/sim_abi.h");

    // Firmware sources
    println!("cargo:rerun-if-changed=../../c_firmware/app/main.c");
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
        .file("c/sim_kernel_bridge.c");

    // ── Guest firmware (FreeRTOS kernel + app) ────────────────────
    build
        .file("../../c_firmware/app/main.c")
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
    }

    if cfg!(target_env = "msvc") {
        build.flag_if_supported("/W3");
    }

    // ── Compile ───────────────────────────────────────────────────
    build.compile("embedded_c_payload");
}
