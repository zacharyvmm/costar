//! Build script for sim-zephyr-port.
//!
//! Compiles the C arch port layer and the standalone Zephyr test app
//! through the `cc` crate.  The compiled object is linked into the
//! final sim-runner binary as a separate static library.

fn main() {
    // Re-run if any C source or header changes.
    println!("cargo:rerun-if-changed=c/zephyr_arch.c");
    println!("cargo:rerun-if-changed=c/zephyr_arch.h");
    println!("cargo:rerun-if-changed=c/sim_zephyr_abi.h");
    println!("cargo:rerun-if-changed=c/zephyr_glue.c");
    println!("cargo:rerun-if-changed=../sim-ffi/include/sim_abi.h");
    println!("cargo:rerun-if-changed=../../c_firmware/zephyr_app/standalone_test.c");

    let mut build = cc::Build::new();

    // ── Arch port ──────────────────────────────────────────────────
    build
        .file("c/zephyr_arch.c")
        .file("c/zephyr_glue.c")
        .file("c/nsi_shim.c")
        .file("../../c_firmware/zephyr_app/standalone_test.c");

    // ── Include paths ─────────────────────────────────────────────
    build
        .include("c") // zephyr_arch.h, sim_zephyr_abi.h
        .include("../sim-ffi/include"); // sim_abi.h

    // ── Defines ───────────────────────────────────────────────────
    build.define("SIMULATION_HOST_MODE", Some("1"));

    // ── Platform-specific flags ───────────────────────────────────
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        build.flag_if_supported("-Wall");
        build.flag_if_supported("-Wextra");
        build.flag_if_supported("-Wno-unused-parameter");
        build.flag_if_supported("-Wno-missing-field-initializers");
    }

    // ── Compile ───────────────────────────────────────────────────
    build.compile("embedded_zephyr_payload");
}
