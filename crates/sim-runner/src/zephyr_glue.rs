//! Zephyr native_sim runner shim — provides nct_*, nce_*, hw_* symbols.
//!
//! All Zephyr threads run on a single corosensei fiber, switching
//! cooperatively via arch_swap → nct_swap_threads → corosensei yield.
//! Each Zephyr thread's entry is called on this shared stack, matching
//! the single-threaded cooperative FreeRTOS port model.
//!
//! Multi-fiber (one fiber per Zephyr thread) is future work tracked as
//! a known limitation in IMPLEMENTATION_STATUS.md §Phase 16.

use std::ffi::c_int;
use std::sync::atomic::{AtomicU64, Ordering};

use sim_fiber::YieldReason;

// ═══════════════════════════════════════════════════════════════════
// NCT — Thread payloads only (no per-thread fibers in single-fiber model)
// ═══════════════════════════════════════════════════════════════════

const MAX_THREADS: usize = 32;

struct NctState {
    payloads: [*mut std::ffi::c_void; MAX_THREADS],
    count: i32,
    entry_fn: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
}

static mut NCT: NctState = NctState {
    payloads: [std::ptr::null_mut(); MAX_THREADS],
    count: 0,
    entry_fn: None,
};

fn nct_mut() -> &'static mut NctState {
    unsafe { &mut *core::ptr::addr_of_mut!(NCT) }
}

#[no_mangle]
pub unsafe extern "C" fn nct_init(
    fptr: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
) -> *mut std::ffi::c_void {
    nct_mut().entry_fn = fptr;
    nct_mut().count = 0;
    1 as *mut std::ffi::c_void
}

#[no_mangle]
pub unsafe extern "C" fn nct_clean_up(_state: *mut std::ffi::c_void) {}

#[no_mangle]
pub unsafe extern "C" fn nct_new_thread(
    _state: *mut std::ffi::c_void,
    payload: *mut std::ffi::c_void,
) -> c_int {
    let s = nct_mut();
    let idx = s.count;
    if idx as usize >= MAX_THREADS {
        return -1;
    }
    s.count += 1;
    s.payloads[idx as usize] = payload;
    idx
}

#[no_mangle]
pub unsafe extern "C" fn nct_swap_threads(_state: *mut std::ffi::c_void, _next_id: c_int) {
    // Yield the single fiber.  Zephyr has already set _current to the
    // next thread via z_current_thread_set in arch_swap.  When the
    // Rust drain loop resumes this fiber, the new thread is running.
    sim_fiber::suspend_active_fiber(YieldReason::RtosPortYield);
}

