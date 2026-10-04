//! Per-machine guest runtime state for the C ABI layer.
//!
//! # Architecture
//!
//! Each [`Simulator`] owns a [`GuestRuntime`] that holds:
//! - The machine's virtual clock (`now`)
//! - The currently executing task identity (`current_task_id`)
//! - The virtual CPU's interrupt-masking state (`interrupts`)
//! - Aligned instance regions created via `sim_instance_state` from guest C code
//!
//! The runtime is activated via [`activate_guest_runtime`] alongside
//! `SimGlobal` and `DeviceBank` so that C ABI functions dispatched from within
//! a fiber resolve into the correct machine's state.
//!
//! [`Simulator`]: crate::simulator::Simulator

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use sim_core::time::Tick;

// ---------------------------------------------------------------------------
// AlignedRegion
// ---------------------------------------------------------------------------

/// An aligned, zeroed heap allocation.
///
/// Wraps a raw pointer and its [`Layout`], freeing the memory on drop.
pub struct AlignedRegion {
    ptr: *mut u8,
    layout: Layout,
}

impl AlignedRegion {
    /// Allocate `size` bytes aligned to `alignment`, zero-initialized.
    ///
    /// Returns `None` if `size` or `alignment` is zero, the layout is invalid,
    /// or the allocator returns null.
    pub fn new(size: usize, alignment: usize) -> Option<Self> {
        if size == 0 || alignment == 0 {
            return None;
        }
        let layout = Layout::from_size_align(size, alignment).ok()?;
        // Safety: layout has non-zero size (checked above).
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            None
        } else {
            Some(Self {
                ptr: ptr.cast(),
                layout,
            })
        }
    }

    /// Returns the raw pointer to the allocated memory.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Returns the layout used for this allocation.
    pub fn layout(&self) -> Layout {
        self.layout
    }
}

impl Drop for AlignedRegion {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.ptr, self.layout);
        }
    }
}

// SAFETY: AlignedRegion owns one uniquely-held heap allocation. Moving the
// owning value between threads does not invalidate the allocation. The C ABI
// (via `sim_instance_state`) may expose the raw pointer while a GuestRuntime
// is active; callers are responsible for upholding aliasing and lifetime
// rules documented on `sim_instance_state`.
unsafe impl Send for AlignedRegion {}
unsafe impl Sync for AlignedRegion {}

// ---------------------------------------------------------------------------
// GuestRuntime
// ---------------------------------------------------------------------------

/// Per-machine guest runtime state.
///
/// Owns the virtual clock, current task identity, and all instance regions
/// allocated through `sim_instance_state`.
pub struct GuestRuntime {
    /// Virtual time in ticks.
    pub now: Cell<Tick>,
    /// Currently executing task id, set by the scheduler before resuming a
    /// fiber.
    pub current_task_id: Cell<u64>,
    /// Instance regions allocated via `sim_instance_state`, keyed by an opaque
    /// guest-provided key.
    pub instance_regions: RefCell<BTreeMap<u32, AlignedRegion>>,
    /// Interrupt-masking state of this machine's virtual CPU.
    pub interrupts: Cell<InterruptState>,
    /// This machine's peripheral event queue (`sim_schedule_event`):
    /// absolute tick → C callbacks.  Per machine, because a World's
    /// machines share a host thread; kept here rather than in `SimGlobal`
    /// because devices schedule events from any context, including while
    /// the engine holds the task table.
    pub peripheral_events: RefCell<PeripheralEvents>,
    /// The contexts (a task id, or 0: scheduler context) that are
    /// dispatching this machine's peripheral callbacks (see
    /// [`begin_dispatch`]).
    dispatching: RefCell<Vec<u64>>,
    /// A scheduler step of this machine is running (see [`begin_step`]).
    stepping: Cell<bool>,
}

/// A machine's peripheral event queue: absolute tick → C callbacks.
pub type PeripheralEvents = BTreeMap<u64, Vec<unsafe extern "C" fn()>>;

