//! A virtual timer that expires while a machine idles masked still fires
//! at its expiry and latches its IRQ; the ISR runs at the unmask.
//!
//! The machine is idle at tick 0 with interrupts masked (by host code).  A
//! one-shot timer expires at tick 5, a peripheral callback disarms it at 6,
//! and another unmasks at 10.  The masked idle paths used to jump straight
//! to the callback at 6, skipping the timer's deadline: disarming then
//! dropped the expiry and no ISR ever ran.  Covered on FreeRTOS
//! (standalone and bounded stepping) and the native scheduler (bounded and
//! unbounded).

use std::cell::Cell;
use std::ffi::{c_char, c_long, c_ulong, c_void};

use sim_core::SimConfig;
use sim_ffi::simulator::Simulator;

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
}

const TIMER_IRQ: u32 = 48;

thread_local! {
    static ISR_AT: Cell<Option<u64>> = const { Cell::new(None) };
    static ISRS: Cell<u32> = const { Cell::new(0) };
    static FIRED_AT_DISARM: Cell<Option<u64>> = const { Cell::new(None) };
}

unsafe extern "C" fn anchor(_: *mut c_void) {
    loop {
        vTaskDelay(1000);
    }
}

unsafe extern "C" fn timer_isr() {
    ISRS.with(|c| c.set(c.get() + 1));
    ISR_AT.with(|t| t.set(Some(sim_ffi::sim_now_ticks())));
}

unsafe extern "C" fn disarm() {
    let fired = sim_devices::with_timer(0, |t| t.fire_count).unwrap();
    FIRED_AT_DISARM.with(|f| f.set(Some(fired)));
    sim_ffi::device_ffi::sim_timer_disarm(0);
}

unsafe extern "C" fn unmask() {
    sim_ffi::freertos::sim_enable_interrupts();
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    FreeRtosStandalone,
    FreeRtosBounded,
    NativeBounded,
    NativeUnbounded,
}

/// `(timer fire count when the callback disarmed it, ISRs run, tick of the
/// ISR)`.
fn run(mode: Mode) -> (Option<u64>, u32, Option<u64>) {
    std::thread::spawn(move || {
        let mut sim = Simulator::new(SimConfig::default());
        sim.enable_owned_devices();
        let _active = sim.activate();
        let freertos = matches!(mode, Mode::FreeRtosStandalone | Mode::FreeRtosBounded);
        if freertos {
            let created = unsafe {
                xTaskCreate(
                    anchor,
                    c"anchor".as_ptr(),
                    128,
                    std::ptr::null_mut(),
                    1,
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(created, 1);
        }
        unsafe { sim_ffi::device_ffi::sim_irq_set_handler(TIMER_IRQ, Some(timer_isr)) };
        sim.set_scheduler_limit(Some(0));
        unsafe { sim_ffi::sim_scheduler_tick() };
        // Idle at tick 0; host code masks interrupts.
        sim_ffi::freertos::sim_disable_interrupts();
        sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(0, TIMER_IRQ));
        unsafe {
            sim_ffi::device_ffi::sim_timer_arm(0, 5);
            sim_ffi::sim_schedule_event(6, Some(disarm));
            sim_ffi::sim_schedule_event(10, Some(unmask));
        }
        let bounded = matches!(mode, Mode::FreeRtosBounded | Mode::NativeBounded);
        for step in 1..=60u64 {
            sim.set_scheduler_limit(bounded.then_some(step.min(20)));
            let more = unsafe { sim_ffi::sim_scheduler_tick() };
            if !bounded && more == 0 {
                break;
            }
        }
        (
            FIRED_AT_DISARM.with(Cell::get),
            ISRS.with(Cell::get),
            ISR_AT.with(Cell::get),
        )
    })
    .join()
    .unwrap()
}

#[test]
fn a_timer_expiring_while_masked_fires_and_its_isr_runs_at_the_unmask() {
    for mode in [
        Mode::FreeRtosStandalone,
        Mode::FreeRtosBounded,
        Mode::NativeBounded,
        Mode::NativeUnbounded,
    ] {
        assert_eq!(
            run(mode),
            (Some(1), 1, Some(10)),
            "{mode:?}: (fired when disarmed, ISRs, ISR tick)"
        );
    }
}
