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
}