/// Interrupt-masking state of a machine's virtual CPU.
///
/// Belongs to the machine, not the host thread: several machines interleave
/// on one thread, and one that stops with interrupts masked must not mask
/// them for the next.
///
/// The mask is the sum of per-context contributions: each context (a task,
/// by id, or scheduler context — host code between steps, a peripheral
/// callback — as [`SCHEDULER_CONTEXT`]) holds its own critical nesting and
/// its own `portDISABLE_INTERRUPTS()` request, and enters and exits only
/// its own critical sections (`portENABLE_INTERRUPTS()` is the CPU-wide
/// flag, see [`InterruptState::enable`]).
/// Interrupts are masked while any context contributes.  A task's
/// contribution dies with the task ([`InterruptState::release_task`]);
/// every other context's stays.  `critical_nesting` and `disabled` are the
/// totals, kept for readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptState {
    /// Total depth of nested `sim_enter_critical()` sections, over every
    /// context (derived from the contributions).
    pub critical_nesting: u32,
    /// Some context has `portDISABLE_INTERRUPTS()` in effect (derived).
    pub disabled: bool,
    /// A context switch was requested while it could not be performed
    /// (interrupts masked, or no task running): the pended PendSV.
    pub yield_pending: bool,
    /// An interrupt service routine is running.
    pub in_isr: bool,
    /// Each masking context's own contribution.
    contributions: [MaskContribution; MASK_CONTEXTS],
}

/// The context id of scheduler context (host code between steps, a
/// peripheral callback) in [`InterruptState`]'s contributions; task ids are
/// never 0.
pub const SCHEDULER_CONTEXT: u64 = 0;

/// Contexts that can mask at once: scheduler context and the task holding
/// the CPU (a masked task keeps it), with room to spare.  A further one
/// counts as scheduler context.
const MASK_CONTEXTS: usize = 8;

/// One context's share of the mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaskContribution {
    context: u64,
    nesting: u32,
    disabled: bool,
}

impl MaskContribution {
    const NONE: Self = Self {
        context: SCHEDULER_CONTEXT,
        nesting: 0,
        disabled: false,
    };

    fn is_empty(&self) -> bool {
        self.nesting == 0 && !self.disabled
    }
}

impl Default for InterruptState {
    fn default() -> Self {
        Self::new()
    }
}

impl InterruptState {
    /// Unmasked, nothing pending.
    pub const fn new() -> Self {
        Self {
            critical_nesting: 0,
            disabled: false,
            yield_pending: false,
            in_isr: false,
            contributions: [MaskContribution::NONE; MASK_CONTEXTS],
        }
    }

    /// Whether interrupts are masked.
    pub fn masked(&self) -> bool {
        self.critical_nesting > 0 || self.disabled
    }

    /// `context`'s contribution: its critical nesting and whether it
    /// disabled interrupts.
    pub fn contribution(&self, context: u64) -> (u32, bool) {
        self.contributions
            .iter()
            .find(|c| !c.is_empty() && c.context == context)
            .map_or((0, false), |c| (c.nesting, c.disabled))
    }

    /// `context`'s entry, created if it has none (in an empty slot; with
    /// none left, scheduler context's).
    fn entry(&mut self, context: u64) -> &mut MaskContribution {
        let found = self
            .contributions
            .iter()
            .position(|c| !c.is_empty() && c.context == context)
            .or_else(|| {
                self.contributions
                    .iter()
                    .position(MaskContribution::is_empty)
            });
        let index = match found {
            Some(index) => index,
            None => {
                return self.entry_for_overflow();
            }
        };
        let entry = &mut self.contributions[index];
        if entry.is_empty() {
            entry.context = context;
        }
        entry
    }

    fn entry_for_overflow(&mut self) -> &mut MaskContribution {
        let index = self
            .contributions
            .iter()
            .position(|c| c.context == SCHEDULER_CONTEXT)
            .unwrap_or(0);
        &mut self.contributions[index]
    }

