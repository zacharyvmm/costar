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
    println!("cargo:rerun-if-env-changed=SIM_TCP");
    println!("cargo:rerun-if-changed=c/port.c");
    println!("cargo:rerun-if-changed=c/sim_hooks.c");
    println!("cargo:rerun-if-changed=c/sim_kernel_bridge.c");
    println!("cargo:rerun-if-changed=c/sim_coverage.c");
    println!("cargo:rerun-if-changed=c/sim_block.c");
    println!("cargo:rerun-if-changed=c/sim_eth.c");
    println!("cargo:rerun-if-changed=c/sim_net_if.c");
    println!("cargo:rerun-if-changed=c/FreeRTOSIPConfig.h");
    println!("cargo:rerun-if-changed=c/portmacro.h");
    println!("cargo:rerun-if-changed=c/sim_port.h");
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
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_entropy.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_task_delete.c");
    println!("cargo:rerun-if-changed=../../c_firmware/tests/sched_regressions.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_net.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_block.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_bt.c");
    println!("cargo:rerun-if-changed=../../c_firmware/app/main_display.c");
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
    let patched_timers_c = out_dir.join("timers.c");

    // Patch tasks.c dynamically
    patch_tasks_c(Path::new("FreeRTOS-Kernel/tasks.c"), &patched_tasks_c)
        .expect("Failed to patch FreeRTOS tasks.c");
    patch_timers_c(Path::new("FreeRTOS-Kernel/timers.c"), &patched_timers_c)
        .expect("Failed to patch FreeRTOS timers.c");

    let mut build = cc::Build::new();

    // ── Port layer ────────────────────────────────────────────────
    build
        .file("c/port.c")
        .file("c/sim_hooks.c")
        .file("c/sim_kernel_bridge.c")
        .file("c/sim_block.c")
        .file("c/sim_eth.c");

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
        .file("../../c_firmware/app/main_entropy.c")
        .file("../../c_firmware/app/main_task_delete.c")
        .file("../../c_firmware/tests/sched_regressions.c")
        .file("../../c_firmware/app/main_net.c")
        .file("../../c_firmware/app/main_block.c")
        .file("../../c_firmware/app/main_bt.c")
        .file("../../c_firmware/app/main_display.c")
        .file(&patched_tasks_c)
        .file("FreeRTOS-Kernel/queue.c")
        .file("FreeRTOS-Kernel/list.c")
        .file(&patched_timers_c)
        .file("FreeRTOS-Kernel/event_groups.c");

    // main_interactive.c uses TCP loopback (cross-platform, no POSIX socketpair).
    build.file("../../c_firmware/app/main_interactive.c");

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

    // ── FreeRTOS+TCP integration (opt-in via SIM_TCP=1) ───────────
    let build_tcp = std::env::var("SIM_TCP").as_deref() == Ok("1");
    if build_tcp {
        println!("cargo:warning=Compiling FreeRTOS+TCP stack");
        build_tcp_stack(&mut build);

        /* TCP echo demo — compiled only with SIM_TCP=1. */
        println!("cargo:rerun-if-changed=../../c_firmware/app/main_tcp_echo.c");
        build.file("../../c_firmware/app/main_tcp_echo.c");

        /* Emit cfg so sim-runner can gate the extern + mode. */
        println!("cargo:TCP_ENABLED=1");
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
        content.insert_str(
            insert_pos,
            "\n#include \"sim_abi.h\"\n#include \"sim_port.h\"\n#include <stdlib.h>",
        );
    } else {
        panic!("Failed to find #include \"stack_macros.h\" in tasks.c");
    }

    // 4. Append simulator bridge functions to the end of tasks.c
    let bridge_functions = r#"
/*-----------------------------------------------------------*/

/*
 * Snapshot every mutable tasks.c singleton.  FreeRTOS is normally one kernel
 * per firmware image; the native simulator instead switches this state at
 * every active Simulator boundary so interleaved Worlds retain independent
 * ready/delayed lists and current TCBs.
 */
