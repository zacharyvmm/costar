//! Host input applied in `Firmware::step` *before* the scheduler runs acts
//! at the step's World time: the kernel is brought up to it first, so a
//! task the input readies runs at that time and times its delays from it.

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
    fn vTaskDelay(ticks: u32);
    fn xTaskGetTickCount() -> u32;
}
unsafe extern "C" fn worker(_: *mut c_void) {
    vTaskSuspend(std::ptr::null_mut());
    sim_ffi::sim_trace_u32(c"external_wake".as_ptr(), xTaskGetTickCount());
    vTaskDelay(5);
    sim_ffi::sim_trace_u32(c"after_delay".as_ptr(), xTaskGetTickCount());
}
struct Fw {
    handle: *mut c_void,
    resumed: bool,
}
impl Firmware for Fw {
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
    fn step(&mut self, now: Tick, m: &mut Machine) {
        let _a = m.activate();
        if now >= 10_000 && !self.resumed {
            self.resumed = true;
            unsafe { vTaskResume(self.handle) };
        }
        unsafe { sim_ffi::sim_scheduler_tick() };
    }
}
#[test]
fn external_input_runs_at_its_world_time() {
    let mut w = World::new();
    w.enable_owned_device_banks();
    let mut m = Machine::with_defaults(1, "ecu");
    m.schedule_at(0, 0, "boot", Box::new(|_| {}));
    m.schedule_at(10_000, 0, "input", Box::new(|_| {}));
    m.load_firmware(Box::new(Fw {
        handle: std::ptr::null_mut(),
        resumed: false,
    }));
    w.add_machine(m);
    w.run_until(20_000).unwrap();
    let records: Vec<_> = w
        .drain_all_traces()
        .into_iter()
        .filter(|l| l.contains("external_wake") || l.contains("after_delay"))
        .collect();
    assert!(
        records
            .iter()
            .any(|l| l.contains("10000") && l.contains("external_wake") && l.ends_with(" 10")),
        "external wake must run at tick 10"
    );
    assert!(
        records
            .iter()
            .any(|l| l.contains("15000") && l.contains("after_delay") && l.ends_with(" 15")),
        "relative delay must end at tick 15"
    );
}

extern "C" {
    fn costar_test_ready_wake_boot();
    fn costar_test_ready_wake_give();
}

struct GiveAtTenMs {
    given: bool,
}

impl Firmware for GiveAtTenMs {
    fn init(&mut self, m: &mut Machine) {
        let _a = m.activate();
        unsafe { costar_test_ready_wake_boot() };
    }
    fn step(&mut self, now: Tick, m: &mut Machine) {
        let _a = m.activate();
        if now >= 10_000 && !self.given {
            self.given = true;
            unsafe { costar_test_ready_wake_give() };
        }
        unsafe { sim_ffi::sim_scheduler_tick() };
        sim_ffi::flush_trace();
    }
}

#[test]
fn semaphore_given_before_the_scheduler_wakes_its_task_at_that_time() {
    let mut w = World::new();
    w.enable_owned_device_banks();
    let mut m = Machine::with_defaults(1, "ecu");
    m.schedule_at(0, 0, "boot", Box::new(|_| {}));
    m.schedule_at(10_000, 0, "input", Box::new(|_| {}));
    m.load_firmware(Box::new(GiveAtTenMs { given: false }));
    w.add_machine(m);
    w.run_until(20_000).unwrap();
    let lines: Vec<_> = w
        .drain_all_traces()
        .into_iter()
        .filter(|l| l.contains("\"readied_by_give\""))
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].split_whitespace().nth(1),
        Some("10000"),
        "{lines:?}"
    );
}