    /// Recompute the totals from the contributions.
    fn sum(&mut self) {
        self.critical_nesting = self
            .contributions
            .iter()
            .map(|c| c.nesting)
            .fold(0u32, u32::saturating_add);
        self.disabled = self.contributions.iter().any(|c| c.disabled);
    }

    /// `sim_enter_critical()` from `context`.
    pub fn enter_critical(&mut self, context: u64) {
        let entry = self.entry(context);
        entry.nesting = entry.nesting.saturating_add(1);
        self.sum();
    }

    /// `sim_exit_critical()` from `context`: only its own nesting (clamped
    /// at 0).
    pub fn exit_critical(&mut self, context: u64) {
        if let Some(c) = self
            .contributions
            .iter_mut()
            .find(|c| !c.is_empty() && c.context == context)
        {
            c.nesting = c.nesting.saturating_sub(1);
        }
        self.sum();
    }

    /// `portDISABLE_INTERRUPTS()` from `context`.
    pub fn disable(&mut self, context: u64) {
        self.entry(context).disabled = true;
        self.sum();
    }

    /// `portENABLE_INTERRUPTS()`: the CPU's interrupt-enable flag is one
    /// flag, as on hardware (PRIMASK): enabling clears every context's
    /// disable request, whoever made it (firmware that unmasks after host
    /// code disabled interrupts unmasks the CPU).  Critical nesting is
    /// left to each context.  Retiring a task, by contrast, removes only
    /// that task's own request ([`Self::release_task`]).
    pub fn enable(&mut self) {
        for c in &mut self.contributions {
            c.disabled = false;
        }
        self.sum();
    }

    /// Task `task` was retired: remove exactly its contribution.  Every
    /// other context's (host code, a callback, scheduler context) stays,
    /// with the pending yield.  Returns whether this unmasked the CPU.
    pub fn release_task(&mut self, task: u64) -> bool {
        if task == SCHEDULER_CONTEXT || !self.masked() {
            return false;
        }
        let Some(c) = self
            .contributions
            .iter_mut()
            .find(|c| !c.is_empty() && c.context == task)
        else {
            return false;
        };
        *c = MaskContribution::NONE;
        self.sum();
        if self.masked() {
            return false;
        }
        // The task's own latched switch dies with it.
        self.yield_pending = false;
        true
    }
}

impl GuestRuntime {
    /// Create a fresh runtime with a zeroed clock and task id.
    pub fn new() -> Self {
        Self {
            now: Cell::new(0),
            current_task_id: Cell::new(0),
            instance_regions: RefCell::new(BTreeMap::new()),
            interrupts: Cell::new(InterruptState::default()),
            peripheral_events: RefCell::new(BTreeMap::new()),
            dispatching: RefCell::new(Vec::new()),
            stepping: Cell::new(false),
        }
    }

    /// Drop all instance regions, leaving the map empty.
    ///
    /// The next `sim_instance_state` call for any key will allocate a fresh
    /// region. This is called on machine reset so region lifetimes match the
    /// machine's.
    pub fn reset(&self) {
        self.instance_regions.borrow_mut().clear();
        self.interrupts.set(InterruptState::default());
        self.peripheral_events.borrow_mut().clear();
    }

    /// Read the virtual clock from this runtime.
    pub fn now_ticks(&self) -> Tick {
        self.now.get()
    }

    /// Set the virtual clock on this runtime.
    pub fn set_now_ticks(&self, now: Tick) {
        self.now.set(now);
    }

    /// Read the current task ID from this runtime.
    pub fn current_task(&self) -> u64 {
        self.current_task_id.get()
    }

    /// Set the current task ID on this runtime.
    pub fn set_current_task(&self, id: u64) {
        self.current_task_id.set(id);
    }
}

impl Default for GuestRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Activation
// ---------------------------------------------------------------------------

