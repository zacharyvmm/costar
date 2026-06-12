//! Build script for sim-zephyr-port.
//!
//! Compiles the simulator arch port and a minimal standalone Zephyr-style
//! test app through the `cc` crate.
//!
//! The real Zephyr kernel is built externally via `west build -b sim` and
//! linked separately.  The standalone test app serves as a CI-friendly
//! verification that the thread→fiber mapping works without the full
//! Zephyr SDK.

fn main() {
    // Re-run if any C source or header changes.
    println!("cargo:rerun-if-changed=c/zephyr_arch.c");
    println!("cargo:rerun-if-changed=c/zephyr_arch.h");
    println!("cargo:rerun-if-changed=c/sim_zephyr_abi.h");
    println!("cargo:rerun-if-changed=../sim-ffi/include/sim_abi.h");
    println!("cargo:rerun-if-changed=../../c_firmware/zephyr_app/standalone_test.c");

    let mut build = cc::Build::new();

    // ── Arch port layer ───────────────────────────────────────────
    build.file("c/zephyr_arch.c");

    // ── Standalone test app (no Zephyr SDK needed) ────────────────
    build.file("../../c_firmware/zephyr_app/standalone_test.c");

    // ── Include paths ─────────────────────────────────────────────
    build
        .include("c") // zephyr_arch.h
        .include("../sim-ffi/include"); // sim_abi.h

    // ── Defines ───────────────────────────────────────────────────
    build
        .define("SIMULATION_HOST_MODE", Some("1"))
        .define("ZEPHYR_PORT_SIM", Some("1"))
        .define("ZEPHYR_STANDALONE_TEST", Some("1"));

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
    build.compile("zephyr_standalone_payload");
}
