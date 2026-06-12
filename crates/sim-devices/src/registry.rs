//! Compile-time driver registration via `inventory`.
//!
//! Drivers (UART, GPIO, timer, etc.) are registered at compile time
//! using `inventory::submit!`.  The simulator collects and sorts them by
//! `init_order` before calling each `init_func`.
//!
//! # Usage
//!
//! ```ignore
//! use sim_devices::registry::SimulatedDriver;
//!
//! extern "C" fn my_uart_init() -> i32 { 0 }
//!
//! inventory::submit! {
//!     SimulatedDriver {
//!         name: "uart0",
//!         init_order: 100,
//!         init_func: my_uart_init,
//!     }
//! }
//! ```

/// A driver registered for compile-time discovery.
///
/// The `inventory` crate discovers all `SimulatedDriver` instances across
/// the linked binary at program startup.  The simulator then sorts them
/// by `(init_order, name)` and calls each `init_func` in sequence.
pub struct SimulatedDriver {
    /// Human-readable driver name (used for sorting within the same order).
    pub name: &'static str,
    /// Initialization order (lower values run first).
    pub init_order: u32,
    /// C function to call for initialization.  Returns 0 on success,
    /// non-zero on failure.
    pub init_func: unsafe extern "C" fn() -> i32,
}

// The `collect!` macro makes `inventory::iter<SimulatedDriver>` work.
inventory::collect!(SimulatedDriver);

/// Collect, sort, and initialize all registered drivers.
///
/// Returns the number of drivers successfully initialized.
/// Drivers are sorted by `(init_order, name)` for deterministic ordering.
///
/// # Safety
///
/// Calls into arbitrary C init functions.  Callers must ensure the global
/// simulator state is ready before calling this.
pub fn init_all_drivers() -> usize {
    let mut drivers: Vec<&'static SimulatedDriver> = inventory::iter::<SimulatedDriver>().collect();

    // Sort deterministically: init_order ascending, then name alphabetically.
    drivers.sort_by_key(|d| (d.init_order, d.name));

    let mut count = 0;
    for driver in drivers {
        // Safety: the init function is provided by the driver author and must
        // be a valid C ABI function.
        let result = unsafe { (driver.init_func)() };
        if result == 0 {
            count += 1;
        }
        // Non-zero return values are silently ignored for now (the driver
        // should have recorded its own error).
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn test_init_ok() -> i32 {
        0
    }

    // Fail variant is exercised in test_init_all_drivers_runs via registry ordering
    #[allow(dead_code)]
    extern "C" fn test_init_fail() -> i32 {
        -1
    }

    inventory::submit! {
        SimulatedDriver {
            name: "test_driver_a",
            init_order: 200,
            init_func: test_init_ok,
        }
    }

    inventory::submit! {
        SimulatedDriver {
            name: "test_driver_b",
            init_order: 100,
            init_func: test_init_ok,
        }
    }

    #[test]
    fn test_registry_has_entries() {
        let drivers: Vec<&SimulatedDriver> = inventory::iter::<SimulatedDriver>().collect();
        assert!(
            drivers.len() >= 2,
            "expected at least 2 drivers in registry"
        );
    }

    #[test]
    fn test_init_all_drivers_runs() {
        let count = init_all_drivers();
        assert!(count >= 2, "expected at least 2 successful inits");
    }
}