thread_local! {
    /// The currently active [`GuestRuntime`], if any.
    ///
    /// When set, C ABI functions like `sim_instance_state` resolve into this
    /// runtime. When `None`, those functions return null.
    static ACTIVE_GUEST_RUNTIME: RefCell<Option<Rc<GuestRuntime>>> =
        const { RefCell::new(None) };

    /// Interrupt state used when no [`GuestRuntime`] is active (standalone
    /// firmware).
    static FALLBACK_INTERRUPTS: Cell<InterruptState> =
        const { Cell::new(InterruptState::new()) };

    /// Peripheral event queue used when no [`GuestRuntime`] is active
    /// (standalone firmware).
    static FALLBACK_EVENTS: RefCell<PeripheralEvents> = const { RefCell::new(BTreeMap::new()) };

    /// `GuestRuntime::dispatching` when no runtime is active.
    static FALLBACK_DISPATCHING: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };

    /// `GuestRuntime::stepping` when no runtime is active.
    static FALLBACK_STEPPING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the active machine's peripheral event queue is borrowed (the
/// engine's no-C-under-a-borrow check, `sim_debug_check_engine_unborrowed`).
pub(crate) fn peripheral_events_held() -> bool {
    let runtime = ACTIVE_GUEST_RUNTIME
        .try_with(|cell| cell.try_borrow().ok().and_then(|rt| rt.clone()))
        .ok()
        .flatten();
    match runtime {
        Some(rt) => rt.peripheral_events.try_borrow_mut().is_err(),
        None => FALLBACK_EVENTS
            .try_with(|q| q.try_borrow_mut().is_err())
            .unwrap_or(false),
    }
}

/// Marks a context's callback dispatch on the active machine as running
/// for its lifetime; see [`begin_dispatch`].
pub(crate) struct DispatchGuard {
    runtime: Option<Rc<GuestRuntime>>,
    context: u64,
}

fn with_dispatching<R>(
    runtime: &Option<Rc<GuestRuntime>>,
    f: impl FnOnce(&mut Vec<u64>) -> R,
) -> R {
    match runtime {
        Some(rt) => f(&mut rt.dispatching.borrow_mut()),
        None => FALLBACK_DISPATCHING.with(|d| f(&mut d.borrow_mut())),
    }
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        let context = self.context;
        let remove = |d: &mut Vec<u64>| {
            if let Some(i) = d.iter().position(|&c| c == context) {
                d.remove(i);
            }
        };
        match &self.runtime {
            Some(rt) => remove(&mut rt.dispatching.borrow_mut()),
            None => {
                let _ = FALLBACK_DISPATCHING.try_with(|d| remove(&mut d.borrow_mut()));
            }
        }
    }
}

/// Begin dispatching the active machine's peripheral callbacks from
/// `context` (the running task's id, or 0 for scheduler context), or
/// `None` if a dispatch from the same context is already running further
/// up the stack (a callback that dispatches callbacks itself): that outer
/// dispatch drains the queue, one callback at a time, each charged to the
/// storm limit, instead of recursing without bound.  Per context: a task
/// whose dispatch is suspended (it never is while dispatching, see
/// `dispatch_events`) cannot keep the scheduler from draining.
pub(crate) fn begin_dispatch(context: u64) -> Option<DispatchGuard> {
    let runtime = ACTIVE_GUEST_RUNTIME.with(|cell| cell.borrow().clone());
    let busy = with_dispatching(&runtime, |d| {
        if d.contains(&context) {
            true
        } else {
            d.push(context);
            false
        }
    });
    if busy {
        None
    } else {
        Some(DispatchGuard { runtime, context })
    }
}

/// Marks a scheduler step of the active machine as running for its
/// lifetime; see [`begin_step`].
pub(crate) struct StepGuard {
    runtime: Option<Rc<GuestRuntime>>,
}

impl Drop for StepGuard {
    fn drop(&mut self) {
        match &self.runtime {
            Some(rt) => rt.stepping.set(false),
            None => {
                let _ = FALLBACK_STEPPING.try_with(|s| s.set(false));
            }
        }
    }
}

