//! Zephyr native_sim runner shim — provides nct_*, nce_*, hw_* symbols.
//!
//! Multi-fiber model: each Zephyr thread gets its own corosensei fiber.
//! When Zephyr's scheduler switches threads via arch_swap →
//! nct_swap_threads, the current fiber yields and the drain loop resumes
//! the next thread's fiber on its own stack.
//!
//! Fibers are stored in a heap-allocated Vec managed via raw pointer
//! inside NctState.  The drain loop uses Option::take() to extract a
//! fiber before resuming it, avoiding re-entrant borrow issues when
//! the fiber calls back into nct_swap_threads.

use std::ffi::c_int;
use std::sync::atomic::{AtomicU64, Ordering};

use sim_fiber::yield_reason::YieldReason;
use sim_fiber::Fiber;

// ═══════════════════════════════════════════════════════════════════
// NCT — Multi-fiber thread emulation
// ═══════════════════════════════════════════════════════════════════

const MAX_THREADS: usize = 32;

/// Per-thread metadata stored alongside the fiber.
#[allow(dead_code)]
struct ThreadInfo {
    /// The Zephyr thread payload pointer (posix_thread_status_t).
    payload: *mut std::ffi::c_void,
    /// Fiber ID (index into the fibers Vec).
    fiber_idx: usize,
}

struct NctState {
    /// Heap-allocated vector of fibers, one per Zephyr thread.
    /// Created in nct_init, never freed (lives for simulation duration).
    fibers: *mut Vec<Option<Fiber>>,
    /// Per-thread metadata (payload pointer, fiber index).
    infos: [Option<ThreadInfo>; MAX_THREADS],
    /// Thread count.
    count: i32,
    /// The entry wrapper (posix_arch_thread_entry).
    entry_fn: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    /// Which thread the drain loop should resume next.
    /// Set by nct_first_thread_start and nct_swap_threads before yielding.
    next_to_resume: i32,
    /// The last yield reason from the most recent fiber yield.
    last_yield: Option<YieldReason>,
}

// Safety: all access is single-threaded (corosensei cooperative model).
// References into NctState must not be held across fiber resume/yield.
static mut NCT: NctState = NctState {
    fibers: std::ptr::null_mut(),
    infos: [const { None }; MAX_THREADS],
    count: 0,
    entry_fn: None,
    next_to_resume: -1,
    last_yield: None,
};

fn nct_mut() -> &'static mut NctState {
    unsafe { &mut *core::ptr::addr_of_mut!(NCT) }
}

/// SAFETY: must not hold any reference into NCT across this call.
fn nct_fibers_mut() -> &'static mut Vec<Option<Fiber>> {
    unsafe { &mut *nct_mut().fibers }
}

#[no_mangle]
pub unsafe extern "C" fn nct_init(
    fptr: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
) -> *mut std::ffi::c_void {
    let s = nct_mut();
    s.entry_fn = fptr;
    s.count = 0;
    s.next_to_resume = -1;
    s.last_yield = None;
    // Allocate the fiber vector on the heap (lives for simulation duration).
    let fibers = Box::new(Vec::with_capacity(MAX_THREADS));
    s.fibers = Box::into_raw(fibers);
    1 as *mut std::ffi::c_void
}

#[no_mangle]
pub unsafe extern "C" fn nct_clean_up(_state: *mut std::ffi::c_void) {
    // Fibers are dropped when the Box is dropped at process exit.
}

/// Create a new Zephyr thread backed by a corosensei fiber.
///
/// The fiber body calls posix_arch_thread_entry(payload), which invokes
/// z_thread_entry(entry, arg1, arg2, arg3) — Zephyr's standard thread
/// entry wrapper.  After the entry function returns, the fiber yields
/// with TaskExit.
#[no_mangle]
pub unsafe extern "C" fn nct_new_thread(
    _state: *mut std::ffi::c_void,
    payload: *mut std::ffi::c_void,
) -> c_int {
    let entry_fn = nct_mut()
        .entry_fn
        .expect("nct_init must be called before nct_new_thread");

    let s = nct_mut();
    let idx = s.count;
    if idx as usize >= MAX_THREADS {
        return -1;
    }

    // Create the fiber.
    let fiber = Fiber::new(
        (idx + 1) as u64,                    // task ID
        "zephyr_thread",                     // name (generic; Zephyr names tracked separately)
        0,                                   // priority (Zephyr manages its own scheduling)
        4096,                                // requested_stack_words (placeholder)
        sim_fiber::MIN_HOST_COROUTINE_STACK, // host stack
        idx as u64,                          // creation_seq
        move |_reason| {
            // Run Zephyr's thread entry wrapper on this fiber's stack.
            unsafe { entry_fn(payload) };
            // After the entry function returns, signal task exit.
            sim_fiber::suspend_active_fiber(YieldReason::TaskExit);
        },
    );

    let fibers = nct_fibers_mut();
    let fiber_idx = fibers.len();
    fibers.push(Some(fiber));

    s.infos[idx as usize] = Some(ThreadInfo { payload, fiber_idx });

    s.count += 1;
    idx
}

