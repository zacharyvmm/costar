//! When a World steps a FreeRTOS machine again after `Firmware::step`.
//!
//! The wake-up is computed from every source of pending firmware work,
//! including work that appears after the firmware ran its scheduler in a
//! step: ISRs taken then (with or without a yield request), armed timers
//! and scheduled IRQs, owed budget ticks; and from the terminal state
//! (`vTaskEndScheduler()`).  Masked work must not busy-wake the machine.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use sim_core::Tick;
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

extern "C" {
    fn costar_test_external_irq_boot();
    fn costar_test_external_irq_no_yield_boot();
    fn costar_test_entry_isr_boot();
    fn costar_test_arm_then_end_boot();
    fn costar_test_isr_masks_in_scheduler_boot();
    fn costar_test_timer_storm_boot();
    fn costar_test_isr_schedules_event_boot();
    fn costar_test_callback_storm_boot();
    fn costar_test_retrigger_boot();
    fn costar_test_retrigger_count() -> u32;
}

/// The fixtures keep state in C statics.
static FIXTURE: Mutex<()> = Mutex::new(());

/// Boots `boot`, runs the scheduler on every step and, on the first step
/// only, runs `after_first` (under the machine's activation) once the
/// scheduler has returned.  Counts its steps.
struct Scripted {
    boot: unsafe extern "C" fn(),
    timer: bool,
    after_first: Option<Box<dyn FnOnce()>>,
    steps: Arc<AtomicU32>,
}

impl Firmware for Scripted {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        if self.timer {
            sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(0, 6));
        }
        unsafe { (self.boot)() };
    }

    fn step(&mut self, _now: Tick, machine: &mut Machine) {
        self.steps.fetch_add(1, Ordering::SeqCst);
        let _active = machine.activate();
        unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::flush_trace();
        if let Some(after) = self.after_first.take() {
            after();
            sim_ffi::flush_trace();
        }
    }
}

struct Outcome {
    world: World,
    steps: u32,
}

impl Outcome {
    fn times(&self, label: &str) -> Vec<u64> {
        let label = format!("\"{label}\"");
        self.world
            .drain_all_traces()
            .iter()
            .filter(|l| l.starts_with("[machine.1]") && l.contains(&label))
            .map(|l| {
                l["[machine.1]".len()..]
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect()
    }
}

fn run(
    boot: unsafe extern "C" fn(),
    timer: bool,
    after_first: impl FnOnce() + 'static,
    until: Tick,
) -> Outcome {
    let steps = Arc::new(AtomicU32::new(0));
    let mut world = World::new();
    world.enable_owned_device_banks();
    let mut machine = Machine::with_defaults(1, "m1");
    machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
    world.add_machine(machine);
    world
        .machine_mut(1)
        .unwrap()
        .load_firmware(Box::new(Scripted {
            boot,
            timer,
            after_first: Some(Box::new(after_first)),
            steps: steps.clone(),
        }));
    world.run_until(until).unwrap();
    let steps = steps.load(Ordering::SeqCst);
    Outcome { world, steps }
}

#[test]
fn isr_after_the_scheduler_that_requests_a_yield_runs_its_task() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let o = run(
        costar_test_external_irq_boot,
        false,
        || unsafe { sim_ffi::device_ffi::sim_irq_raise(6) },
        10_000,
    );
    assert_eq!(o.times("timer_isr"), vec![0]);
    assert_eq!(o.times("isr_woke_task"), vec![0]);
}

#[test]
fn isr_after_the_scheduler_that_only_readies_a_task_runs_it() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let o = run(
        costar_test_external_irq_no_yield_boot,
        false,
        || unsafe { sim_ffi::device_ffi::sim_irq_raise(6) },
        10_000,
    );
    assert_eq!(o.times("timer_isr"), vec![0]);
    assert_eq!(o.times("isr_woke_task"), vec![0]);
}

#[test]
fn ended_scheduler_with_an_armed_timer_stops_waking() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let o = run(costar_test_arm_then_end_boot, true, || {}, 20_000);
    assert_eq!(o.times("ending"), vec![0]);
    assert!(
        o.steps <= 3,
        "an ended machine kept waking: {} steps",
        o.steps
    );
    assert_eq!(o.world.machine(1).unwrap().next_event_time(), None);
}

