//! # sim-ffi
//!
//! C ABI exports consumed by the FreeRTOS port layer.
//!
//! This crate provides `#[no_mangle]` exports for:
//! * `sim_now_ticks`
//! * `sim_create_task`
//! * `sim_start_scheduler`
//! * `sim_port_yield`
//! * `sim_task_exit`
//! * `sim_enter_critical` / `sim_exit_critical`
//! * `sim_trace_u32`

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
