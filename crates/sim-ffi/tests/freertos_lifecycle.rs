//! FreeRTOS machine lifecycle regressions: budget ticks across World steps
//! within one tick, legacy task matching after a task has gone, stepping
//! after `vTaskEndScheduler()`, and the longest finite delay.
//!
//! Each test drives a `Simulator` directly through the C ABI.

use sim_core::{trace::TraceEvent, SimConfig};
use sim_ffi::simulator::Simulator;
use std::ffi::{c_char, c_long, c_ulong, c_void};

extern "C" {
    fn costar_test_abi_delay_boot();
    fn xTaskCreate(
        entry: unsafe extern "C" fn(*mut c_void),
        name: *const c_char,
        depth: u16,
        arg: *mut c_void,
        priority: c_ulong,
        handle: *mut *mut c_void,
    ) -> c_long;
    fn vTaskDelay(ticks: u32);
    fn vTaskEndScheduler();
}

fn records(
    global: &std::rc::Rc<std::cell::RefCell<sim_ffi::SimGlobal>>,
    label: &str,
) -> Vec<(u64, u32)> {
    global
        .borrow()
        .trace
        .as_ref()
        .unwrap()
        .events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::UserU32 {
                at,
                label: l,
                value,
            } if *l == label => Some((*at, *value)),
            _ => None,
        })
        .collect()
}

#[test]
fn same_tick_step_must_not_resume_a_budget_exhausted_task() {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    let _active = sim.activate();
    unsafe {
        costar_test_abi_delay_boot();
    }
    sim_ffi::spawn_rust_task("high", 6, 4096, |ctx| {
        ctx.sleep_for(1);
        unsafe {
            sim_ffi::sim_trace_u32(c"high_woke".as_ptr(), ctx.now() as u32);
        }
    });
    sim_ffi::spawn_rust_task("low", 5, 4096, |ctx| unsafe {
        sim_ffi::sim_budget_set_limit(1);
        sim_ffi::sim_budget_poll(std::ptr::null(), 1);
        sim_ffi::sim_trace_u32(c"after_budget".as_ptr(), ctx.now() as u32);
    });
    global.borrow_mut().scheduler_limit = Some(0);
    unsafe {
        sim_ffi::sim_scheduler_tick();
    }
    assert!(records(&global, "after_budget").is_empty());
    // World advances 500 us, which still maps to tick limit 0.
    unsafe {
        sim_ffi::sim_scheduler_tick();
    }
    assert!(
        records(&global, "after_budget").is_empty(),
        "resumed before tick 1: {:?}",
        records(&global, "after_budget")
    );
}

unsafe extern "C" fn returning(arg: *mut c_void) {
    sim_ffi::sim_trace_u32(c"called_with".as_ptr(), arg as usize as u32);
}

#[test]
fn sim_create_task_after_same_entry_exits_must_create_a_new_task() {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    let _active = sim.activate();
    unsafe {
        assert_eq!(
            xTaskCreate(
                returning,
                c"first".as_ptr(),
                128,
                std::ptr::without_provenance_mut::<c_void>(1),
                3,
                std::ptr::null_mut()
            ),
            1
        );
        for _ in 0..100 {
            if sim_ffi::sim_scheduler_tick() == 0 {
                break;
            }
        }
        assert_eq!(records(&global, "called_with"), vec![(0, 1)]);
        let id = sim_ffi::sim_create_task(
            c"second".as_ptr(),
            Some(returning),
            std::ptr::without_provenance_mut::<c_void>(2),
            128,
            3,
        );
        let state = global
            .borrow()
            .tasks
            .iter()
            .find(|t| t.id == id as u64)
            .unwrap()
            .state;
        assert!(
            !matches!(state, sim_fiber::TaskState::Exited),
            "returned the finished task"
        );
        for _ in 0..100 {
            if sim_ffi::sim_scheduler_tick() == 0 {
                break;
            }
        }
    }
    assert_eq!(records(&global, "called_with"), vec![(0, 1), (0, 2)]);
    // The same entry, name and parameter as the finished task: still a new
    // task, not the finished one.
    unsafe {
        sim_ffi::sim_create_task(
            c"first".as_ptr(),
            Some(returning),
            std::ptr::without_provenance_mut::<c_void>(1),
            128,
            3,
        );
        for _ in 0..100 {
            if sim_ffi::sim_scheduler_tick() == 0 {
                break;
            }
        }
    }
    assert_eq!(
        records(&global, "called_with"),
        vec![(0, 1), (0, 2), (0, 1)]
    );
}

unsafe extern "C" fn stopper(_: *mut c_void) {
    vTaskDelay(1);
    vTaskEndScheduler();
}
unsafe extern "C" fn sleeper(_: *mut c_void) {
    vTaskDelay(10);
}

#[test]
fn ended_scheduler_must_not_create_new_kernel_tasks_on_later_steps() {
    let mut sim = Simulator::new(SimConfig::default());
    let global = sim.sim_global.clone();
    let _active = sim.activate();
    unsafe {
        assert_eq!(
            xTaskCreate(
                stopper,
                c"stopper".as_ptr(),
                128,
                std::ptr::null_mut(),
                3,
                std::ptr::null_mut()
            ),
            1
        );
    }
    unsafe {
        assert_eq!(
            xTaskCreate(
                sleeper,
                c"sleeper".as_ptr(),
                128,
                std::ptr::null_mut(),
                1,
                std::ptr::null_mut()
            ),
            1
        );
    }
    global.borrow_mut().scheduler_limit = Some(1);
    assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0);
    let before = global.borrow().tasks.len();
    for tick in 2..10 {
        global.borrow_mut().scheduler_limit = Some(tick);
        assert_eq!(unsafe { sim_ffi::sim_scheduler_tick() }, 0);
    }
    assert!(
        !global
            .borrow()
            .trace
            .as_ref()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e, TraceEvent::Fatal { .. })),
        "normal stepping after vTaskEndScheduler produced a kernel assertion"
    );
    assert_eq!(
        global.borrow().tasks.len(),
        before,
        "stopped simulation restarted kernel tasks"
    );
}
