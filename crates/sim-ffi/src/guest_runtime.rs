//! Per-machine guest runtime state for the C ABI layer.
//!
//! # Architecture
//!
//! Each [`Simulator`] owns a [`GuestRuntime`] that holds:
//! - The machine's virtual clock (`now`)
//! - The currently executing task identity (`current_task_id`)
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
    /// Returns `None` if the layout is invalid or the allocator returns null.
    pub fn new(size: usize, alignment: usize) -> Option<Self> {
        let layout = Layout::from_size_align(size, alignment).ok()?;
        // Safety: layout has non-zero size (enforced by from_size_align).
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

// SAFETY: AlignedRegion owns a uniquely-held allocation. The raw pointer
// is never exposed for aliasing mutation outside the owning BTreeMap.
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
}

impl GuestRuntime {
    /// Create a fresh runtime with a zeroed clock and task id.
    pub fn new() -> Self {
        Self {
            now: Cell::new(0),
            current_task_id: Cell::new(0),
            instance_regions: RefCell::new(BTreeMap::new()),
        }
    }

    /// Drop all instance regions, leaving the map empty.
    ///
    /// The next `sim_instance_state` call for any key will allocate a fresh
    /// region. This is called on machine reset so region lifetimes match the
    /// machine's.
    pub fn reset(&self) {
        self.instance_regions.borrow_mut().clear();
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
