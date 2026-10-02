//! The interrupt-mask semantics (docs/scheduling.md, "Interrupt masking")
//! in a World, as one property over every masked work source.
//!
//! Host code masks a machine's interrupts at boot and unmasks them at
//! 10 ms.  Meanwhile: a peripheral callback due at 5 ms, IRQ input
//! arriving at 5 ms, and (FreeRTOS) a task sleeping until tick 5.  The
//! callback runs on time and masked; the ISR and the sleeper wait for the
//! unmask and run exactly then; time keeps moving; and the World wakes the
//! machine only for work it can do (no busy-wake).  A busy task that masks
//! interrupts itself keeps time moving too.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use sim_core::Tick;
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

extern "C" {
    fn costar_test_spawn_task(name: *const std::ffi::c_char, body: extern "C" fn(), priority: u32);
}

thread_local! {
    /// `(label, firmware tick, masked)`.
    static LOG: RefCell<Vec<(&'static str, u64, bool)>> = const { RefCell::new(Vec::new()) };
}

fn log(label: &'static str) {
    let at = unsafe { sim_ffi::sim_now_ticks() };
    LOG.with(|l| {
        l.borrow_mut()
            .push((label, at, sim_ffi::is_critical_locked()))
    });
}

unsafe extern "C" fn callback() {
    log("callback");
}

unsafe extern "C" fn isr() {
    log("isr");
}

extern "C" fn sleeper() {
    unsafe { sim_ffi::sim_task_delay_until(5) };
    log("sleeper");
    loop {
        unsafe { sim_ffi::sim_task_delay_until(1_000) };
    }
}

extern "C" fn busy() {
    unsafe {
        sim_ffi::sim_budget_set_limit(2);
        sim_ffi::sim_budget_reset();
        sim_ffi::sim_enter_critical();
        while sim_ffi::sim_now_ticks() < 5 {
            sim_ffi::sim_budget_poll(std::ptr::null(), 1);
        }
        log("busy_done");
        sim_ffi::sim_exit_critical();
        sim_ffi::sim_budget_set_limit(1_000_000);
        loop {
            sim_ffi::sim_task_delay_until(1_000);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Case {
    /// Host-masked: callback, IRQ input and (FreeRTOS) a sleeper.
    HostMasked,
    /// A FreeRTOS task masks interrupts and busy-waits for tick 5.
    BusyMasked,
}

struct Fw {
    freertos: bool,
    case: Case,
    steps: Arc<AtomicU32>,
    unmasked: bool,
}

impl Firmware for Fw {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe { sim_ffi::device_ffi::sim_irq_set_handler(6, Some(isr)) };
        match (self.case, self.freertos) {
            (Case::HostMasked, true) => unsafe {
                costar_test_spawn_task(c"sleeper".as_ptr(), sleeper, 2)
            },
            (Case::HostMasked, false) => {}
            (Case::BusyMasked, _) => unsafe { costar_test_spawn_task(c"busy".as_ptr(), busy, 1) },
        }
        if self.case == Case::HostMasked {
            unsafe { sim_ffi::sim_schedule_event(5, Some(callback)) };
        }
    }

    fn step(&mut self, now: Tick, machine: &mut Machine) {
        self.steps.fetch_add(1, Ordering::SeqCst);
        let _active = machine.activate();
        if self.case == Case::HostMasked && now >= 10_000 && !self.unmasked {
            self.unmasked = true;
            // At World time `now` (firmware tick now / 1000).
            LOG.with(|l| l.borrow_mut().push(("unmask", now / 1_000, false)));
            sim_ffi::freertos::sim_enable_interrupts();
        }
        unsafe { sim_ffi::sim_scheduler_tick() };
        // Masked by the host once the firmware has booted (starting the
        // kernel resets the interrupt state).
        if self.case == Case::HostMasked && now == 0 {
            sim_ffi::freertos::sim_disable_interrupts();
        }
        sim_ffi::flush_trace();
    }
}

/// Runs a case to 20 ms.  Returns the log and the firmware steps taken
/// before the unmask.
fn run(freertos: bool, case: Case) -> (Vec<(&'static str, u64, bool)>, u32) {
    LOG.with(|l| l.borrow_mut().clear());
    let steps = Arc::new(AtomicU32::new(0));
    let mut world = World::new();
    world.enable_owned_device_banks();
    let mut machine = Machine::with_defaults(1, "m");
    machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
    // Steps the machine at 10 ms, where the host unmasks.
    machine.schedule_at(10_000, 0, "unmask", Box::new(|_| {}));
    world.add_machine(machine);
    world.machine_mut(1).unwrap().load_firmware(Box::new(Fw {
        freertos,
        case,
        steps: steps.clone(),
        unmasked: false,
    }));
    if case == Case::HostMasked {
        world.machine_mut(1).unwrap().raise_irq(6, 5_000);
    }
    world.run_until(9_999).unwrap();
    let masked_steps = steps.load(Ordering::SeqCst);
    world.run_until(20_000).unwrap();
    (LOG.with(|l| l.borrow().clone()), masked_steps)
}

#[test]
fn a_host_masked_freertos_machine_follows_the_mask_semantics() {
    let (log, masked_steps) = run(true, Case::HostMasked);
    assert_eq!(
        log,
        vec![
            // On time and masked.
            ("callback", 5, true),
            ("unmask", 10, false),
            // Deferred to the unmask: the IRQ, then the task the held-off
            // ticks woke.
            ("isr", 10, false),
            ("sleeper", 10, false),
        ]
    );
    // Boot, the callback / IRQ-arrival wake at 5 ms and nothing else: no
    // busy-wake while masked.
    assert!(masked_steps <= 4, "{masked_steps} steps while masked");
}

#[test]
fn a_host_masked_native_machine_follows_the_mask_semantics() {
    let (log, masked_steps) = run(false, Case::HostMasked);
    assert_eq!(
        log,
        vec![
            ("callback", 5, true),
            ("unmask", 10, false),
            ("isr", 10, false),
        ]
    );
    assert!(masked_steps <= 4, "{masked_steps} steps while masked");
}

#[test]
fn a_busy_masked_freertos_task_keeps_time_moving_in_a_world() {
    let (log, steps) = run(true, Case::BusyMasked);
    assert_eq!(log, vec![("busy_done", 5, true)]);
    // One step per tick of CPU time at most.
    assert!(steps <= 12, "{steps} steps");
}