#[no_mangle]
pub unsafe extern "C" fn nct_first_thread_start(_state: *mut std::ffi::c_void, next_id: c_int) {
    // Run the first thread's entry function on the boot fiber's stack.
    // All subsequent threads also run on this same fiber, switchable
    // cooperatively via nct_swap_threads.
    let entry_fn = nct_mut().entry_fn;
    let payload = nct_mut().payloads[next_id as usize];
    if let Some(entry) = entry_fn {
        // Calls posix_arch_thread_entry(payload) → z_thread_entry(...)
        // This enters Zephyr's main thread and never returns.
        unsafe { entry(payload) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn nct_abort_thread(_state: *mut std::ffi::c_void, _t: c_int) {}
#[no_mangle]
pub unsafe extern "C" fn nct_get_unique_thread_id(_s: *mut std::ffi::c_void, t: c_int) -> c_int {
    t + 1
}
#[no_mangle]
pub unsafe extern "C" fn nct_thread_name_set(
    _s: *mut std::ffi::c_void,
    _t: c_int,
    _n: *const std::ffi::c_char,
) -> c_int {
    0
}

// ═══════════════════════════════════════════════════════════════════
// NCE — CPU Emulator
// ═══════════════════════════════════════════════════════════════════

static NCE_RUNNING: AtomicU64 = AtomicU64::new(0);
#[no_mangle]
pub unsafe extern "C" fn nce_init() -> *mut std::ffi::c_void {
    1 as *mut std::ffi::c_void
}
#[no_mangle]
pub unsafe extern "C" fn nce_terminate(_: *mut std::ffi::c_void) {}
#[no_mangle]
pub unsafe extern "C" fn nce_boot_cpu(_: *mut std::ffi::c_void, r: Option<unsafe extern "C" fn()>) {
    NCE_RUNNING.store(1, Ordering::Relaxed);
    if let Some(f) = r {
        unsafe { f() };
    }
}
#[no_mangle]
pub unsafe extern "C" fn nce_halt_cpu(_: *mut std::ffi::c_void) {
    NCE_RUNNING.store(0, Ordering::Relaxed);
    sim_fiber::suspend_active_fiber(YieldReason::Cooperative);
}
#[no_mangle]
pub unsafe extern "C" fn nce_wake_cpu(_: *mut std::ffi::c_void) {
    NCE_RUNNING.store(1, Ordering::Relaxed);
}
#[no_mangle]
pub unsafe extern "C" fn nce_is_cpu_running(_: *mut std::ffi::c_void) -> c_int {
    NCE_RUNNING.load(Ordering::Relaxed) as c_int
}

// ═══════════════════════════════════════════════════════════════════
// HW models — stubs for hello_world
// ═══════════════════════════════════════════════════════════════════

macro_rules! stub {
    ($name:ident, $($p:ident : $t:ty),*) => {
        #[no_mangle] pub unsafe extern "C" fn $name($($p: $t),*) { $(let _ = $p;)* }
    };
}

#[no_mangle]
pub unsafe extern "C" fn hw_irq_ctrl_get_cur_prio() -> c_int {
    256
}
#[no_mangle]
pub unsafe extern "C" fn hw_irq_ctrl_get_current_lock() -> u32 {
    0
}
#[no_mangle]
pub unsafe extern "C" fn hw_irq_ctrl_get_highest_prio_irq() -> c_int {
    -1
}
#[no_mangle]
pub unsafe extern "C" fn hw_irq_ctrl_get_prio(_: u32) -> u8 {
    0
}
#[no_mangle]
pub unsafe extern "C" fn hw_irq_ctrl_is_irq_enabled(_: u32) -> i32 {
    0
}
#[no_mangle]
pub unsafe extern "C" fn hw_irq_ctrl_change_lock(new_lock: u32) -> u32 {
    new_lock
}
#[no_mangle]
pub unsafe extern "C" fn hw_irq_ctrl_get_irq_status() -> u64 {
    0
}

stub!(hw_irq_ctrl_clear_irq, irq: u32);
stub!(hw_irq_ctrl_disable_irq, irq: u32);
stub!(hw_irq_ctrl_enable_irq, irq: u32);
stub!(hw_irq_ctrl_set_irq, irq: u32);
stub!(hw_irq_ctrl_raise_im, irq: u32);
stub!(hw_irq_ctrl_prio_set, irq: u32, prio: u32);
stub!(hw_irq_ctrl_raise_im_from_sw, irq: u32);
stub!(hw_irq_ctrl_set_cur_prio, prio: c_int);
stub!(hwtimer_enable, period: u64);
stub!(hwtimer_timer_reached,);
stub!(hwtimer_set_real_time_mode, mode: bool);
stub!(hwtimer_set_silent_ticks, silent: i64);
stub!(hwtimer_wake_in_time, time: u64);
stub!(hwtimer_reset_rtc,);
stub!(hwtimer_set_rtc_offset, offset: i64);
stub!(hwtimer_set_rt_ratio, ratio: f64);
stub!(hwtimer_adjust_rtc_offset, offset_delta: i64);
stub!(hwtimer_adjust_rt_ratio, ratio_correction: f64);

#[no_mangle]
pub unsafe extern "C" fn hwtimer_get_pending_silent_ticks() -> i64 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn hwtimer_get_simu_rtc_time() -> i64 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn hwtimer_get_pseudohost_rtc_time(nsec: *mut u32, sec: *mut u64) {
    if !nsec.is_null() {
        unsafe {
            *nsec = 0;
        }
    }
    if !sec.is_null() {
        unsafe {
            *sec = 0;
        }
    }
}