typedef struct SimFreeRtosTaskState
{
    TCB_t *pxCurrentTCB;
    List_t pxReadyTasksLists[ configMAX_PRIORITIES ];
    List_t xDelayedTaskList1;
    List_t xDelayedTaskList2;
    List_t *pxDelayedTaskList;
    List_t *pxOverflowDelayedTaskList;
    List_t xPendingReadyList;
#if ( INCLUDE_vTaskDelete == 1 )
    List_t xTasksWaitingTermination;
    UBaseType_t uxDeletedTasksWaitingCleanUp;
#endif
#if ( INCLUDE_vTaskSuspend == 1 )
    List_t xSuspendedTaskList;
#endif
    UBaseType_t uxCurrentNumberOfTasks;
    TickType_t xTickCount;
    UBaseType_t uxTopReadyPriority;
    BaseType_t xSchedulerRunning;
    TickType_t xPendedTicks;
    BaseType_t xYieldPendings[ configNUMBER_OF_CORES ];
    BaseType_t xNumOfOverflows;
    UBaseType_t uxTaskNumber;
    TickType_t xNextTaskUnblockTime;
    TaskHandle_t xIdleTaskHandles[ configNUMBER_OF_CORES ];
    UBaseType_t uxSchedulerSuspended;
} SimFreeRtosTaskState;

void *sim_freertos_task_state_create( void )
{
    SimFreeRtosTaskState *state =
        ( SimFreeRtosTaskState * ) calloc( 1, sizeof( SimFreeRtosTaskState ) );
    if( state != NULL )
    {
        /* Match FreeRTOS power-on: no unblock until the scheduler starts.
         * A zeroed xNextTaskUnblockTime makes the first xTaskIncrementTick
         * walk a NULL delayed list and SIGSEGV. */
        state->xNextTaskUnblockTime = portMAX_DELAY;
    }
    return state;
}

void sim_freertos_task_state_destroy( void *opaque )
{
    free( opaque );
}

void sim_freertos_task_state_save( void *opaque )
{
    SimFreeRtosTaskState *state = ( SimFreeRtosTaskState * ) opaque;
    if( state == NULL ) return;
    state->pxCurrentTCB = pxCurrentTCB;
    state->pxReadyTasksLists[ 0 ] = pxReadyTasksLists[ 0 ];
    for( UBaseType_t i = 1; i < configMAX_PRIORITIES; i++ )
        state->pxReadyTasksLists[ i ] = pxReadyTasksLists[ i ];
    state->xDelayedTaskList1 = xDelayedTaskList1;
    state->xDelayedTaskList2 = xDelayedTaskList2;
    state->pxDelayedTaskList = pxDelayedTaskList;
    state->pxOverflowDelayedTaskList = pxOverflowDelayedTaskList;
    state->xPendingReadyList = xPendingReadyList;
#if ( INCLUDE_vTaskDelete == 1 )
    state->xTasksWaitingTermination = xTasksWaitingTermination;
    state->uxDeletedTasksWaitingCleanUp = uxDeletedTasksWaitingCleanUp;
#endif
#if ( INCLUDE_vTaskSuspend == 1 )
    state->xSuspendedTaskList = xSuspendedTaskList;
#endif
    state->uxCurrentNumberOfTasks = uxCurrentNumberOfTasks;
    state->xTickCount = xTickCount;
    state->uxTopReadyPriority = uxTopReadyPriority;
    state->xSchedulerRunning = xSchedulerRunning;
    state->xPendedTicks = xPendedTicks;
    for( UBaseType_t i = 0; i < configNUMBER_OF_CORES; i++ )
        state->xYieldPendings[ i ] = xYieldPendings[ i ];
    state->xNumOfOverflows = xNumOfOverflows;
    state->uxTaskNumber = uxTaskNumber;
    state->xNextTaskUnblockTime = xNextTaskUnblockTime;
    for( UBaseType_t i = 0; i < configNUMBER_OF_CORES; i++ )
        state->xIdleTaskHandles[ i ] = xIdleTaskHandles[ i ];
    state->uxSchedulerSuspended = uxSchedulerSuspended;
}

