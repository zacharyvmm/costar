//! A machine's firmware deadlines run at their absolute World time, on
//! every backend: the World converts each firmware tick through the one
//! clock anchor fixed at the machine's first step, however often other
//! World events step it in between.
//!
//! A callback due at firmware tick 5 schedules another for tick 6: they
//! run at World 5000 µs and 6000 µs (and read ticks 5 and 6) on a native
//! machine, a Zephyr-step machine and a FreeRTOS machine, with and without
//! an unrelated World event at 100 µs.

use std::cell::{Cell, RefCell};

use sim_core::Tick;
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use sim_world::world::World;

extern "C" {
    fn costar_test_spawn_task(name: *const std::ffi::c_char, body: extern "C" fn(), priority: u32);
}

thread_local! {
    /// The World instant being stepped.
    static WORLD_NOW: Cell<Tick> = const { Cell::new(0) };
    /// `(World µs, firmware tick)` of each callback.
    static RAN: RefCell<Vec<(Tick, Tick)>> = const { RefCell::new(Vec::new()) };
}

fn record() {
    let at = WORLD_NOW.with(Cell::get);
    RAN.with(|r| {
        r.borrow_mut()
            .push((at, unsafe { sim_ffi::sim_now_ticks() }))
    });
}

unsafe extern "C" fn second() {
    record();
}

unsafe extern "C" fn first() {
    record();
    sim_ffi::sim_schedule_event(6, Some(second));
}

extern "C" fn idle_task() {
    loop {
        unsafe { sim_ffi::sim_task_delay_until(sim_ffi::sim_now_ticks() + 1_000) };
    }
}

#[derive(Clone, Copy, Debug)]
enum Backend {
    Native,
    ZephyrStep,
    FreeRtos,
}

struct Fw(Backend);

impl Firmware for Fw {
    fn init(&mut self, machine: &mut Machine) {
        let _active = machine.activate();
        if let Backend::FreeRtos = self.0 {
            unsafe { costar_test_spawn_task(c"idle".as_ptr(), idle_task, 1) };
        }
        unsafe { sim_ffi::sim_schedule_event(5, Some(first)) };
    }

    fn step(&mut self, _now: Tick, machine: &mut Machine) {
        let _active = machine.activate();
        unsafe {
            match self.0 {
                Backend::ZephyrStep => sim_ffi::zephyr_ffi::sim_zephyr_scheduler_tick(),
                _ => sim_ffi::sim_scheduler_tick(),
            };
        }
    }
}

fn run(backend: Backend, unrelated_event: bool) -> Vec<(Tick, Tick)> {
    std::thread::spawn(move || {
        let mut world = World::new();
        world.enable_owned_device_banks();
        let mut machine = Machine::with_defaults(1, "m");
        machine.schedule_at(0, 0, "boot", Box::new(|_| {}));
        world.add_machine(machine);
        world
            .machine_mut(1)
            .unwrap()
            .load_firmware(Box::new(Fw(backend)));
        if unrelated_event {
            let mut other = Machine::with_defaults(2, "other");
            other.schedule_at(100, 0, "unrelated", Box::new(|_| {}));
            world.add_machine(other);
        }
        // Step the World one instant at a time, noting the instant each
        // step runs at: the World time callbacks really run at (some run
        // as the step brings the kernel up to its time, before
        // `Firmware::step`).
        while let Some(at) = world.next_global_event_time().filter(|&at| at <= 20_000) {
            WORLD_NOW.with(|w| w.set(at));
            world.step().unwrap();
        }
        RAN.with(|r| r.borrow().clone())
    })
    .join()
    .unwrap()
}

#[test]
fn callbacks_run_at_their_absolute_world_time_on_every_backend() {
    for backend in [Backend::Native, Backend::ZephyrStep, Backend::FreeRtos] {
        for unrelated_event in [false, true] {
            assert_eq!(
                run(backend, unrelated_event),
                vec![(5_000, 5), (6_000, 6)],
                "{backend:?} unrelated_event={unrelated_event}"
            );
        }
    }
}