/// Start the first Zephyr thread.
///
/// In the multi-fiber model, this yields the boot fiber and signals
/// the drain loop to resume the fiber for thread `next_id`.
#[no_mangle]
pub unsafe extern "C" fn nct_first_thread_start(_state: *mut std::ffi::c_void, next_id: c_int) {
    let s = nct_mut();
    s.next_to_resume = next_id;
    // Yield the boot fiber.  The drain loop will see next_to_resume
    // and resume the correct Zephyr thread fiber.
    sim_fiber::suspend_active_fiber(YieldReason::RtosPortYield);
    // After resume, the boot fiber continues (but normally nct_first_thread_start
    // isn't resumed — Zephyr runs threads until exit).
}

/// Switch from the current Zephyr thread to thread `next_id`.
///
/// Yields the current fiber.  The drain loop will resume the fiber
/// for thread `next_id`.
#[no_mangle]
pub unsafe extern "C" fn nct_swap_threads(_state: *mut std::ffi::c_void, next_id: c_int) {
    let s = nct_mut();
    s.next_to_resume = next_id;
    sim_fiber::suspend_active_fiber(YieldReason::RtosPortYield);
}

/// Abort a Zephyr thread.
///
/// If this is the currently running thread, Zephyr will have already
/// switched away via z_reschedule before calling nct_swap_threads.
/// We just mark the metadata.
#[no_mangle]
pub unsafe extern "C" fn nct_abort_thread(_state: *mut std::ffi::c_void, _t: c_int) {
    // In the multi-fiber model, the aborted thread's fiber will be
    // marked Exited when its entry function returns or when we detect
    // the abort in the drain loop.
}

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
// Drain-loop accessors
// ═══════════════════════════════════════════════════════════════════

/// Returns the next thread ID that the drain loop should resume.
/// Resets to -1 after reading.
pub fn nct_take_next_to_resume() -> i32 {
    let s = nct_mut();
    let next = s.next_to_resume;
    s.next_to_resume = -1;
    next
}

/// Signal that the given thread should be resumed next.
/// Used by the drain loop after sim_clock_announce to manually
/// direct the scheduler to the newly-ready thread.
pub fn nct_signal_next(thread_id: i32) {
    nct_mut().next_to_resume = thread_id;
}

/// Take a fiber out of the NCT state for resumption.
///
/// Uses Option::take() to avoid holding a reference into NCT across
/// fiber.resume(), preventing re-entrant borrow when the fiber calls
/// back into nct_swap_threads.
///
/// Returns (fiber, fiber_idx) if the thread has a live fiber.
pub fn nct_take_fiber(thread_id: i32) -> Option<(Fiber, usize)> {
    let s = nct_mut();
    if thread_id < 0 || thread_id as usize >= MAX_THREADS {
        return None;
    }
    let info = s.infos[thread_id as usize].as_ref()?;
    let fiber_idx = info.fiber_idx;
    let fibers = nct_fibers_mut();
    let fiber = fibers.get_mut(fiber_idx)?.take()?;
    // Only return if not terminated.
    if fiber.is_terminated() {
        // Put it back so it stays terminated.
        fibers[fiber_idx] = Some(fiber);
        return None;
    }
    Some((fiber, fiber_idx))
}

/// Return a fiber to the NCT state after resumption.
pub fn nct_return_fiber(fiber_idx: usize, fiber: Fiber) {
    let fibers = nct_fibers_mut();
    if fiber_idx < fibers.len() {
        fibers[fiber_idx] = Some(fiber);
    }
}

/// Check whether there are any non-terminated fibers.
pub fn nct_has_live_threads() -> bool {
    let fibers = nct_fibers_mut();
    fibers
        .iter()
        .any(|f| f.as_ref().is_some_and(|fiber| !fiber.is_terminated()))
}

/// Returns the number of non-terminated fibers.
#[allow(dead_code)]
pub fn nct_live_count() -> usize {
    let fibers = nct_fibers_mut();
    fibers
        .iter()
        .filter(|f| f.as_ref().is_some_and(|fiber| !fiber.is_terminated()))
        .count()
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
