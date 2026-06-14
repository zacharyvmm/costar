//! Build script for sim-zephyr-port.
//!
//! Two modes:
//! 1. Standalone test (default): compiles zephyr_arch.c + zephyr_glue.c +
//!    nsi_shim.c + standalone_test.c via cc crate. No Zephyr SDK needed.
//!
//! 2. Real Zephyr kernel (when ZEPHYR_BASE is set): compiles the actual
//!    Zephyr kernel sources + our sim_arch.c arch layer + pre-generated
//!    config headers. This replaces `west build` with direct cc crate
//!    compilation on Unix-like hosts — no CMake/Kconfig/DTSC needed.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=ZEPHYR_BASE");

    let zephyr_base = std::env::var("ZEPHYR_BASE").unwrap_or_default();

    if !zephyr_base.is_empty()
        && Path::new(&zephyr_base).join("kernel/init.c").exists()
        && real_zephyr_cc_supported()
    {
        build_real_zephyr(&zephyr_base);
    } else {
        if !zephyr_base.is_empty() && cfg!(target_os = "windows") {
            println!(
                "cargo:warning=ZEPHYR_BASE is set, but Zephyr's POSIX native kernel assumes GNU/LP64 C ABI; using standalone Zephyr payload on Windows"
            );
        }
        build_standalone();
    }
}