#[test]
fn masked_pending_irq_does_not_busy_wake() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    // The host masks interrupts after the scheduler ran and raises IRQ 6:
    // it stays pending, and nothing can take it until an unmask.
    let o = run(
        costar_test_external_irq_boot,
        false,
        || unsafe {
            sim_ffi::freertos::sim_disable_interrupts();
            sim_ffi::device_ffi::sim_irq_raise(6);
        },
        20_000,
    );
    assert!(o.times("timer_isr").is_empty());
    assert!(
        o.steps <= 3,
        "masked work busy-woke the machine: {} steps",
        o.steps
    );
}

#[test]
fn owed_budget_tick_with_a_pending_yield_waits_for_the_next_tick() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    // The spinner uses up its budget at tick 0 (a tick is owed); the ISR
    // taken after the scheduler wakes the waiter, which runs at tick 1,
    // before the spinner — without the machine waking at every µs of tick 0.
    let o = run(
        costar_test_entry_isr_boot,
        false,
        || unsafe { sim_ffi::device_ffi::sim_irq_raise(6) },
        3_000,
    );
    assert_eq!(o.times("timer_isr"), vec![0]);
    assert_eq!(o.times("isr_woke_task"), vec![1_000]);
    assert_eq!(o.times("spinner_resumed"), vec![1_000]);
    assert!(
        o.steps <= 6,
        "busy-woke during the owed tick: {} steps",
        o.steps
    );
}

#[test]
fn timer_and_irq_staged_after_the_scheduler_still_wake() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let o = run(
        costar_test_external_irq_boot,
        true,
        || unsafe { sim_ffi::device_ffi::sim_timer_arm(0, 3) },
        10_000,
    );
    assert_eq!(o.times("timer_isr"), vec![3_000]);
    assert_eq!(o.times("isr_woke_task"), vec![3_000]);
}

#[test]
fn isr_that_readies_a_task_and_masks_does_not_busy_wake() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    // The ISR resumes a suspended high-priority task, requests a yield and
    // leaves interrupts disabled: the task stays held off (a switch is
    // needed), and the machine must not wake every microsecond for it.
    let o = run(
        costar_test_isr_masks_in_scheduler_boot,
        false,
        || unsafe { sim_ffi::device_ffi::sim_irq_raise(6) },
        100_000,
    );
    assert_eq!(o.times("resume_isr"), vec![0]);
    assert!(o.times("high_resumed").is_empty());
    assert!(
        o.steps <= 3,
        "busy-woke for a held-off task: {} steps",
        o.steps
    );
}

/// No busy wake, as a property: whatever the firmware leaves pending, the
/// World steps a FreeRTOS machine at most a few times per firmware tick.
#[test]
fn no_scenario_wakes_the_machine_more_than_a_few_times_per_tick() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    type After = fn();
    fn raise() {
        unsafe { sim_ffi::device_ffi::sim_irq_raise(6) };
    }
    fn mask_and_raise() {
        unsafe {
            sim_ffi::freertos::sim_disable_interrupts();
            sim_ffi::device_ffi::sim_irq_raise(6);
        }
    }
    fn arm_now() {
        unsafe { sim_ffi::device_ffi::sim_timer_arm(0, 0) };
    }
    fn mask_and_arm_now() {
        unsafe {
            sim_ffi::freertos::sim_disable_interrupts();
            sim_ffi::device_ffi::sim_timer_arm(0, 0);
        }
    }
    fn nothing() {}
    let scenarios: [(&str, unsafe extern "C" fn(), bool, After); 12] = [
        (
            "isr schedules callback",
            costar_test_isr_schedules_event_boot,
            false,
            raise,
        ),
        (
            "callback storm",
            costar_test_callback_storm_boot,
            false,
            nothing,
        ),
        ("timer storm", costar_test_timer_storm_boot, true, nothing),
        ("irq yield", costar_test_external_irq_boot, false, raise),
        (
            "irq ready only",
            costar_test_external_irq_no_yield_boot,
            false,
            raise,
        ),
        (
            "masked irq",
            costar_test_external_irq_boot,
            false,
            mask_and_raise,
        ),
        (
            "masked ready task",
            costar_test_isr_masks_in_scheduler_boot,
            false,
            raise,
        ),
        (
            "owed tick + yield",
            costar_test_entry_isr_boot,
            false,
            raise,
        ),
        ("timer due", costar_test_external_irq_boot, true, arm_now),
        (
            "masked timer due",
            costar_test_external_irq_boot,
            true,
            mask_and_arm_now,
        ),
        (
            "ended + timer",
            costar_test_arm_then_end_boot,
            true,
            nothing,
        ),
        ("idle", costar_test_external_irq_boot, false, nothing),
    ];
    const US: Tick = 100_000; // 100 firmware ticks
    for (name, boot, timer, after) in scenarios {
        let o = run(boot, timer, after, US);
        assert!(
            o.steps <= 3 * (US / 1_000) as u32 + 10,
            "{name}: {} firmware steps in {US} us",
            o.steps
        );
    }
}

