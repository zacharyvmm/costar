//! No scheduler switch while interrupts are masked, on any path.
//!
//! Host code may mask a machine's interrupts between steps (as a debugger
//! or a test does).  The engine then holds every switch off — the owed budget
//! tick, the tick charged for a used-up budget, the parked idle task of a
//! bounded step — and the kernel tick is not serviced (the tick interrupt
//! is masked too).  Unmasking services the ticks and performs the switch.

use sim_core::{SimConfig, TraceEvent};
use sim_ffi::simulator::Simulator;
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
    fn vTaskDelay(ticks: u32);
    fn vTaskSuspend(handle: *mut c_void);
    fn vTaskResume(handle: *mut c_void);
    fn xTaskGetTickCount() -> u32;
}

fn masked() -> u32 {
    u32::from(sim_ffi::is_critical_locked())
}

fn trace(label: &std::ffi::CStr, value: u32) {
    unsafe { sim_ffi::sim_trace_u32(label.as_ptr(), value) };
}

/// `(time, label, value)` of every `sim_trace_u32` record.
fn records(sim: &Simulator) -> Vec<(u64, &'static str, u32)> {
    sim_ffi::flush_trace();
    sim.sim_global
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 { at, label, value } if *label != "budget_exceeded" => {
                Some((*at, *label, *value))
            }
            _ => None,
        })
        .collect()
}

unsafe extern "C" fn high(_: *mut c_void) {
    vTaskDelay(1);
    trace(c"high_masked", masked());
    delete_self();
}

unsafe extern "C" fn low(_: *mut c_void) {
    sim_ffi::sim_budget_set_limit(1);
    sim_ffi::sim_budget_poll(std::ptr::null(), 0);
    // Resumed after the owed tick, with interrupts masked by the host.
    trace(c"low_masked", masked());
    // The tick interrupt was masked: the kernel has not counted it yet.
    trace(c"low_kernel_tick", xTaskGetTickCount());
    sim_ffi::freertos::sim_enable_interrupts();
    // The unmask serviced the tick and the woken high-priority task ran.
    trace(c"low_after_unmask", xTaskGetTickCount());
    delete_self();
}

unsafe fn delete_self() {
    extern "C" {
        fn vTaskDelete(task: *mut c_void);
    }
    vTaskDelete(std::ptr::null_mut());
}

/// A busy low-priority task uses up its budget at a bounded step's limit
/// (tick 0); host code masks interrupts before the next step.  Charging the
/// owed tick must not switch to the high-priority task due at tick 1.
fn owed_tick_case(bounded: bool) {
    let mut sim = Simulator::new(SimConfig::default());
    let _a = sim.activate();
    unsafe {
        assert_eq!(
            xTaskCreate(
                high,
                c"high".as_ptr(),
                128,
                std::ptr::null_mut(),
                6,
                std::ptr::null_mut()
            ),
            1
        );
        assert_eq!(
            xTaskCreate(
                low,
                c"low".as_ptr(),
                128,
                std::ptr::null_mut(),
                5,
                std::ptr::null_mut()
            ),
            1
        );
    }
    sim.set_scheduler_limit(Some(0));
    unsafe { sim_ffi::sim_scheduler_tick() };
    sim_ffi::freertos::sim_disable_interrupts();
    sim.set_scheduler_limit(bounded.then_some(1));
    for _ in 0..10 {
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    let r: Vec<_> = records(&sim)
        .into_iter()
        .map(|(_, label, value)| (label, value))
        .collect();
    assert_eq!(
        r,
        vec![
            ("low_masked", 1),
            ("low_kernel_tick", 0),
            ("high_masked", 0),
            ("low_after_unmask", 1),
        ],
        "bounded={bounded}"
    );
}

#[test]
fn owed_tick_does_not_switch_while_masked_bounded() {
    owed_tick_case(true);
}

#[test]
fn owed_tick_does_not_switch_while_masked_unbounded() {
    owed_tick_case(false);
}

unsafe extern "C" fn resumed(_: *mut c_void) {
    vTaskSuspend(std::ptr::null_mut());
    trace(c"resumed_masked", masked());
    delete_self();
}

/// A bounded step parks the idle task; host code masks interrupts and
/// resumes a suspended task.  The next steps must not run it while masked;
/// it runs after the host unmasks.
#[test]
fn parked_idle_does_not_switch_while_masked() {
    for bounded in [true, false] {
        let mut sim = Simulator::new(SimConfig::default());
        let _a = sim.activate();
        let mut handle = std::ptr::null_mut();
        unsafe {
            assert_eq!(
                xTaskCreate(
                    resumed,
                    c"resumed".as_ptr(),
                    128,
                    std::ptr::null_mut(),
                    5,
                    &mut handle
                ),
                1
            );
        }
        sim.set_scheduler_limit(Some(0));
        assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0);
        sim_ffi::freertos::sim_disable_interrupts();
        unsafe { vTaskResume(handle) };
        sim.set_scheduler_limit(bounded.then_some(2));
        for _ in 0..5 {
            unsafe { sim_ffi::sim_scheduler_tick() };
        }
        assert!(
            records(&sim).is_empty(),
            "bounded={bounded}: ran while masked"
        );
        // Unmasking wakes the machine; the next step runs the task.
        sim_ffi::freertos::sim_enable_interrupts();
        assert!(
            !sim.sim_global.borrow().freertos_quiescent,
            "bounded={bounded}"
        );
        for _ in 0..5 {
            unsafe { sim_ffi::sim_scheduler_tick() };
        }
        let r: Vec<_> = records(&sim).into_iter().map(|(_, l, v)| (l, v)).collect();
        assert_eq!(r, vec![("resumed_masked", 0)], "bounded={bounded}");
    }
}