/// Begin a scheduler step of the active machine, or `None` if one is
/// already running further up the stack (a callback or an ISR that calls
/// `sim_scheduler_tick()`): scheduler steps do not nest.
pub(crate) fn begin_step() -> Option<StepGuard> {
    let runtime = ACTIVE_GUEST_RUNTIME.with(|cell| cell.borrow().clone());
    let busy = match &runtime {
        Some(rt) => rt.stepping.replace(true),
        None => FALLBACK_STEPPING.with(|s| s.replace(true)),
    };
    if busy {
        None
    } else {
        Some(StepGuard { runtime })
    }
}

/// Whether `context` is dispatching the active machine's callbacks.
pub(crate) fn dispatching_in(context: u64) -> bool {
    let runtime = ACTIVE_GUEST_RUNTIME
        .try_with(|cell| cell.borrow().clone())
        .ok()
        .flatten();
    match &runtime {
        Some(rt) => rt.dispatching.borrow().contains(&context),
        None => FALLBACK_DISPATCHING
            .try_with(|d| d.borrow().contains(&context))
            .unwrap_or(false),
    }
}

/// Run `f` on the active machine's peripheral event queue.
///
/// `f` must not call back into the C ABI.
pub fn with_peripheral_events<R>(f: impl FnOnce(&mut PeripheralEvents) -> R) -> R {
    let runtime = ACTIVE_GUEST_RUNTIME.with(|cell| cell.borrow().clone());
    match runtime {
        Some(rt) => f(&mut rt.peripheral_events.borrow_mut()),
        None => FALLBACK_EVENTS.with(|q| f(&mut q.borrow_mut())),
    }
}

/// RAII guard returned by [`activate_guest_runtime`].
///
/// On drop, restores whichever [`GuestRuntime`] was active before this guard
/// was created.
#[must_use = "a guest runtime activation ends when its guard is dropped"]
pub struct GuestRuntimeGuard {
    prior: Option<Rc<GuestRuntime>>,
}

impl Drop for GuestRuntimeGuard {
    fn drop(&mut self) {
        // `try_with` avoids a second panic during TLS teardown.
        let _ = ACTIVE_GUEST_RUNTIME.try_with(|cell| {
            *cell.borrow_mut() = self.prior.take();
        });
    }
}

/// Activate `runtime` for the current thread, returning a guard that restores
/// the previous active runtime on drop.
pub fn activate_guest_runtime(runtime: &Rc<GuestRuntime>) -> GuestRuntimeGuard {
    let prior = ACTIVE_GUEST_RUNTIME.with(|cell| {
        let mut active = cell.borrow_mut();
        let prior = active.clone();
        *active = Some(runtime.clone());
        prior
    });
    GuestRuntimeGuard { prior }
}

/// Run `f` with `runtime` temporarily active, then restore the prior runtime.
pub fn with_guest_runtime<R>(runtime: &Rc<GuestRuntime>, f: impl FnOnce() -> R) -> R {
    let _guard = activate_guest_runtime(runtime);
    f()
}

// ---------------------------------------------------------------------------
// Global accessors: read/write through the active GuestRuntime,
// falling back to the legacy process-global atomics when no runtime is active.
// ---------------------------------------------------------------------------

use std::sync::atomic::Ordering;

/// Return the current virtual time.
///
/// Reads from the active [`GuestRuntime`]'s `now` Cell when a runtime is
/// active; falls back to the legacy [`SIM_NOW`](crate::SIM_NOW) atomic when
/// no runtime is active (legacy single-simulator tests).
///
/// Safe to call from any context — uses `RefCell::borrow` on the activation
/// thread-local, not the global `SIM_GLOBAL` RefCell.
pub fn active_now() -> Tick {
    // At thread exit (instrumented C or a trace call from a thread-local
    // destructor) the activation may be gone: the legacy clock answers.
    ACTIVE_GUEST_RUNTIME
        .try_with(|cell| cell.borrow().as_ref().map(|rt| rt.now.get()))
        .ok()
        .flatten()
        .unwrap_or_else(|| crate::SIM_NOW.load(Ordering::Relaxed))
}

