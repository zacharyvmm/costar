//! Build script for sim-zephyr-port.
//!
//! Two modes:
//! 1. Standalone test (default): compiles zephyr_arch.c + zephyr_glue.c +
//!    nsi_shim.c + standalone_test.c via cc crate. No Zephyr SDK needed.
//!
//! 2. Real Zephyr kernel (when ZEPHYR_BASE is set): compiles the actual
//!    Zephyr kernel sources + our sim_arch.c arch layer + pre-generated
//!    config headers. This replaces `west build` with direct cc crate
//!    compilation — cross-platform, no CMake/Kconfig/DTSC needed.

use std::path::{Path, PathBuf};

fn main() {
    let zephyr_base = std::env::var("ZEPHYR_BASE").unwrap_or_default();

    if !zephyr_base.is_empty() && Path::new(&zephyr_base).join("kernel/init.c").exists() {
        build_real_zephyr(&zephyr_base);
    } else {
        build_standalone();
    }
}

/// Compile the standalone test app (no Zephyr SDK needed).
fn build_standalone() {
    println!("cargo:rerun-if-changed=c/zephyr_arch.c");
    println!("cargo:rerun-if-changed=c/zephyr_arch.h");
    println!("cargo:rerun-if-changed=c/sim_zephyr_abi.h");
    println!("cargo:rerun-if-changed=c/zephyr_glue.c");
    println!("cargo:rerun-if-changed=../sim-ffi/include/sim_abi.h");
    println!("cargo:rerun-if-changed=../../c_firmware/zephyr_app/standalone_test.c");

    let mut build = cc::Build::new();

    build
        .file("c/zephyr_arch.c")
        .file("c/zephyr_glue.c")
        .file("../../c_firmware/zephyr_app/standalone_test.c");

    build.include("c").include("../sim-ffi/include");
    build.define("SIMULATION_HOST_MODE", Some("1"));

    platform_flags(&mut build);
    build.compile("embedded_zephyr_payload");

    println!("cargo:warning=Standalone Zephyr test compiled (no real kernel)");
}

