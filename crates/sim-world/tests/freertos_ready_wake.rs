//! A task readied between scheduling steps — by `vTaskResume()`, a task
//! notification or a semaphore given from `Firmware::step` after the
//! scheduler went quiescent — still runs: the World steps the machine again.

use sim_core::Tick;
use sim_world::{firmware::Firmware, machine::Machine, world::World};
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
    fn vTaskSuspend(handle: *mut c_void);
    fn vTaskResume(handle: *mut c_void);
}
unsafe extern "C" fn worker(_: *mut c_void) {
    vTaskSuspend(std::ptr::null_mut());
    sim_ffi::sim_trace_u32(c"resumed_worker".as_ptr(), 1);
}
struct ResumingFirmware {
    handle: *mut c_void,
    resumed: bool,
}
impl Firmware for ResumingFirmware {
    fn init(&mut self, m: &mut Machine) {
        let _a = m.activate();
        assert_eq!(
            unsafe {
                xTaskCreate(
                    worker,
                    c"worker".as_ptr(),
                    128,
                    std::ptr::null_mut(),
                    5,
                    &mut self.handle,
                )
            },
            1
        );
    }
    fn step(&mut self, _: Tick, m: &mut Machine) {
        let _a = m.activate();
        let more = unsafe { sim_ffi::sim_scheduler_tick() };
        if more == 0 && !self.resumed {
            self.resumed = true;
            unsafe {
                vTaskResume(self.handle);
            }
        }
    }
}
#[test]
fn resuming_a_quiescent_task_keeps_world_running() {
    let mut w = World::new();
    let mut m = Machine::with_defaults(1, "ecu");
    w.enable_owned_device_banks();
    m.schedule_at(0, 0, "boot", Box::new(|_| {}));
    m.load_firmware(Box::new(ResumingFirmware {
        handle: std::ptr::null_mut(),
        resumed: false,
    }));
    w.add_machine(m);
    w.run_until(20_000).unwrap();
    let logs: Vec<_> = w
        .drain_all_traces()
        .into_iter()
        .filter(|l| l.contains("resumed_worker"))
        .collect();
    eprintln!("logs={logs:?}, next event={:?}", w.next_global_event_time());
    assert_eq!(logs.len(), 1, "World dropped a task readied by vTaskResume");
}

extern "C" {
    fn costar_test_ready_wake_boot();
    fn costar_test_ready_wake_resume();
    fn costar_test_ready_wake_notify();
    fn costar_test_ready_wake_give();
}

/// The ready-wake fixture's tasks are C statics.
static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct ReadyingFirmware {
    trigger: unsafe extern "C" fn(),
    done: bool,
}

impl Firmware for ReadyingFirmware {
    fn init(&mut self, m: &mut Machine) {
        let _a = m.activate();
        unsafe { costar_test_ready_wake_boot() };
    }
    fn step(&mut self, _: Tick, m: &mut Machine) {
        let _a = m.activate();
        let more = unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::flush_trace();
        if more == 0 && !self.done {
            self.done = true;
            unsafe { (self.trigger)() };
        }
    }
}

#[test]
fn tasks_readied_after_a_quiescent_step_run() {
    let _f = FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
    for (trigger, label) in [
        (
            costar_test_ready_wake_resume as unsafe extern "C" fn(),
            "readied_by_resume",
        ),
        (costar_test_ready_wake_notify, "readied_by_notify"),
        (costar_test_ready_wake_give, "readied_by_give"),
    ] {
        let mut w = World::new();
        w.enable_owned_device_banks();
        let mut m = Machine::with_defaults(1, "ecu");
        m.schedule_at(0, 0, "boot", Box::new(|_| {}));
        w.add_machine(m);
        w.machine_mut(1)
            .unwrap()
            .load_firmware(Box::new(ReadyingFirmware {
                trigger,
                done: false,
            }));
        w.run_until(20_000).unwrap();
        let ran = w
            .drain_all_traces()
            .into_iter()
            .filter(|l| l.contains(&format!("\"{label}\"")))
            .count();
        assert_eq!(ran, 1, "{label}: the readied task never ran");
    }
}