/// Set the current virtual time.
///
/// Writes to the active [`GuestRuntime`]'s `now` Cell when a runtime is
/// active; falls back to the legacy [`SIM_NOW`](crate::SIM_NOW) atomic when
/// no runtime is active.
///
/// Called from the scheduler only — never from within a fiber.
pub fn set_active_now(now: Tick) {
    ACTIVE_GUEST_RUNTIME.with(|cell| {
        if let Some(rt) = cell.borrow().as_ref() {
            rt.now.set(now);
        } else {
            crate::SIM_NOW.store(now, Ordering::Relaxed);
        }
    })
}

/// Return the current task ID.
///
/// Reads from the active [`GuestRuntime`]'s `current_task_id` Cell when a
/// runtime is active; falls back to the legacy
/// [`CURRENT_TASK_ID`](crate::CURRENT_TASK_ID) atomic when no runtime is
/// active.
///
/// Safe to call from any context — uses `RefCell::borrow` on the activation
/// thread-local, not the global `SIM_GLOBAL` RefCell.
pub fn active_task_id() -> u64 {
    ACTIVE_GUEST_RUNTIME.with(|cell| {
        if let Some(rt) = cell.borrow().as_ref() {
            return rt.current_task_id.get();
        }
        crate::CURRENT_TASK_ID.load(Ordering::Relaxed)
    })
}

/// Set the current task ID.
///
/// Writes to the active [`GuestRuntime`]'s `current_task_id` Cell when a
/// runtime is active; falls back to the legacy
/// [`CURRENT_TASK_ID`](crate::CURRENT_TASK_ID) atomic when no runtime is
/// active.
///
/// Called from the scheduler only — never from within a fiber.
pub fn set_active_task_id(id: u64) {
    ACTIVE_GUEST_RUNTIME.with(|cell| {
        if let Some(rt) = cell.borrow().as_ref() {
            rt.current_task_id.set(id);
        } else {
            crate::CURRENT_TASK_ID.store(id, Ordering::Relaxed);
        }
    })
}

/// Return the active machine's interrupt state.
///
/// Falls back to a thread-local state when no runtime is active.  Safe to
/// call from any context.
pub fn interrupt_state() -> InterruptState {
    ACTIVE_GUEST_RUNTIME.with(|cell| {
        if let Some(rt) = cell.borrow().as_ref() {
            return rt.interrupts.get();
        }
        FALLBACK_INTERRUPTS.with(|s| s.get())
    })
}

/// Update the active machine's interrupt state and return `f`'s result.
///
/// `f` must not call back into the C ABI.
pub fn update_interrupt_state<R>(f: impl FnOnce(&mut InterruptState) -> R) -> R {
    let apply = |cell: &Cell<InterruptState>| {
        let mut state = cell.get();
        let result = f(&mut state);
        cell.set(state);
        result
    };
    let runtime = ACTIVE_GUEST_RUNTIME.with(|cell| cell.borrow().clone());
    match runtime {
        Some(rt) => apply(&rt.interrupts),
        None => FALLBACK_INTERRUPTS.with(apply),
    }
}

// ---------------------------------------------------------------------------
// C ABI: sim_instance_state
// ---------------------------------------------------------------------------

