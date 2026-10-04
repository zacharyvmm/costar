//! An ISR's mask survives retiring the task it interrupted: the retired
//! task's interrupt state is released once, at retirement, before any ISR
//! runs — never again afterwards, which would clear a mask a later ISR set
//! (IRQ 6's ISR masks, holding IRQ 7).  Exit and fault, standalone and
//! bounded (the reviewer's probes).

use sim_core::SimConfig;
use sim_ffi::device_ffi::{sim_irq_raise, sim_irq_set_handler};
use sim_ffi::simulator::Simulator;
use std::cell::RefCell;
extern "C" {
    fn costar_test_spawn_task(name: *const std::ffi::c_char, body: extern "C" fn(), priority: u32);
    fn sim_disable_interrupts();
}
thread_local! { static ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) }; }
unsafe extern "C" fn masking_isr() {
    ORDER.with(|v| v.borrow_mut().push("masking_isr"));
    sim_disable_interrupts();
}
unsafe extern "C" fn held_isr() {
    ORDER.with(|v| v.borrow_mut().push("held_isr"));
}
extern "C" fn exiting_task() {
    unsafe {
        sim_ffi::sim_enter_critical();
        sim_irq_raise(6);
        sim_irq_raise(7);
        sim_ffi::sim_task_exit();
    }
}
#[test]
fn isr_mask_survives_retiring_the_interrupted_task() {
    let mut failures = 0;
    for bounded in [true, false] {
        let result = std::thread::spawn(move || {
            let mut sim = Simulator::new(SimConfig::default());
            sim.enable_owned_devices();
            if bounded {
                sim.set_scheduler_limit(Some(0));
            }
            let _active = sim.activate();
            unsafe {
                sim_irq_set_handler(6, Some(masking_isr));
                sim_irq_set_handler(7, Some(held_isr));
                costar_test_spawn_task(c"exiting".as_ptr(), exiting_task, 3);
                sim_ffi::sim_scheduler_tick();
            }
            let order = ORDER.with(|v| v.borrow().clone());
            eprintln!(
                "exit: bounded={bounded}, order={order:?}, masked={}",
                sim_ffi::is_critical_locked()
            );
            assert_eq!(order, ["masking_isr"]);
            assert!(sim_ffi::is_critical_locked());
        })
        .join();
        failures += usize::from(result.is_err());
    }
    assert_eq!(failures, 0);
}

extern "C" fn seed_task() {}
#[test]
fn isr_mask_survives_retiring_a_faulted_task() {
    let mut failures = 0;
    for bounded in [false, true] {
        let result = std::thread::spawn(move || {
            let mut sim = Simulator::new(SimConfig::default());
            sim.enable_owned_devices();
            if bounded {
                sim.set_scheduler_limit(Some(0));
            }
            let _active = sim.activate();
            unsafe {
                sim_irq_set_handler(6, Some(masking_isr));
                sim_irq_set_handler(7, Some(held_isr));
                costar_test_spawn_task(c"seed".as_ptr(), seed_task, 1);
            }
            sim_ffi::spawn_rust_task("faulting", 3, 65536, |_| {
                unsafe {
                    sim_ffi::sim_enter_critical();
                    sim_irq_raise(6);
                    sim_irq_raise(7);
                }
                panic!("task fault");
            });
            unsafe {
                sim_ffi::sim_scheduler_tick();
            }
            let order = ORDER.with(|v| v.borrow().clone());
            eprintln!(
                "fault: bounded={bounded}, order={order:?}, masked={}",
                sim_ffi::is_critical_locked()
            );
            assert_eq!(order, ["masking_isr"]);
            assert!(sim_ffi::is_critical_locked());
        })
        .join();
        failures += usize::from(result.is_err());
    }
    assert_eq!(failures, 0);
}
