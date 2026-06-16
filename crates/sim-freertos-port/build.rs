//! Build script for sim-freertos-port.
//!
//! Compiles the C port layer and the guest firmware through the `cc`
//! crate.  The compiled object is linked into the final sim-runner binary.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    // Re-run if any C source, header, or configuration environment variable changes.
    println!("cargo:rerun-if-env-changed=SIM_INSTRUMENT_FUNCTIONS");
    println!("cargo:rerun-if-env-changed=SIM_INSTRUMENT_EDGES");
    println!("cargo:rerun-if-changed=c/port.c");
    println!("cargo:rerun-if-changed=c/sim_hooks.c");
    println!("cargo:rerun-if-changed=c/sim_kernel_bridge.c");
    println!("cargo:rerun-if-changed=c/sim_coverage.c");
    println!("cargo:rerun-if-changed=c/portmacro.h");
    println!("cargo:rerun-if-changed=c/FreeRTOSConfig.h");
    println!("cargo:rerun-if-changed=../sim-ffi/include/sim_abi.h");

    // Firmware sources
    println!("cargo:rerun-if-changed=../../c_firmware/app/main.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_interactive.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/tight_loop_demo.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_broader_api.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_i2c_spi.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_can.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_devices.c");
    println!("cargo:rerun-if-changed=FreeRTOS-Kernel/tasks.c");
    println!("cargo:rerun-if-changed=FreeRTOS-Kernel/queue.c");
    println!("cargo:rerun-if-changed=FreeRTOS-Kernel/list.c");
    println!("cargo:rerun-if-changed=FreeRTOS-Kernel/timers.c");
    println!("cargo:rerun-if-changed=FreeRTOS-Kernel/event_groups.c");

    // Firmware headers
    for header in &[
        "FreeRTOS.h",
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
        println!("cargo:rerun-if-changed=FreeRTOS-Kernel/include/{}", header);
    }

    // Path to OUT_DIR where we'll place the patched tasks.c
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let patched_tasks_c = out_dir.join("tasks.c");

    // Patch tasks.c dynamically
    patch_tasks_c(Path::new("FreeRTOS-Kernel/tasks.c"), &patched_tasks_c)
        .expect("Failed to patch FreeRTOS tasks.c");

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
        .file("../../c_firmware/app/main_i2c_spi.c")
        .file("../../c_firmware/app/main_can.c")
        .file("../../c_firmware/app/main_devices.c")
        .file(&patched_tasks_c)
        .file("FreeRTOS-Kernel/queue.c")
        .file("FreeRTOS-Kernel/list.c")
        .file("FreeRTOS-Kernel/timers.c")
        .file("FreeRTOS-Kernel/event_groups.c");

    // main_interactive.c uses POSIX socketpair / fcntl — skip on Windows.
    if cfg!(not(windows)) {
        build.file("../../c_firmware/app/main_interactive.c");
    }

    // ── Include paths ─────────────────────────────────────────────
    build
        .include("c") // portmacro.h, FreeRTOSConfig.h
        .include("../sim-ffi/include") // sim_abi.h
        .include("FreeRTOS-Kernel/include"); // FreeRTOS.h, etc.

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