/// Return or allocate instance-local state for guest code.
///
/// On the first call for a given `key`, allocates `size` zeroed bytes at the
/// requested `alignment`. Subsequent calls for the same key return the existing
/// pointer. If the existing region's size or alignment doesn't match the
/// request, returns null.
///
/// Returns null when no [`GuestRuntime`] is active.
///
/// The returned pointer is valid for the lifetime of the active machine — until
/// [`GuestRuntime::reset`] is called or the runtime is dropped.
///
/// # Safety
///
/// The caller must ensure that the returned pointer is only accessed within
/// the lifetime of the active [`GuestRuntime`] and that reads/writes respect
/// the size and alignment of the allocated region.
#[no_mangle]
pub unsafe extern "C" fn sim_instance_state(key: u32, size: u32, alignment: u32) -> *mut u8 {
    let runtime = ACTIVE_GUEST_RUNTIME.with(|cell| cell.borrow().clone());
    let runtime = match runtime {
        Some(rt) => rt,
        None => return std::ptr::null_mut(),
    };

    let mut regions = runtime.instance_regions.borrow_mut();

    if let Some(region) = regions.get(&key) {
        // Existing region: validate size and alignment match.
        if region.layout().size() != size as usize || region.layout().align() != alignment as usize
        {
            return std::ptr::null_mut();
        }
        return region.as_ptr();
    }

    // First call: allocate a fresh region.
    let region = match AlignedRegion::new(size as usize, alignment as usize) {
        Some(r) => r,
        None => return std::ptr::null_mut(),
    };

    let ptr = region.as_ptr();
    regions.insert(key, region);
    ptr
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_region_zero_size_returns_none() {
        assert!(AlignedRegion::new(0, 4).is_none());
    }

    #[test]
    fn aligned_region_zero_alignment_returns_none() {
        assert!(AlignedRegion::new(4, 0).is_none());
    }

    #[test]
    fn aligned_region_both_zero_returns_none() {
        assert!(AlignedRegion::new(0, 0).is_none());
    }

    #[test]
    fn aligned_region_valid_allocation_works() {
        let region = AlignedRegion::new(16, 8).expect("valid allocation");
        assert!(!region.as_ptr().is_null());
        assert_eq!(region.layout().size(), 16);
        assert_eq!(region.layout().align(), 8);
    }

    // ── sim_instance_state FFI-level tests ──────────────────────────────

    #[test]
    fn sim_instance_state_no_runtime_returns_null() {
        assert!(unsafe { sim_instance_state(1, 4, 4) }.is_null());
    }

    #[test]
    fn sim_instance_state_zero_size_returns_null() {
        let rt = Rc::new(GuestRuntime::default());
        let _guard = activate_guest_runtime(&rt);
        assert!(unsafe { sim_instance_state(2, 0, 4) }.is_null());
    }

    #[test]
    fn sim_instance_state_zero_alignment_returns_null() {
        let rt = Rc::new(GuestRuntime::default());
        let _guard = activate_guest_runtime(&rt);
        assert!(unsafe { sim_instance_state(3, 4, 0) }.is_null());
    }

    #[test]
    fn sim_instance_state_both_zero_returns_null() {
        let rt = Rc::new(GuestRuntime::default());
        let _guard = activate_guest_runtime(&rt);
        assert!(unsafe { sim_instance_state(4, 0, 0) }.is_null());
    }

    #[test]
    fn sim_instance_state_mismatched_size_returns_null() {
        let rt = Rc::new(GuestRuntime::default());
        let _guard = activate_guest_runtime(&rt);
        let p1 = unsafe { sim_instance_state(5, 8, 4) };
        assert!(!p1.is_null());
        // Same key, different size → null.
        assert!(unsafe { sim_instance_state(5, 16, 4) }.is_null());
    }

    #[test]
    fn sim_instance_state_mismatched_alignment_returns_null() {
        let rt = Rc::new(GuestRuntime::default());
        let _guard = activate_guest_runtime(&rt);
        let p1 = unsafe { sim_instance_state(6, 8, 4) };
        assert!(!p1.is_null());
        // Same key, different alignment → null.
        assert!(unsafe { sim_instance_state(6, 8, 8) }.is_null());
    }

    #[test]
    fn sim_instance_state_matching_returns_same_pointer() {
        let rt = Rc::new(GuestRuntime::default());
        let _guard = activate_guest_runtime(&rt);
        let p1 = unsafe { sim_instance_state(7, 8, 4) };
        let p2 = unsafe { sim_instance_state(7, 8, 4) };
        assert!(!p1.is_null());
        assert_eq!(p1, p2);
    }

    #[test]
    fn sim_instance_state_different_keys_return_distinct_pointers() {
        let rt = Rc::new(GuestRuntime::default());
        let _guard = activate_guest_runtime(&rt);
        let p1 = unsafe { sim_instance_state(10, 8, 4) };
        let p2 = unsafe { sim_instance_state(20, 8, 4) };
        assert!(!p1.is_null());
        assert!(!p2.is_null());
        assert_ne!(p1, p2);
    }

    // ── R1: per-machine time and task-id isolation tests ────────────────

    #[test]
    fn two_simulator_interleave_isolates_time_and_task_id() {
        // Two simulators activated in A/B and B/A order must observe
        // only their own time and task ID, 100 repetitions each.
        for seed in 0..100 {
            let rt_a = Rc::new(GuestRuntime::new());
            let rt_b = Rc::new(GuestRuntime::new());
            rt_a.set_now_ticks(seed as u64);
            rt_b.set_now_ticks((seed * 2) as u64);
            rt_a.set_current_task(42);
            rt_b.set_current_task(99);

            // A then B
            {
                let _guard_a = activate_guest_runtime(&rt_a);
                assert_eq!(active_now(), seed as u64);
                assert_eq!(active_task_id(), 42);
            }
            {
                let _guard_b = activate_guest_runtime(&rt_b);
                assert_eq!(active_now(), (seed * 2) as u64);
                assert_eq!(active_task_id(), 99);
            }

            // B then A
            {
                let _guard_b = activate_guest_runtime(&rt_b);
                assert_eq!(active_now(), (seed * 2) as u64);
                assert_eq!(active_task_id(), 99);
            }
            {
                let _guard_a = activate_guest_runtime(&rt_a);
                assert_eq!(active_now(), seed as u64);
                assert_eq!(active_task_id(), 42);
            }
        }
    }

    #[test]
    fn nested_activation_restores_prior_runtime() {
        let outer = Rc::new(GuestRuntime::new());
        let inner = Rc::new(GuestRuntime::new());
        outer.set_now_ticks(100);
        inner.set_now_ticks(200);
        outer.set_current_task(1);
        inner.set_current_task(2);

        // Activate outer
        let _outer_guard = activate_guest_runtime(&outer);
        assert_eq!(active_now(), 100);
        assert_eq!(active_task_id(), 1);

        // Nested inner activation
        {
            let _inner_guard = activate_guest_runtime(&inner);
            assert_eq!(active_now(), 200);
            assert_eq!(active_task_id(), 2);
        }

        // After inner guard drops, outer is restored
        assert_eq!(active_now(), 100);
        assert_eq!(active_task_id(), 1);
    }

    #[test]
    fn nested_activation_panic_unwind_restores_prior() {
        let outer = Rc::new(GuestRuntime::new());
        let inner = Rc::new(GuestRuntime::new());
        outer.set_now_ticks(100);
        inner.set_now_ticks(200);

        let _outer_guard = activate_guest_runtime(&outer);
        assert_eq!(active_now(), 100);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner_guard = activate_guest_runtime(&inner);
            assert_eq!(active_now(), 200);
            panic!("simulated fiber panic");
        }));

        assert!(result.is_err());
        // After panic unwind, outer runtime must be restored
        assert_eq!(active_now(), 100);
    }

    #[test]
    fn fallback_to_atomics_when_no_runtime_active() {
        // When no runtime is active, the accessors must fall back to
        // the legacy global atomics.
        crate::SIM_NOW.store(777, Ordering::Relaxed);
        crate::CURRENT_TASK_ID.store(888, Ordering::Relaxed);
        assert_eq!(active_now(), 777);
        assert_eq!(active_task_id(), 888);
        // Restore to zero so other tests aren't affected.
        crate::SIM_NOW.store(0, Ordering::Relaxed);
        crate::CURRENT_TASK_ID.store(0, Ordering::Relaxed);
    }
}