void sim_freertos_task_state_restore( const void *opaque )
{
    const SimFreeRtosTaskState zero = { 0 };
    const SimFreeRtosTaskState *state =
        opaque == NULL ? &zero : ( const SimFreeRtosTaskState * ) opaque;
    pxCurrentTCB = state->pxCurrentTCB;
    for( UBaseType_t i = 0; i < configMAX_PRIORITIES; i++ )
        pxReadyTasksLists[ i ] = state->pxReadyTasksLists[ i ];
    xDelayedTaskList1 = state->xDelayedTaskList1;
    xDelayedTaskList2 = state->xDelayedTaskList2;
    pxDelayedTaskList = state->pxDelayedTaskList;
    pxOverflowDelayedTaskList = state->pxOverflowDelayedTaskList;
    xPendingReadyList = state->xPendingReadyList;
#if ( INCLUDE_vTaskDelete == 1 )
    xTasksWaitingTermination = state->xTasksWaitingTermination;
    uxDeletedTasksWaitingCleanUp = state->uxDeletedTasksWaitingCleanUp;
#endif
#if ( INCLUDE_vTaskSuspend == 1 )
    xSuspendedTaskList = state->xSuspendedTaskList;
#endif
    uxCurrentNumberOfTasks = state->uxCurrentNumberOfTasks;
    xTickCount = state->xTickCount;
    uxTopReadyPriority = state->uxTopReadyPriority;
    xSchedulerRunning = state->xSchedulerRunning;
    xPendedTicks = state->xPendedTicks;
    for( UBaseType_t i = 0; i < configNUMBER_OF_CORES; i++ )
        xYieldPendings[ i ] = state->xYieldPendings[ i ];
    xNumOfOverflows = state->xNumOfOverflows;
    uxTaskNumber = state->uxTaskNumber;
    xNextTaskUnblockTime = state->xNextTaskUnblockTime;
    for( UBaseType_t i = 0; i < configNUMBER_OF_CORES; i++ )
        xIdleTaskHandles[ i ] = state->xIdleTaskHandles[ i ];
    uxSchedulerSuspended = state->uxSchedulerSuspended;

    /* Never-started / NULL snapshot: rebuild empty lists so tick advance is
     * safe before vTaskStartScheduler runs. */
    if( pxDelayedTaskList == NULL )
    {
        prvInitialiseTaskLists();
        xNextTaskUnblockTime = portMAX_DELAY;
    }
}

/*
 * traceTASK_CREATE: create the task's fiber right away.  This runs inside
 * xTaskCreate*(), either before the scheduler starts or at runtime from a
 * running task; the engine never holds its task table borrowed while a task
 * runs, so both are fine.
 */
void sim_port_task_created( void *pvTCB )
{
    TCB_t *tcb = ( TCB_t * ) pvTCB;
    const SimPortFrame *frame = ( const SimPortFrame * ) ( tcb->pxTopOfStack + 1 );

    configASSERT( frame->magic == SIM_PORT_FRAME_MAGIC );

    SIM_TCB_HANDLE( tcb ) = ( void * ) sim_freertos_task_created(
        tcb,
        tcb->pcTaskName,
        frame->code,
        frame->param,
        configMINIMAL_STACK_SIZE,
        ( uint32_t ) tcb->uxPriority );
}

/* ── Kernel accessors for the engine ─────────────────────────────── */

uint64_t sim_freertos_current_handle( void )
{
    return ( pxCurrentTCB != NULL ) ? ( uint64_t ) ( uintptr_t ) SIM_TCB_HANDLE( pxCurrentTCB ) : 0;
}

uint32_t sim_freertos_current_is_idle( void )
{
    return ( pxCurrentTCB != NULL ) && ( pxCurrentTCB == xIdleTaskHandles[ 0 ] );
}

uint32_t sim_freertos_ticks_until_unblock( void )
{
    if( pxDelayedTaskList == NULL )
    {
        return 0xFFFFFFFFu; /* no task created yet */
    }

    if( listLIST_IS_EMPTY( pxDelayedTaskList ) == pdFALSE )
    {
        /* xNextTaskUnblockTime is the head of the delayed list. */
        return ( uint32_t ) ( xNextTaskUnblockTime - xTickCount );
    }

    if( listLIST_IS_EMPTY( pxOverflowDelayedTaskList ) == pdFALSE )
    {
        /* Wake-ups past the next tick-counter wrap: advance to the wrap,
         * where the kernel swaps the delayed lists. */
        return ( uint32_t ) ( ( TickType_t ) 0U - xTickCount );
    }

    return 0xFFFFFFFFu;
}