fn real_zephyr_cc_supported() -> bool {
    !cfg!(target_os = "windows")
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
    let use_host_stubs = cfg!(any(target_os = "macos", target_os = "windows"));

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
    configure_real_zephyr_compiler(&mut build);

    // ── Our arch layer ──────────────────────────────────────────────
    build.file("c/sim_arch.c").file("c/nsi_shim.c");
    build.file("c/linker_stubs.S");

    // ── Generated config (checksum + version) ───────────────────────
    build.file("config/configs.c");

    // ── Zephyr application main() ──────────────────────────────────
    let zephyr_app = std::env::var("ZEPHYR_APP").unwrap_or_default();
    let app_file = if zephyr_app == "broader_api" {
        println!("cargo:warning=Building broader-api Zephyr app (k_sem, k_mutex, k_msgq, k_timer, k_work)");
        "config/app_broader_api.c"
    } else if zephyr_app == "ztest" {
        println!("cargo:warning=Building ztest Zephyr app");
        "config/app_ztest.c"
    } else {
        "config/app_main.c"
    };
    build.file(app_file);
    println!("cargo:rerun-if-env-changed=ZEPHYR_APP");

    // ── Zephyr kernel core ──────────────────────────────────────────
    // init.c is compiled separately with -Dmain=zephyr_app_main to
    // avoid a symbol collision between Zephyr's app main() and Rust's
    // main() (the ELF entry point).  See config/app_main.c.
    let kernel_files = [
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
        "init_static.c",
        "timeslicing.c",
        "sem.c",
        "mutex.c",
        "msg_q.c",
        "condvar.c",
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
    // offsets.c emits ELF-only absolute-symbol inline assembly; macOS and
    // Windows use the generated config/zephyr/offsets.h checked into this
    // crate instead.
    if !use_host_stubs {
        build.file(base.join("arch/posix/core/offsets/offsets.c"));
    }
    build.file(base.join("arch/posix/core/fatal.c"));
    build.file(base.join("arch/posix/core/cpuhalt.c"));

    // ── Zephyr lib/ ─────────────────────────────────────────────────
    for f in &[
        "os/thread_entry.c",
        "os/printk.c",
        "os/cbprintf.c",
        "os/cbprintf_complete.c",
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
    // cbprintf_packaged.c reconstructs va_list using SysV x86_64 ABI details.
    // That is not valid for Windows' MSVC ABI.
    if !use_host_stubs {
        build.file(base.join("lib/os/cbprintf_packaged.c"));
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
        "posix_arch_if.c",
    ] {
        build.file(boards_dir.join(f));
    }
    if !use_host_stubs {
        build.file(boards_dir.join("misc.c"));
    }

    // ── Zephyr drivers/ ─────────────────────────────────────────────
    build.file(base.join("drivers/timer/sys_clock_init.c"));
    if use_host_stubs {
        build.file("c/zephyr_host_stubs.c");
    } else {
        build.file(base.join("drivers/console/posix_arch_console.c"));
    }

    // ── Zephyr ztest subsystem ────────────────────────────────────
    // ztest.c defines its own main() which conflicts with Rust's main.
    // Compile it separately with -Dmain=zephyr_ztest_main.
    // ztest_glue.c provides non-inline wrappers for static inline
    // functions from ztest_test.h (needed at -O0 where GCC doesn't
    // emit the static symbols).
    if zephyr_app == "ztest" {
        let ztest_dir = base.join("subsys/testsuite/ztest");
        let ztest_include = base.join("subsys/testsuite/include");
        build.file("c/ztest_glue.c");
        build.file(ztest_dir.join("src/ztest_defaults.c"));
        build.include(&ztest_dir.join("include"));
        build.include(&ztest_include);

        {
            let mut zbuild = cc::Build::new();
            configure_real_zephyr_compiler(&mut zbuild);
            zbuild.file(ztest_dir.join("src/ztest.c"));
            zbuild.define("main", "zephyr_ztest_main");
            zbuild.flag("-include").flag("zephyr/autoconf.h");
            zbuild
                .include("config")
                .include("../sim-ffi/include")
                .include(&arch_posix_include)
                .include(&kernel_include)
                .include(&include_dir)
                .include(&soc_dir)
                .include(&boards_dir)
                .include(&nsi_common)
                .include(&nsi_native)
                .include(&base);
            zbuild.include(&ztest_dir.join("include"));
            zbuild.include(&ztest_include);
            zbuild
                .define("CONFIG_NATIVE_LIBRARY", "1")
                .define("CONFIG_NATIVE_APPLICATION", "1")
                .define("CONFIG_ARCH_POSIX", "1");
            if use_host_stubs {
                zbuild.flag("-D__noinit=");
                zbuild.flag("-D__in_section_unique(seg)=");
            }
            platform_flags(&mut zbuild);
            zbuild.compile("zephyr_ztest_renamed");
        }
    }

    // ── Zephyr subsys/ ──────────────────────────────────────────────
    let tracing = base.join("subsys/tracing/tracing_none.c");
    if tracing.exists() {
        build.file(tracing);
    }

    // ── Compile init.c with main→zephyr_app_main rename ──────────────
    // Zephyr's init.c (bg_thread_main) calls main(), which would collide
    // with Rust's main() ELF entry point.  We compile init.c separately
    // with a preprocessor rename so bg_thread_main calls zephyr_app_main
    // instead.  The app's entry is defined in config/app_main.c.
    {
        let mut init_build = cc::Build::new();
        configure_real_zephyr_compiler(&mut init_build);
        init_build.file(kernel_dir.join("init.c"));
        init_build.define("main", "zephyr_app_main");
        // Same includes and defines as the main build.
        init_build.flag("-include").flag("zephyr/autoconf.h");
        init_build
            .include("config")
            .include("../sim-ffi/include")
            .include(&arch_posix_include)
            .include(&kernel_include)
            .include(&include_dir)
            .include(&soc_dir)
            .include(&boards_dir)
            .include(&nsi_common)
            .include(&nsi_native)
            .include(&base);
        init_build
            .define("CONFIG_NATIVE_LIBRARY", "1")
            .define("CONFIG_NATIVE_APPLICATION", "1")
            .define("CONFIG_ARCH_POSIX", "1");
        if use_host_stubs {
            init_build.flag("-D__noinit=");
            init_build.flag("-D__in_section_unique(seg)=");
        }
        platform_flags(&mut init_build);
        // Compile into its own library.
        init_build.compile("zephyr_init_renamed");
    }

    // ── Includes and forced config ──────────────────────────────────
    // Force-include autoconf.h so all Zephyr headers see CONFIG_* defines.
    build.flag("-include").flag("zephyr/autoconf.h");

    // ── Include paths ───────────────────────────────────────────────
    // Order matters: our config/ first (overrides generated headers),
    // then Zephyr's standard include hierarchy.
    build
        .include("config") // our pre-generated configs
        .include("../sim-ffi/include") // sim_abi.h for trace calls
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

    // macOS/Windows: Zephyr uses ELF-specific section attributes.
    // -D flags can override __noinit/__in_section_unique (command-line
    // definition defeats the header's macro chain), but Z_INIT_ENTRY_SECTION
    // is re-#defined in init.h after our -D, so that path still fails on
    // Darwin. COFF can compile those sections, but the checked-in linker
    // marker stubs do not preserve Zephyr's init ordering there either, so
    // keep Windows on the same compile-only path.
    if use_host_stubs {
        build.flag("-D__noinit=");
        build.flag("-D__in_section_unique(seg)=");
    } else {
        build.file(kernel_dir.join("system_work_q.c"));
    }

    platform_flags(&mut build);

    // ── Link outputs ────────────────────────────────────────────────
    // Tell Cargo to re-link if the Zephyr source changes (best-effort).
    println!("cargo:rerun-if-changed=c/sim_arch.c");
    println!("cargo:rerun-if-changed=c/nsi_shim.c");
    println!("cargo:rerun-if-changed=c/linker_stubs.S");
    println!("cargo:rerun-if-changed=c/zephyr_host_stubs.c");
    println!("cargo:rerun-if-changed=config/");
    println!("cargo:rerun-if-changed=config/app_main.c");
    println!("cargo:rerun-if-changed=config/app_broader_api.c");
    println!("cargo:rerun-if-changed=config/app_ztest.c");

    build.compile("embedded_zephyr_payload");

    println!("cargo:warning=Real Zephyr kernel compiled successfully via cc crate");
    println!("cargo:rustc-cfg=zephyr_cc_kernel_port");
}

fn configure_real_zephyr_compiler(build: &mut cc::Build) {
    if !cfg!(target_os = "windows") {
        return;
    }

    if !cc_env_override_is_set() {
        build.compiler("clang");
        println!(
            "cargo:warning=Using clang for Zephyr C sources on Windows; Zephyr v4.1 is not cl.exe-compatible"
        );
    }
}

fn cc_env_override_is_set() -> bool {
    let target = std::env::var("TARGET").unwrap_or_default();
    let target_underscored = target.replace('-', "_");

    [
        format!("CC_{}", target),
        format!("CC_{}", target_underscored),
        "CC".to_string(),
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some())
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

    if cfg!(target_os = "windows") {
        build.flag_if_supported("-Wall");
        build.flag_if_supported("-Wextra");
        build.flag_if_supported("-Wno-unused-parameter");
        build.flag_if_supported("-Wno-unused-variable");
        build.flag_if_supported("-Wno-missing-field-initializers");
        build.flag_if_supported("-Wno-macro-redefined");
        build.flag_if_supported("-Wno-ignored-attributes");
        build.flag_if_supported("-Wno-error");
    }
}
