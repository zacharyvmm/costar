//! Standalone firmware that starts FreeRTOS itself (`vTaskStartScheduler()`,
//! which runs `sim_start_scheduler()`) after native-only steps keeps the
//! machine's virtual clock instead of restarting it at 0.
//!
//! Its own test binary: it drives the thread's default (standalone)
//! simulator and kernel context, without the kernel lock a `Simulator`
//! activation takes, so it must not share a process with other kernel users.

use std::ffi::{c_char, c_long, c_ulong, c_void};

extern "C" {
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
    fn vTaskStartScheduler();
    fn sim_freertos_context_create() -> *mut c_void;
    fn sim_freertos_context_activate(ctx: *mut c_void) -> *mut c_void;
    fn vTaskDelay(ticks: u32);
    fn xTaskGetTickCount() -> u32;
}

fn records(label: &str) -> Vec<(u64, u32)> {
    sim_ffi::with_global(|g| {
        g.trace
            .as_ref()
            .unwrap()
            .events
            .iter()
            .filter_map(|e| match e {
                sim_core::trace::TraceEvent::UserU32 {
                    at,
                    label: l,
                    value,
                } if *l == label => Some((*at, *value)),
                _ => None,
            })
            .collect()
    })
}

unsafe extern "C" fn delayed(_: *mut c_void) {
    vTaskDelay(5);
    sim_ffi::sim_trace_u32(c"kernel_tick".as_ptr(), xTaskGetTickCount());
}

#[test]
fn standalone_late_start_keeps_the_clock() {
    sim_ffi::init_global(Box::new(sim_core::trace::TraceSink::new()));
    unsafe { sim_freertos_context_activate(sim_freertos_context_create()) };
    // Native-only steps up to tick 3.
    sim_ffi::spawn_rust_task("native", 5, 4096, |ctx| ctx.sleep_until(3));
    for _ in 0..20 {
        unsafe { sim_ffi::sim_scheduler_tick() };
        if sim_ffi::with_global(|g| g.scheduler_sim_time) >= 3 {
            break;
        }
    }
    assert_eq!(sim_ffi::with_global(|g| g.scheduler_sim_time), 3);
    // Standalone firmware then starts FreeRTOS itself.
    unsafe {
        assert_eq!(
            xTaskCreate(
                delayed,
                c"delayed".as_ptr(),
                128,
                std::ptr::null_mut(),
                5,
                std::ptr::null_mut()
            ),
            1
        );
        vTaskStartScheduler();
    }
    // vTaskDelay(5) from tick 3: timestamp and kernel tick agree.
    assert_eq!(records("kernel_tick"), vec![(8, 8)]);
}
