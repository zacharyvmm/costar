//! A failed FreeRTOS assertion (`configASSERT`) in an ISR.
//!
//! On a task's fiber (the ISR interrupted a task) the task stops, as for
//! any assertion in a task, and the machine keeps running.  In scheduler
//! context (an IRQ arriving while the machine is parked) there is no task
//! to stop: the process aborts deliberately with a diagnostic, instead of
//! returning into the kernel and crashing.  That case runs in a child
//! process.

use std::ffi::{c_char, c_long, c_void};

use sim_core::{SimConfig, TraceEvent};
use sim_ffi::device_ffi::{sim_irq_raise, sim_irq_set_handler};
use sim_ffi::simulator::Simulator;

extern "C" {
    fn costar_test_spawn_task(name: *const c_char, body: extern "C" fn(), priority: u32);
    fn xQueueGiveFromISR(queue: *mut c_void, higher_priority_task_woken: *mut c_long) -> c_long;
}

unsafe extern "C" fn faulting_isr() {
    // xSemaphoreGiveFromISR(NULL, NULL): queue.c guards it with configASSERT.
    xQueueGiveFromISR(std::ptr::null_mut(), std::ptr::null_mut());
    sim_ffi::sim_trace_u32(c"after_assert".as_ptr(), 1);
}

extern "C" fn parked_task() {
    unsafe { sim_ffi::sim_task_delay_until(100) };
}

extern "C" fn interrupted_task() {
    unsafe { sim_irq_raise(6) };
    unsafe { sim_ffi::sim_trace_u32(c"task_continued".as_ptr(), 1) };
}

extern "C" fn healthy_task() {
    unsafe { sim_ffi::sim_trace_u32(c"healthy_ran".as_ptr(), 1) };
}

#[test]
fn an_assertion_in_an_isr_on_a_task_fiber_stops_that_task() {
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    let _a = sim.activate();
    unsafe {
        sim_irq_set_handler(6, Some(faulting_isr));
        costar_test_spawn_task(c"interrupted".as_ptr(), interrupted_task, 3);
        costar_test_spawn_task(c"healthy".as_ptr(), healthy_task, 2);
    }
    for step in 0..10 {
        sim.set_scheduler_limit(Some(step));
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
    sim_ffi::flush_trace();
    let g = sim.sim_global.borrow();
    let labels: Vec<_> = g
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 { label, .. } => Some(*label),
            TraceEvent::Fatal { .. } => Some("fatal"),
            _ => None,
        })
        .collect();
    // The assertion stopped the interrupted task (nothing after it ran in
    // the ISR or the task); the healthy task still ran.
    assert!(labels.contains(&"fatal"), "{labels:?}");
    assert!(!labels.contains(&"after_assert"), "{labels:?}");
    assert!(!labels.contains(&"task_continued"), "{labels:?}");
    assert!(labels.contains(&"healthy_ran"), "{labels:?}");
}

/// The child process's part: an IRQ taken while the machine is parked.
fn scheduler_context_assert() {
    let mut sim = Simulator::new(SimConfig::default());
    sim.enable_owned_devices();
    sim.set_scheduler_limit(Some(0));
    let _a = sim.activate();
    unsafe {
        sim_irq_set_handler(6, Some(faulting_isr));
        costar_test_spawn_task(c"parked".as_ptr(), parked_task, 3);
        sim_ffi::sim_scheduler_tick();
        sim_irq_raise(6);
    }
    println!("CHILD: the ISR returned past its failed assertion");
}

#[test]
fn an_assertion_in_a_scheduler_context_isr_aborts_with_a_diagnostic() {
    if std::env::var_os("COSTAR_ISR_ASSERT_CHILD").is_some() {
        scheduler_context_assert();
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "an_assertion_in_a_scheduler_context_isr_aborts_with_a_diagnostic",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("COSTAR_ISR_ASSERT_CHILD", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SIGABRT (a deliberate abort), not SIGSEGV.
        assert_eq!(out.status.signal(), Some(6), "{:?}\n{stderr}", out.status);
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("failed outside any task") && stderr.contains("queue.c"),
        "no diagnostic: {stderr}"
    );
    assert!(!stdout.contains("returned past"), "{stdout}");
}
