//! The legacy creation pattern (`xTaskCreate()` + `sim_create_task()` for
//! one task) collapses into one task only when both calls describe the
//! same task: entry point, parameter, name (as far as FreeRTOS keeps it)
//! and priority.  Independent tasks that merely share an entry point and
//! parameter both run, in either creation order.

use std::cell::Cell;
use std::ffi::{c_char, c_long, c_ulong, c_void, CStr};

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
}

thread_local! {
    static RUNS: Cell<u32> = const { Cell::new(0) };
}

unsafe extern "C" fn worker(_: *mut c_void) {
    RUNS.with(|r| r.set(r.get() + 1));
}

#[derive(Clone, Copy, Debug)]
enum Order {
    /// `xTaskCreate()` first, then `sim_create_task()`.
    FreeRtosFirst,
    /// `sim_create_task()` first, then `xTaskCreate()`.
    SimFirst,
}

/// Creates `worker` (NULL parameter) through both APIs and returns how
/// often it ran.
fn runs(order: Order, sim: (&CStr, u32), freertos: (&CStr, u32)) -> u32 {
    RUNS.with(|r| r.set(0));
    let mut simulator = Simulator::new(SimConfig::default());
    let _active = simulator.activate();
    let create_freertos = || {
        let created = unsafe {
            xTaskCreate(
                worker,
                freertos.0.as_ptr(),
                256,
                std::ptr::null_mut(),
                c_ulong::from(freertos.1),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(created, 1);
    };
    let create_sim = || unsafe {
        sim_ffi::sim_create_task(
            sim.0.as_ptr(),
            Some(worker),
            std::ptr::null_mut(),
            256,
            sim.1,
        );
    };
    match order {
        Order::FreeRtosFirst => {
            create_freertos();
            create_sim();
        }
        Order::SimFirst => {
            create_sim();
            create_freertos();
        }
    }
    simulator.set_scheduler_limit(Some(5));
    unsafe { sim_ffi::sim_scheduler_tick() };
    RUNS.with(Cell::get)
}

const ORDERS: [Order; 2] = [Order::FreeRtosFirst, Order::SimFirst];

#[test]
fn independent_tasks_sharing_an_entry_point_both_run() {
    for order in ORDERS {
        // Different names and priorities (the reviewer's case).
        assert_eq!(
            runs(order, (c"independent_a", 3), (c"independent_b", 4)),
            2,
            "{order:?}: names and priorities differ"
        );
        // Different names only.
        assert_eq!(
            runs(order, (c"independent_a", 3), (c"independent_b", 3)),
            2,
            "{order:?}: names differ"
        );
        // Different priorities only.
        assert_eq!(
            runs(order, (c"worker", 3), (c"worker", 4)),
            2,
            "{order:?}: priorities differ"
        );
    }
}

#[test]
fn a_genuine_legacy_pair_is_one_task() {
    for order in ORDERS {
        assert_eq!(runs(order, (c"legacy", 3), (c"legacy", 3)), 1, "{order:?}");
        // FreeRTOS keeps configMAX_TASK_NAME_LEN - 1 = 15 bytes of a long
        // name; the pair still matches.
        assert_eq!(
            runs(
                order,
                (c"a_rather_long_task_name", 2),
                (c"a_rather_long_task_name", 2)
            ),
            1,
            "{order:?}: long name"
        );
    }
}

/// A pair is matched on the name bytes FreeRTOS keeps, whether or not
/// they are valid UTF-8: identity never goes through the display name.
#[test]
fn names_are_compared_as_bytes() {
    let invalid_a = c"task\xff";
    let invalid_b = c"task\xfe";
    for order in ORDERS {
        // FreeRTOS keeps 15 bytes, cutting the two-byte `é` in half.
        assert_eq!(
            runs(
                order,
                (c"aaaaaaaaaaaaaa\u{e9}", 1),
                (c"aaaaaaaaaaaaaa\u{e9}", 1)
            ),
            1,
            "{order:?}: name truncated inside a character"
        );
        assert_eq!(
            runs(order, (invalid_a, 1), (invalid_a, 1)),
            1,
            "{order:?}: same invalid UTF-8 name"
        );
        // Two invalid names are still two names.
        assert_eq!(
            runs(order, (invalid_a, 1), (invalid_b, 1)),
            2,
            "{order:?}: different invalid UTF-8 names"
        );
    }
}