fn patch_tasks_c(src_path: &Path, dest_path: &Path) -> std::io::Result<()> {
    let mut content = fs::read_to_string(src_path)?;

    // Standardize line endings to LF for robustness in patching on Windows/Mac/Linux
    content = content.replace("\r\n", "\n");

    // 1. Add #include "sim_abi.h"
    let stack_macros_include = "#include \"stack_macros.h\"";
    if let Some(pos) = content.find(stack_macros_include) {
        let insert_pos = pos + stack_macros_include.len();
        content.insert_str(insert_pos, "\n#include \"sim_abi.h\"");
    } else {
        panic!("Failed to find #include \"stack_macros.h\" in tasks.c");
    }

    // 2. Add simHandle field inside struct tskTaskControlBlock (TCB_t)
    let pc_task_name_field = "char pcTaskName[ configMAX_TASK_NAME_LEN ]; /**< Descriptive name given to the task when created.  Facilitates debugging only. */";
    if let Some(pos) = content.find(pc_task_name_field) {
        let insert_pos = pos + pc_task_name_field.len();
        content.insert_str(insert_pos, "\n    sim_task_handle_t simHandle;                 /**< Rust fiber handle for the simulator bridge. */");
    } else {
        panic!("Failed to find pcTaskName field in TCB struct in tasks.c");
    }

    // 3. Add sim_task_delay_until in vTaskDelay
    let vtask_delay_fn = "void vTaskDelay( const TickType_t xTicksToDelay )";
    if let Some(fn_pos) = content.find(vtask_delay_fn) {
        let force_reschedule_comment = "        /* Force a reschedule if xTaskResumeAll has not already done so, we may\n         * have put ourselves to sleep. */";
        if let Some(pos) = content[fn_pos..].find(force_reschedule_comment) {
            let insert_pos = fn_pos + pos + force_reschedule_comment.len();
            content.insert_str(insert_pos, "\n        /* Simulator bridge: tell Rust fiber when to wake. */\n        if( xTicksToDelay > ( TickType_t ) 0U )\n            sim_task_delay_until( (uint64_t) ( xTickCount + xTicksToDelay ) );\n");
        } else {
            panic!("Failed to find force reschedule comment in vTaskDelay in tasks.c");
        }
    } else {
        panic!("Failed to find void vTaskDelay in tasks.c");
    }

    // 4. Append simulator bridge functions to the end of tasks.c
    let bridge_functions = r#"
/*-----------------------------------------------------------*/

void sim_bridge_add_pending_tcb( void *pvTCB );

/* PendingTCB is defined in sim_kernel_bridge.c; we duplicate the
 * typedef here because C lacks a shared header for this internal type. */
typedef struct { struct tskTaskControlBlock *tcb; } PendingTCB;

void sim_port_task_created( void *pvTCB ) {
    /* Defer fiber creation — creating corosensei coroutines deep
     * inside FreeRTOS's call stack causes segfaults on resume.
     * Instead, record the TCB and create the fiber lazily when
     * sim_bridge_create_pending_fibers() is called from the
     * Rust scheduler at the start of the drain loop. */
    sim_bridge_add_pending_tcb( pvTCB );
}

/* Create Rust fibers for all TCBs that were registered via
 * sim_port_task_created.  Called from the Rust scheduler at the
 * start of sim_start_scheduler().  This function lives here
 * (in tasks.c) because it needs access to the TCB struct fields
 * which are private to this compilation unit. */
uint32_t sim_bridge_create_pending_fibers( void )
{
    extern PendingTCB pending_tcbs[];
    extern int pending_count;

    uint32_t created = 0;

    for( int i = 0; i < pending_count; i++ )
    {
        TCB_t *tcb = pending_tcbs[i].tcb;

        /* The entry point and parameter are stored on the task's
         * stack by pxPortInitialiseStack.  The frame layout is:
         *   sp[-0] = magic    (0xDEADBEEF)
         *   sp[-1] = entry    (task function pointer)
         *   sp[-2] = param    (task argument)
         *   sp[-3] = simHandle
         * pxPortInitialiseStack returns &sp[-PORT_STACK_SLOTS],
         * so the metadata slots are at positive offsets from
         * pxTopOfStack. */
        volatile StackType_t *sp = tcb->pxTopOfStack;

        StackType_t magic      = sp[3];
        StackType_t entry_raw  = sp[2];
        StackType_t param_raw  = sp[1];

        (void)magic; /* 0xDEADBEEF */

        sim_task_entry_fn entry = (sim_task_entry_fn)(uintptr_t)entry_raw;
        void *param = (void *)(uintptr_t)param_raw;

        if( entry == NULL )
        {
            /* Idle task — skip fiber creation.  FreeRTOS will never
             * try to schedule it because our scheduler loop only
             * resumes tasks that have Rust fibers. */
            continue;
        }

        const char *name = tcb->pcTaskName;
        uint32_t priority = tcb->uxPriority;
        uint32_t stack_words = configMINIMAL_STACK_SIZE;

        sim_task_handle_t handle = sim_create_task(
            name, entry, param, stack_words, priority
        );

        /* Store the handle in the TCB and bridge table. */
        sp[0] = (StackType_t)handle;  /* simHandle slot */
        tcb->simHandle = handle;
        sim_bridge_register( handle, (void *)tcb );

        created++;
    }

    pending_count = 0;
    return created;
}
"#;
    content.push_str(bridge_functions);

    fs::write(dest_path, content)?;
    Ok(())
}