uint32_t sim_freertos_scheduler_running( void )
{
    return xSchedulerRunning != pdFALSE;
}
"#;
    content.push_str(bridge_functions);

    fs::write(dest_path, content)?;
    Ok(())
}

fn patch_timers_c(src_path: &Path, dest_path: &Path) -> std::io::Result<()> {
    let mut content = fs::read_to_string(src_path)?.replace("\r\n", "\n");
    let marker = "#include \"task.h\"";
    let pos = content
        .find(marker)
        .expect("Failed to find task.h include in timers.c");
    content.insert_str(
        pos + marker.len(),
        "\n#include <stdlib.h>\n\n/* Simulator: prvSampleTimeNow()'s function-local static lives here so it\n * is part of the per-World timer state below. */\nstatic TickType_t xLastTime = ( TickType_t ) 0U;",
    );
    let local_last_time =
        "        PRIVILEGED_DATA static TickType_t xLastTime = ( TickType_t ) 0U;\n";
    assert!(
        content.contains(local_last_time),
        "Failed to find prvSampleTimeNow's xLastTime in timers.c"
    );
    content = content.replace(local_last_time, "");

    // The timer command queue normally lives in function-local static
    // storage when static allocation is enabled, which every simulated
    // machine would share.  Allocate it from the active machine's heap.
    let static_timer_queue = "                    PRIVILEGED_DATA static StaticQueue_t xStaticTimerQueue;\n                    PRIVILEGED_DATA static uint8_t ucStaticTimerQueueStorage[ ( size_t ) configTIMER_QUEUE_LENGTH * sizeof( DaemonTaskMessage_t ) ];\n\n                    xTimerQueue = xQueueCreateStatic( ( UBaseType_t ) configTIMER_QUEUE_LENGTH, ( UBaseType_t ) sizeof( DaemonTaskMessage_t ), &( ucStaticTimerQueueStorage[ 0 ] ), &xStaticTimerQueue );\n";
    assert!(
        content.contains(static_timer_queue),
        "Failed to find the static timer queue in timers.c"
    );
    content = content.replace(
        static_timer_queue,
        "                    /* Simulator: one queue per machine, not a shared static. */\n                    xTimerQueue = xQueueCreate( ( UBaseType_t ) configTIMER_QUEUE_LENGTH, ( UBaseType_t ) sizeof( DaemonTaskMessage_t ) );\n",
    );

    /*
     * Keep timer daemon/list state paired with the tasks.c snapshot.  Queue
     * instances themselves are allocated through pvPortMalloc and remain
     * reachable through these saved pointers while their World is inactive.
     */
    content.push_str(
        r#"
/* ── Native simulator per-World timer context ─────────────────────── */
typedef struct SimFreeRtosTimerState
{
    List_t xActiveTimerList1;
    List_t xActiveTimerList2;
    List_t *pxCurrentTimerList;
    List_t *pxOverflowTimerList;
    QueueHandle_t xTimerQueue;
    TaskHandle_t xTimerTaskHandle;
    TickType_t xLastTime;
} SimFreeRtosTimerState;

void *sim_freertos_timer_state_create( void )
{
    return calloc( 1, sizeof( SimFreeRtosTimerState ) );
}

void sim_freertos_timer_state_destroy( void *opaque )
{
    free( opaque );
}

void sim_freertos_timer_state_save( void *opaque )
{
    SimFreeRtosTimerState *state = ( SimFreeRtosTimerState * ) opaque;
    if( state == NULL ) return;
    state->xActiveTimerList1 = xActiveTimerList1;
    state->xActiveTimerList2 = xActiveTimerList2;
    state->pxCurrentTimerList = pxCurrentTimerList;
    state->pxOverflowTimerList = pxOverflowTimerList;
    state->xTimerQueue = xTimerQueue;
    state->xTimerTaskHandle = xTimerTaskHandle;
    state->xLastTime = xLastTime;
}

/* Non-zero once the firmware created a software timer (the timer service
 * queue exists), even if it created no task. */
uint32_t sim_freertos_timers_in_use( void )
{
    return xTimerQueue != NULL;
}

void sim_freertos_timer_state_restore( const void *opaque )
{
    const SimFreeRtosTimerState zero = { 0 };
    const SimFreeRtosTimerState *state =
        opaque == NULL ? &zero : ( const SimFreeRtosTimerState * ) opaque;
    xActiveTimerList1 = state->xActiveTimerList1;
    xActiveTimerList2 = state->xActiveTimerList2;
    pxCurrentTimerList = state->pxCurrentTimerList;
    pxOverflowTimerList = state->pxOverflowTimerList;
    xTimerQueue = state->xTimerQueue;
    xTimerTaskHandle = state->xTimerTaskHandle;
    xLastTime = state->xLastTime;
}
"#,
    );
    fs::write(dest_path, content)?;
    Ok(())
}