/// Compile the real Zephyr kernel sources with our sim arch layer.
fn build_real_zephyr(zephyr_base: &str) {
    let base = PathBuf::from(zephyr_base);

    // ── Verify required paths ───────────────────────────────────────
    let kernel_dir = base.join("kernel");
    let include_dir = base.join("include");
    let arch_posix_include = base.join("arch/posix/include");
    let kernel_include = base.join("kernel/include");
    let soc_dir = base.join("soc/native/inf_clock");
    let boards_dir = base.join("boards/native/native_sim");
    let nsi_common = base.join("scripts/native_simulator/common/src/include");
    let nsi_native = base.join("scripts/native_simulator/native/src/include");

    if !kernel_dir.join("init.c").exists() {
        println!(
            "cargo:warning=ZEPHYR_BASE={} does not look like a Zephyr tree (missing kernel/init.c)",
            zephyr_base
        );
        build_standalone();
        return;
    }

    println!(
        "cargo:warning=Building real Zephyr kernel from {}",
        zephyr_base
    );

    let mut build = cc::Build::new();

    // ── Our arch layer ──────────────────────────────────────────────
    build.file("c/sim_arch.c").file("c/nsi_shim.c");
    build.file("c/linker_stubs.S");

    // ── Generated config (checksum + version) ───────────────────────
    build.file("config/configs.c");

    // ── Zephyr kernel core ──────────────────────────────────────────
    let kernel_files = [
        "init.c",
        "sched.c",
        "thread.c",
        "timeout.c",
        "timer.c",
        "queue.c",
        "idle.c",
        "device.c",
        "errno.c",
        "version.c",
        "banner.c",
        "work.c",
        "system_work_q.c",
        "init_static.c",
        "timeslicing.c",
    ];
    for f in &kernel_files {
        let path = kernel_dir.join(f);
        if path.exists() {
            build.file(path);
        } else {
            println!("cargo:warning=Kernel file not found: {}", path.display());
        }
    }

    // ── Zephyr arch/posix core (subset we DON'T replace) ────────────
    // swap.c, irq.c, thread.c, posix_core_nsi.c are REPLACED by sim_arch.c.
    // offsets.c, fatal.c, and cpuhalt.c are still needed.
    build.file(base.join("arch/posix/core/offsets/offsets.c"));
    build.file(base.join("arch/posix/core/fatal.c"));
    build.file(base.join("arch/posix/core/cpuhalt.c"));

    // ── Zephyr lib/ ─────────────────────────────────────────────────
    for f in &[
        "os/thread_entry.c",
        "os/printk.c",
        "os/cbprintf.c",
        "os/cbprintf_complete.c",
        "os/cbprintf_packaged.c",
        "os/assert.c",
        "os/sem.c",
        "heap/heap.c",
        "utils/dec.c",
        "utils/hex.c",
        "utils/rb.c",
        "utils/timeutil.c",
        "utils/bitarray.c",
        "utils/ring_buffer.c",
    ] {
        let path = base.join("lib").join(f);
        if path.exists() {
            build.file(path);
        }
    }

    // ── Zephyr soc/ — we need posix_boot_cpu from soc.c ─────────────
    build.file(soc_dir.join("soc.c"));
    build.file(soc_dir.join("native_tasks.c"));

    // ── Zephyr boards/ ──────────────────────────────────────────────
    for f in &[
        "cmdline.c",
        "cpu_wait.c",
        "nsi_if.c",
        "irq_handler.c",
        "misc.c",
        "posix_arch_if.c",
    ] {
        build.file(boards_dir.join(f));
    }

    // ── Zephyr drivers/ ─────────────────────────────────────────────
    build.file(base.join("drivers/console/posix_arch_console.c"));
    build.file(base.join("drivers/timer/sys_clock_init.c"));
    build.file(base.join("drivers/timer/native_posix_timer.c"));

    // ── Zephyr subsys/ ──────────────────────────────────────────────
    let tracing = base.join("subsys/tracing/tracing_none.c");
    if tracing.exists() {
        build.file(tracing);
    }

    // ── Includes and forced config ──────────────────────────────────
    // Force-include autoconf.h so all Zephyr headers see CONFIG_* defines.
    build.flag("-include").flag("zephyr/autoconf.h");

    // ── Include paths ───────────────────────────────────────────────
    // Order matters: our config/ first (overrides generated headers),
    // then Zephyr's standard include hierarchy.
    build
        .include("config") // our pre-generated configs
        .include(arch_posix_include) // posix_core.h, etc.
        .include(kernel_include) // kswap.h, kernel_internal.h
        .include(include_dir) // public Zephyr API
        .include(soc_dir)
        .include(boards_dir)
        .include(nsi_common)
        .include(nsi_native)
        .include(&base); // root for absolute includes

    // ── Essential defines (needed BEFORE autoconf.h for header guards) ──
    build
        .define("CONFIG_NATIVE_LIBRARY", "1")
        .define("CONFIG_NATIVE_APPLICATION", "1")
        .define("CONFIG_ARCH_POSIX", "1");

    // macOS: Zephyr uses ELF-specific section attributes (__noinit,
    // __in_section_unique) that fail on Mach-O. Neutralize them so
    // thread stacks land in regular data sections — safe since the
    // simulator manages memory virtually.
    if cfg!(target_os = "macos") {
        build.flag("-D__noinit=");
        build.flag("-D__in_section_unique(seg)=");
    }

    platform_flags(&mut build);

    // ── Link outputs ────────────────────────────────────────────────
    // Tell Cargo to re-link if the Zephyr source changes (best-effort).
    println!("cargo:rerun-if-changed=c/sim_arch.c");
    println!("cargo:rerun-if-changed=c/nsi_shim.c");
    println!("cargo:rerun-if-changed=config/");

    build.compile("embedded_zephyr_payload");

    println!("cargo:warning=Real Zephyr kernel compiled successfully via cc crate");
    println!("cargo:rustc-cfg=zephyr_cc_kernel_port");
}

fn platform_flags(build: &mut cc::Build) {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        build.flag_if_supported("-Wall");
        build.flag_if_supported("-Wextra");
        build.flag_if_supported("-Wno-unused-parameter");
        build.flag_if_supported("-Wno-unused-variable");
        build.flag_if_supported("-Wno-missing-field-initializers");
        /* Zephyr kernel has some known type mismatches in printf code. */
        build.flag_if_supported("-Wno-incompatible-pointer-types");
        /* Don't let Zephyr kernel warnings break the build. */
        build.flag_if_supported("-Wno-error");
    }
}