#[test]
fn timer_storm_neither_hangs_nor_busy_wakes_the_world() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    // An ISR re-arms its timer with zero delay from tick 1 on.
    let o = run(costar_test_timer_storm_boot, true, || {}, 10_000);
    assert_eq!(o.times("slept_through_storm"), vec![3_000]);
    assert!(o.times("irq_storm").contains(&1_000));
    assert!(o.steps <= 40, "{} firmware steps in 10 ticks", o.steps);
}

#[test]
fn callback_scheduled_by_an_isr_after_the_scheduler_still_runs() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let o = run(
        costar_test_isr_schedules_event_boot,
        false,
        || unsafe { sim_ffi::device_ffi::sim_irq_raise(6) },
        10_000,
    );
    assert_eq!(o.times("peripheral_callback"), vec![5_000]);
}

#[test]
fn callback_storm_neither_hangs_nor_busy_wakes_the_world() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let o = run(costar_test_callback_storm_boot, false, || {}, 10_000);
    assert_eq!(o.times("slept_through_storm"), vec![3_000]);
    assert!(o.times("irq_storm").contains(&1_000));
    assert!(o.steps <= 40, "{} firmware steps in 10 ticks", o.steps);
}

#[test]
fn self_retriggering_irq_runs_to_completion_in_a_world() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    let o = run(
        costar_test_retrigger_boot,
        false,
        || unsafe { sim_ffi::device_ffi::sim_irq_raise(6) },
        20_000,
    );
    assert_eq!(unsafe { costar_test_retrigger_count() }, 50_000);
    assert!(o.steps <= 400, "{} firmware steps", o.steps);
}

unsafe extern "C" fn traced_callback() {
    sim_ffi::sim_trace_u32(c"owed_callback".as_ptr(), 1);
}

#[test]
fn callback_due_while_a_budget_tick_is_owed_runs_on_time() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    // The spinner uses up its budget at tick 0 (a tick is owed); after the
    // scheduler ran, firmware schedules a callback for tick 0.  Callbacks
    // run at their deadline: the machine is woken within tick 0 for it,
    // and that step runs it without resuming the spinner.
    let o = run(
        costar_test_entry_isr_boot,
        false,
        || unsafe { sim_ffi::sim_schedule_event(0, Some(traced_callback)) },
        2_000,
    );
    assert_eq!(o.times("owed_callback"), vec![0]);
    assert!(
        o.steps <= 6,
        "busy-woke during the owed tick: {} steps",
        o.steps
    );
}

/// No busy wake with a budget tick owed, crossed with every wake source:
/// the spinner of `costar_test_entry_isr_boot` owes tick 0 when each
/// source is added after the scheduler ran.
#[test]
fn no_wake_source_busy_wakes_while_a_budget_tick_is_owed() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    type After = fn();
    fn raise() {
        unsafe { sim_ffi::device_ffi::sim_irq_raise(6) };
    }
    fn mask_and_raise() {
        unsafe {
            sim_ffi::freertos::sim_disable_interrupts();
            sim_ffi::device_ffi::sim_irq_raise(6);
        }
    }
    fn arm_now() {
        unsafe { sim_ffi::device_ffi::sim_timer_arm(0, 0) };
    }
    fn mask_and_arm_now() {
        unsafe {
            sim_ffi::freertos::sim_disable_interrupts();
            sim_ffi::device_ffi::sim_timer_arm(0, 0);
        }
    }
    fn callback_now() {
        unsafe { sim_ffi::sim_schedule_event(0, Some(traced_callback)) };
    }
    fn new_task() {
        sim_ffi::spawn_rust_task("late", 3, 4096, |_| {});
    }
    fn nothing() {}
    let sources: [(&str, After); 7] = [
        ("irq", raise),
        ("masked irq", mask_and_raise),
        ("timer due", arm_now),
        ("masked timer due", mask_and_arm_now),
        ("callback due", callback_now),
        ("new task", new_task),
        ("nothing", nothing),
    ];
    const US: Tick = 20_000; // 20 firmware ticks
    for (name, after) in sources {
        let o = run(costar_test_entry_isr_boot, true, after, US);
        assert!(
            o.steps <= 3 * (US / 1_000) as u32 + 10,
            "owed tick + {name}: {} firmware steps in {US} us",
            o.steps
        );
    }
}