fn build_tcp_stack(build: &mut cc::Build) {
    let tcp_dir = Path::new("FreeRTOS-Plus-TCP/source");

    // ── Our NetworkInterface driver ────────────────────────────────
    build.file("c/sim_net_if.c");

    // ── FreeRTOS+TCP include paths ─────────────────────────────────
    build
        .include(tcp_dir.join("include"))
        .include(tcp_dir.join("portable/NetworkInterface/include"))
        .include(tcp_dir.join("portable/Compiler/GCC"));

    // ── Core IP stack ──────────────────────────────────────────────
    for f in &[
        "FreeRTOS_IP.c",
        "FreeRTOS_ARP.c",
        "FreeRTOS_ICMP.c",
        "FreeRTOS_Sockets.c",
        "FreeRTOS_Stream_Buffer.c",
        "FreeRTOS_IP_Timers.c",
        "FreeRTOS_IP_Utils.c",
    ] {
        build.file(tcp_dir.join(f));
    }

    // ── TCP ────────────────────────────────────────────────────────
    for f in &[
        "FreeRTOS_TCP_IP.c",
        "FreeRTOS_TCP_Reception.c",
        "FreeRTOS_TCP_Transmission.c",
        "FreeRTOS_TCP_State_Handling.c",
        "FreeRTOS_TCP_Utils.c",
        "FreeRTOS_TCP_WIN.c",
        "FreeRTOS_Tiny_TCP.c",
        // IPv4-specific TCP implementations.
        "FreeRTOS_TCP_IP_IPv4.c",
        "FreeRTOS_TCP_Transmission_IPv4.c",
        "FreeRTOS_TCP_State_Handling_IPv4.c",
        "FreeRTOS_TCP_Utils_IPv4.c",
    ] {
        build.file(tcp_dir.join(f));
    }

    // ── IPv4 ───────────────────────────────────────────────────────
    for f in &[
        "FreeRTOS_IPv4.c",
        "FreeRTOS_IPv4_Sockets.c",
        "FreeRTOS_IPv4_Utils.c",
    ] {
        build.file(tcp_dir.join(f));
    }

    // ── UDP ────────────────────────────────────────────────────────
    build.file(tcp_dir.join("FreeRTOS_UDP_IP.c"));
    build.file(tcp_dir.join("FreeRTOS_UDP_IPv4.c"));

    // ── Routing ────────────────────────────────────────────────────
    build.file(tcp_dir.join("FreeRTOS_Routing.c"));

    // ── DNS (basic) ────────────────────────────────────────────────
    for f in &[
        "FreeRTOS_DNS.c",
        "FreeRTOS_DNS_Cache.c",
        "FreeRTOS_DNS_Parser.c",
    ] {
        build.file(tcp_dir.join(f));
    }

    // ── Buffer management (portable layer) ─────────────────────────
    build.file(tcp_dir.join("portable/BufferManagement/BufferAllocation_2.c"));

    // ── NetworkInterface common ────────────────────────────────────
    build.file(tcp_dir.join("portable/NetworkInterface/Common/phyHandling.c"));

    println!(
        "cargo:warning=FreeRTOS+TCP stack compiled ({} source files)",
        21
    );
}
