//! Virtual Peripheral Devices C ABI FFI exports.

use crate::{is_critical_locked, SIM_NOW, TL_TRACE};
use std::sync::atomic::Ordering;

/// Raise a virtual interrupt.
///
/// Records the event in the trace and adds the IRQ to the pending set.
/// Actual delivery happens when `sim_irq_deliver_pending()` is called
/// from a non-critical context.
///
/// # Safety
///
/// Always safe — only touches the thread-local IRQ controller and trace.
/// Can be called from any context (within a fiber, from C, etc.).
#[no_mangle]
pub unsafe extern "C" fn sim_irq_raise(irq: u32) {
    let now = SIM_NOW.load(Ordering::Relaxed);

    // Record in trace
    TL_TRACE.with(|tl| {
        tl.borrow_mut()
            .push(sim_core::trace::TraceEvent::InterruptRaised { at: now, irq });
    });

    // Add to IRQ controller
    sim_devices::irq::with_irq_mut(|ctrl| {
        ctrl.raise(irq);
    });
}

/// Clear a pending virtual interrupt (e.g., acknowledged by handler).
///
/// # Safety
///
/// Always safe — only touches the thread-local IRQ controller.
#[no_mangle]
pub unsafe extern "C" fn sim_irq_clear(irq: u32) {
    sim_devices::irq::with_irq_mut(|ctrl| {
        ctrl.clear(irq);
    });
}

/// Check whether any virtual interrupt is pending.
///
/// Returns the lowest pending IRQ number, or `u32::MAX` if none are pending.
///
/// # Safety
///
/// Always safe — only reads the thread-local IRQ controller.
#[no_mangle]
pub unsafe extern "C" fn sim_irq_pending() -> u32 {
    sim_devices::irq::with_irq(|ctrl| ctrl.peek_pending().first().copied().unwrap_or(u32::MAX))
}

/// Deliver all pending virtual interrupts, if not in a critical section.
///
/// Returns the number of interrupts delivered.  Each delivered interrupt
/// records an `InterruptDelivered` trace event.
///
/// Called by the scheduler loop between task resumptions.
///
/// # Safety
///
/// Safe — only touches thread-local state.
#[no_mangle]
pub unsafe extern "C" fn sim_irq_deliver_pending(now: u64) -> u32 {
    if is_critical_locked() {
        return 0;
    }

    let irqs = sim_devices::irq::with_irq_mut(|ctrl| ctrl.take_pending());

    let count = irqs.len() as u32;
    for irq in irqs {
        // Record delivery in trace
        TL_TRACE.with(|tl| {
            tl.borrow_mut()
                .push(sim_core::trace::TraceEvent::InterruptDelivered { at: now, irq });
        });
    }
    count
}

/// Deliver pending IRQs and drain expired timers.
///
/// Called by the scheduler after each task yield.
pub(crate) fn deliver_pending_irqs(now: u64) -> u32 {
    // Drain expired timers first (which may raise IRQs)
    sim_devices::drain_expired_timers(now);

    // Then deliver pending IRQs
    unsafe { sim_irq_deliver_pending(now) }
}

/// Write bytes to a virtual UART.
///
/// Returns the number of bytes actually written.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local UART map).
#[no_mangle]
pub unsafe extern "C" fn sim_uart_write(id: u32, data_ptr: *const u8, len: u32) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }

    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };

    let now = SIM_NOW.load(Ordering::Relaxed);

    // Record in trace
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "uart_tx",
            value: id,
        });
    });

    sim_devices::with_uart_mut(id, |uart| uart.write(data)).unwrap_or(0) as u32
}

/// Arm a virtual timer to fire after `delay_ticks` from the current time.
///
/// If the timer was already armed, the previous schedule is overwritten.
///
/// # Safety
///
/// Always safe — uses atomic time read and thread-local timer storage.
#[no_mangle]
pub unsafe extern "C" fn sim_timer_arm(id: u32, delay_ticks: u64) {
    let now = SIM_NOW.load(Ordering::Relaxed);
    sim_devices::with_timer_mut(id, |timer| {
        timer.arm(now, delay_ticks);
    });
}

/// Disarm a virtual timer.  No interrupt will fire.
///
/// # Safety
///
/// Always safe — uses thread-local timer storage.
#[no_mangle]
pub unsafe extern "C" fn sim_timer_disarm(id: u32) {
    sim_devices::with_timer_mut(id, |timer| {
        timer.disarm();
    });
}

/// Set a GPIO pin state.
///
/// Returns the IRQ number if the change triggered an interrupt, or
/// `u32::MAX` if no interrupt was triggered.
///
/// # Safety
///
/// Always safe — uses thread-local GPIO storage.
#[no_mangle]
pub unsafe extern "C" fn sim_gpio_set(id: u32, pin: u32, state: u32) -> u32 {
    let result = sim_devices::with_gpio_mut(id, |gpio| gpio.set(pin as usize, state != 0));
    match result {
        Some(Some(irq)) => {
            // GPIO change triggered an IRQ — raise it
            sim_irq_raise(irq);
            irq
        }
        _ => u32::MAX,
    }
}

// ---------------------------------------------------------------------------
// Virtual I2C C ABI exports (Phase 22)
// ---------------------------------------------------------------------------

/// Write data to an I2C target from the master.
///
/// The target address must have been set with `sim_i2c_set_address` first.
/// Returns the number of bytes written, or 0 if the controller is disabled
/// or not registered.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_write(id: u32, data_ptr: *const u8, len: u32) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "i2c_write",
            value: id,
        });
    });

    sim_devices::with_i2c_mut(id, |i2c| i2c.write(data)).unwrap_or(0) as u32
}

/// Read data from an I2C target into a caller-provided buffer.
///
/// The target address must have been set with `sim_i2c_set_address` first.
/// Returns the number of bytes read.  The RX buffer must be pre-populated
/// via test-script injection.
///
/// # Safety
///
/// `buf_ptr` must be a valid pointer to at least `len` bytes of writable memory.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_read(id: u32, buf_ptr: *mut u8, len: u32) -> u32 {
    if buf_ptr.is_null() || len == 0 {
        return 0;
    }

    // Fault injection: check if a NACK was injected
    if sim_devices::with_fault_injector_mut(|f| f.consume_i2c_nack()) {
        return 0;
    }

    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "i2c_read",
            value: id,
        });
    });

    let result = sim_devices::with_i2c_mut(id, |i2c| i2c.read(len as usize));
    match result {
        Some(data) => {
            let actual = data.len().min(len as usize);
            let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, actual) };
            buf.copy_from_slice(&data[..actual]);
            actual as u32
        }
        None => 0,
    }
}

/// Perform a combined I2C write-then-read (repeated start).
///
/// Writes `tx_len` bytes from `tx_ptr`, then reads `rx_len` bytes into
/// `rx_buf`.  Returns the number of bytes read, or 0 if the controller
/// is not found.
///
/// # Safety
///
/// `tx_ptr` must be valid for `tx_len` bytes.  `rx_buf` must be writable
/// for at least `rx_len` bytes.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_write_read(
    id: u32,
    tx_ptr: *const u8,
    tx_len: u32,
    rx_buf: *mut u8,
    rx_len: u32,
) -> u32 {
    if tx_ptr.is_null() || rx_buf.is_null() || tx_len == 0 || rx_len == 0 {
        return 0;
    }
    let tx_data = unsafe { std::slice::from_raw_parts(tx_ptr, tx_len as usize) };
    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "i2c_wr",
            value: id,
        });
    });

    let result = sim_devices::with_i2c_mut(id, |i2c| i2c.write_read(tx_data, rx_len as usize));
    match result {
        Some((_written, rx_data)) => {
            let actual = rx_data.len().min(rx_len as usize);
            let buf = unsafe { std::slice::from_raw_parts_mut(rx_buf, actual) };
            buf.copy_from_slice(&rx_data[..actual]);
            actual as u32
        }
        None => 0,
    }
}

/// Set the I2C target address.
///
/// # Safety
///
/// Always safe — uses thread-local I2C storage.
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_set_address(id: u32, address: u16, ten_bit: u32) {
    sim_devices::with_i2c_mut(id, |i2c| {
        i2c.set_address(address, ten_bit != 0);
    });
}

/// Check whether the last I2C operation received a NACK.
///
/// Returns 1 if NACK was received, 0 otherwise (or if controller not found).
///
/// # Safety
///
/// Always safe — uses thread-local I2C storage.
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_get_nack(id: u32) -> u32 {
    sim_devices::with_i2c(id, |i2c| i2c.nack as u32).unwrap_or(0)
}

/// Inject bytes into the I2C RX buffer (for test scripts).
///
/// This simulates an I2C target device sending data to the master.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local I2C storage).
#[no_mangle]
pub unsafe extern "C" fn sim_i2c_inject_rx(id: u32, data_ptr: *const u8, len: u32) {
    if data_ptr.is_null() || len == 0 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    sim_devices::with_i2c_mut(id, |i2c| {
        i2c.inject_rx(data);
    });
}

// ---------------------------------------------------------------------------
// Virtual SPI C ABI exports (Phase 22)
// ---------------------------------------------------------------------------

/// Perform a full-duplex SPI transfer.
///
/// Writes `tx_len` bytes from `tx_ptr`, reads into `rx_buf` (up to `rx_len`
/// bytes).  Returns the number of bytes received.  The RX buffer should
/// be pre-populated via `sim_spi_inject_rx` for deterministic tests.
///
/// # Safety
///
/// `tx_ptr` must be valid for `tx_len` bytes.  `rx_buf` must be writable
/// for at least `rx_len` bytes.
/// Safe to call from any context (uses thread-local SPI storage).
#[no_mangle]
pub unsafe extern "C" fn sim_spi_transfer(
    id: u32,
    tx_ptr: *const u8,
    tx_len: u32,
    rx_buf: *mut u8,
    rx_len: u32,
) -> u32 {
    if tx_ptr.is_null() || rx_buf.is_null() || tx_len == 0 || rx_len == 0 {
        return 0;
    }
    let tx_data = unsafe { std::slice::from_raw_parts(tx_ptr, tx_len as usize) };
    let now = SIM_NOW.load(Ordering::Relaxed);

    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "spi_xfer",
            value: id,
        });
    });

    let result = sim_devices::with_spi_mut(id, |spi| spi.transfer(tx_data));
    match result {
        Some(rx_data) => {
            let actual = rx_data.len().min(rx_len as usize);
            let buf = unsafe { std::slice::from_raw_parts_mut(rx_buf, actual) };
            buf.copy_from_slice(&rx_data[..actual]);

            // Fault injection: corrupt first byte if SPI error was injected
            if sim_devices::with_fault_injector_mut(|f| f.consume_spi_error()) {
                buf[0] ^= 0xFF;
            }

            actual as u32
        }
        None => 0,
    }
}

/// Set SPI configuration: mode (0-3), clock speed (Hz), and word size (8 or 16).
///
/// # Safety
///
/// Always safe — uses thread-local SPI storage.
#[no_mangle]
pub unsafe extern "C" fn sim_spi_set_config(
    id: u32,
    mode: u32,
    speed_hz: u32,
    word_size: u32,
) -> u32 {
    let spi_mode = match mode {
        0 => sim_devices::SpiMode::Mode0,
        1 => sim_devices::SpiMode::Mode1,
        2 => sim_devices::SpiMode::Mode2,
        3 => sim_devices::SpiMode::Mode3,
        _ => return 1, // invalid mode
    };
    if word_size != 8 && word_size != 16 {
        return 2; // invalid word size
    }
    sim_devices::with_spi_mut(id, |spi| {
        spi.set_mode(spi_mode);
        spi.speed_hz = speed_hz;
        spi.set_word_size(word_size as u8);
    });
    0
}

/// Set SPI chip select state.
///
/// Returns 0 on success, 1 if controller not found.
///
/// # Safety
///
/// Always safe — uses thread-local SPI storage.
#[no_mangle]
pub unsafe extern "C" fn sim_spi_set_cs(id: u32, active: u32) -> u32 {
    let found = sim_devices::with_spi_mut(id, |spi| {
        spi.set_cs(active != 0);
    });
    if found.is_some() {
        0
    } else {
        1
    }
}

/// Inject bytes into the SPI RX buffer (for test scripts).
///
/// This simulates an SPI peripheral device sending data to the master.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local SPI storage).
#[no_mangle]
pub unsafe extern "C" fn sim_spi_inject_rx(id: u32, data_ptr: *const u8, len: u32) {
    if data_ptr.is_null() || len == 0 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    sim_devices::with_spi_mut(id, |spi| {
        spi.inject_rx(data);
    });
}

// ---------------------------------------------------------------------------
// Virtual CAN C ABI exports (Phase 23)
// ---------------------------------------------------------------------------

/// Send a CAN frame from the specified controller.
///
/// If loopback mode is enabled on the controller, the frame is also
/// placed in the RX queue.  A `can_send` trace event is recorded.
///
/// Returns 0 on success, 1 if controller not found or send failed.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_send(
    ctrl_id: u32,
    can_id: u32,
    data_ptr: *const u8,
    len: u32,
    is_ext: u32,
    is_remote: u32,
) -> u32 {
    let dlc = len.min(8) as u8;
    let mut frame = if is_remote != 0 {
        sim_devices::CanFrame::new_remote(can_id, is_ext != 0)
    } else if is_ext != 0 {
        sim_devices::CanFrame::new_data_ext(can_id, &[])
    } else {
        sim_devices::CanFrame::new_data(can_id, &[])
    };
    frame.is_remote = is_remote != 0;
    frame.dlc = dlc;

    if !data_ptr.is_null() && len > 0 && is_remote == 0 {
        let data = unsafe { std::slice::from_raw_parts(data_ptr, dlc as usize) };
        frame.data[..dlc as usize].copy_from_slice(data);
    }

    let now = SIM_NOW.load(Ordering::Relaxed);
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "can_send",
            value: ctrl_id,
        });
    });

    let ok = sim_devices::with_can_mut(ctrl_id, |can| can.send(frame)).unwrap_or(false);
    if ok {
        0
    } else {
        1
    }
}

/// Receive the oldest CAN frame from the RX queue.
///
/// Writes the frame payload into `buf` (up to `buf_len` bytes).  Writes the
/// CAN ID into `can_id_out`, the extended flag into `is_ext_out` (1 = extended),
/// and the remote flag into `is_remote_out` (1 = RTR).  A `can_recv` trace
/// event is recorded.
///
/// Returns the data length (DLC) of the received frame, or 0 if no frame
/// is available or the controller is not found.
///
/// # Safety
///
/// `buf` must be writable for at least `buf_len` bytes.  `can_id_out`,
/// `is_ext_out`, and `is_remote_out` must be valid pointers to u32.
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_recv(
    ctrl_id: u32,
    buf: *mut u8,
    buf_len: u32,
    can_id_out: *mut u32,
    is_ext_out: *mut u32,
    is_remote_out: *mut u32,
) -> u32 {
    let now = SIM_NOW.load(Ordering::Relaxed);
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "can_recv",
            value: ctrl_id,
        });
    });

    let result = sim_devices::with_can_mut(ctrl_id, |can| can.recv());
    match result {
        Some(Some(frame)) => {
            if !can_id_out.is_null() {
                unsafe { *can_id_out = frame.id };
            }
            if !is_ext_out.is_null() {
                unsafe { *is_ext_out = frame.is_extended as u32 };
            }
            if !is_remote_out.is_null() {
                unsafe { *is_remote_out = frame.is_remote as u32 };
            }
            let actual = (frame.dlc as usize).min(buf_len as usize);
            if !buf.is_null() && actual > 0 {
                let out = unsafe { std::slice::from_raw_parts_mut(buf, actual) };
                out.copy_from_slice(&frame.data[..actual]);
            }
            frame.dlc as u32
        }
        _ => 0,
    }
}

/// Inject a CAN frame into the RX queue (simulates an external node).
///
/// Places a frame with the given ID, data, and flags into the controller's
/// RX queue.  A `can_inject` trace event is recorded.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_inject_rx(
    ctrl_id: u32,
    can_id: u32,
    data_ptr: *const u8,
    len: u32,
    is_ext: u32,
) {
    let dlc = len.min(8) as u8;
    let mut frame = if is_ext != 0 {
        sim_devices::CanFrame::new_data_ext(can_id, &[])
    } else {
        sim_devices::CanFrame::new_data(can_id, &[])
    };
    frame.dlc = dlc;

    if !data_ptr.is_null() && len > 0 {
        let data = unsafe { std::slice::from_raw_parts(data_ptr, dlc as usize) };
        frame.data[..dlc as usize].copy_from_slice(data);
    }

    let now = SIM_NOW.load(Ordering::Relaxed);
    TL_TRACE.with(|tl| {
        tl.borrow_mut().push(sim_core::trace::TraceEvent::UserU32 {
            at: now,
            label: "can_inject",
            value: ctrl_id,
        });
    });

    sim_devices::with_can_mut(ctrl_id, |can| can.inject_rx(frame));
}

/// Enable or disable loopback mode on a CAN controller.
///
/// In loopback mode, frames sent by the controller are automatically
/// copied to the RX queue.
///
/// Returns 0 on success, 1 if controller not found.
///
/// # Safety
///
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_set_loopback(ctrl_id: u32, enable: u32) -> u32 {
    sim_devices::with_can_mut(ctrl_id, |can| {
        can.loopback = enable != 0;
    })
    .map(|_| 0)
    .unwrap_or(1)
}

/// Get the error state of a CAN controller.
///
/// Returns: 0 = Error Active, 1 = Error Warning, 2 = Error Passive,
/// 3 = Bus Off, or u32::MAX if the controller is not found.
///
/// # Safety
///
/// Safe to call from any context (uses thread-local CAN storage).
#[no_mangle]
pub unsafe extern "C" fn sim_can_get_error(ctrl_id: u32) -> u32 {
    sim_devices::with_can(ctrl_id, |can| match can.error_state() {
        sim_devices::CanErrorState::ErrorActive => 0,
        sim_devices::CanErrorState::ErrorWarning => 1,
        sim_devices::CanErrorState::ErrorPassive => 2,
        sim_devices::CanErrorState::BusOff => 3,
    })
    .unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// Virtual Sensor C ABI exports (ADC + Temperature)
// ---------------------------------------------------------------------------

/// Read the ADC value for a specific channel.
///
/// Returns the pre-injected reading for the given channel of the ADC
/// identified by `id`.  If the ADC is not registered, returns 0.
///
/// # Safety
///
/// Always safe — uses thread-local ADC storage.
#[no_mangle]
pub unsafe extern "C" fn sim_adc_read(id: u32, channel: u32) -> u16 {
    sim_devices::with_adc_mut(id, |adc| {
        adc.set_channel(channel as usize);
        adc.read()
    })
    .unwrap_or(0)
}

/// Inject a reading for a specific ADC channel.
///
/// Sets the ADC reading for the given channel so that subsequent
/// `sim_adc_read` calls for that channel return `value`.
/// If the ADC is not registered, this is a no-op.
///
/// # Safety
///
/// Always safe — uses thread-local ADC storage.
#[no_mangle]
pub unsafe extern "C" fn sim_adc_inject_reading(id: u32, channel: u32, value: u16) {
    sim_devices::with_adc_mut(id, |adc| {
        adc.inject_reading(channel as usize, value);
    });
}

/// Set the ADC resolution in bits.
///
/// Valid values: 8, 10, 12, 16.  Invalid values are silently ignored.
/// If the ADC is not registered, this is a no-op.
///
/// # Safety
///
/// Always safe — uses thread-local ADC storage.
#[no_mangle]
pub unsafe extern "C" fn sim_adc_set_resolution(id: u32, bits: u32) {
    sim_devices::with_adc_mut(id, |adc| {
        adc.set_resolution(bits as u8);
    });
}

/// Read the current temperature from a virtual temperature sensor.
///
/// Returns the temperature in millidegrees Celsius (m°C), or 0 if the
/// sensor is not registered.  Default is 25000 (= 25.0 °C).
///
/// # Safety
///
/// Always safe — uses thread-local temperature sensor storage.
#[no_mangle]
pub unsafe extern "C" fn sim_temp_read(id: u32) -> i32 {
    sim_devices::with_temp_sensor(id, |sensor| sensor.read_milli_c()).unwrap_or(0)
}

/// Set the temperature of a virtual temperature sensor.
///
/// The value is in millidegrees Celsius (m°C):
///   - `25000` → 25.000 °C
///   - `-10000` → -10.000 °C
///
/// If the sensor is not registered, this is a no-op.
///
/// # Safety
///
/// Always safe — uses thread-local temperature sensor storage.
#[no_mangle]
pub unsafe extern "C" fn sim_temp_set_value(id: u32, milli_c: i32) {
    sim_devices::with_temp_sensor_mut(id, |sensor| {
        sensor.set_value(milli_c);
    });
}

// ---------------------------------------------------------------------------
// Virtual storage C ABI exports (EEPROM / Flash) — Phase 24
// ---------------------------------------------------------------------------

/// Read a byte from a virtual EEPROM at `addr`.
///
/// # Safety
///
/// Always safe — uses thread-local EEPROM storage.
#[no_mangle]
pub unsafe extern "C" fn sim_eeprom_read(id: u32, addr: u32) -> u32 {
    sim_devices::with_eeprom(id, |e| e.read(addr as usize).map(|b| b as u32))
        .flatten()
        .unwrap_or(u32::MAX)
}

/// Write a byte to a virtual EEPROM at `addr`.
///
/// # Safety
///
/// Always safe — uses thread-local EEPROM storage.
#[no_mangle]
pub unsafe extern "C" fn sim_eeprom_write(id: u32, addr: u32, byte: u32) -> u32 {
    let success = sim_devices::with_eeprom_mut(id, |e| e.write(addr as usize, (byte & 0xFF) as u8))
        .unwrap_or(false);
    if success {
        0
    } else {
        1
    }
}

/// Return the size of a virtual EEPROM in bytes.
///
/// # Safety
///
/// Always safe — uses thread-local EEPROM storage.
#[no_mangle]
pub unsafe extern "C" fn sim_eeprom_size(id: u32) -> u32 {
    sim_devices::with_eeprom(id, |e| e.size as u32).unwrap_or(0)
}

/// Read a byte from a virtual Flash device at `addr`.
///
/// # Safety
///
/// Always safe — uses thread-local Flash storage.
#[no_mangle]
pub unsafe extern "C" fn sim_flash_read(id: u32, addr: u32) -> u32 {
    sim_devices::with_flash(id, |f| f.read(addr as usize).map(|b| b as u32))
        .flatten()
        .unwrap_or(u32::MAX)
}

/// Write data to a virtual Flash page.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local Flash storage).
#[no_mangle]
pub unsafe extern "C" fn sim_flash_write(
    id: u32,
    page: u32,
    offset: u32,
    data_ptr: *const u8,
    len: u32,
) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    let success =
        sim_devices::with_flash_mut(id, |f| f.write_page(page as usize, offset as usize, data))
            .unwrap_or(false);
    if success {
        len
    } else {
        0
    }
}

/// Erase a page of virtual Flash memory.
///
/// # Safety
///
/// Always safe — uses thread-local Flash storage.
#[no_mangle]
pub unsafe extern "C" fn sim_flash_erase(id: u32, page: u32) -> u32 {
    let success = sim_devices::with_flash_mut(id, |f| f.erase_page(page as usize)).unwrap_or(false);
    if success {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Fault injection C ABI exports — Phase 24
// ---------------------------------------------------------------------------

/// Inject an I2C NACK fault on the next read.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_inject_i2c_nack() {
    sim_devices::with_fault_injector_mut(|f| f.inject_i2c_nack());
}

/// Inject an SPI data/CRC error on the next transfer.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_inject_spi_error() {
    sim_devices::with_fault_injector_mut(|f| f.inject_spi_error());
}

/// Inject a CAN bus error on the next send.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_inject_can_error() {
    sim_devices::with_fault_injector_mut(|f| f.inject_can_error());
}

/// Clear all injected faults.
///
/// # Safety
///
/// Always safe — uses thread-local fault injector storage.
#[no_mangle]
pub unsafe extern "C" fn sim_fault_clear() {
    sim_devices::with_fault_injector_mut(|f| f.clear_all());
}

// ---------------------------------------------------------------------------
// Virtual entropy C ABI exports — Phase 30
// ---------------------------------------------------------------------------

/// Fill a buffer with deterministic pseudo-random bytes.
///
/// Writes up to `len` bytes into the buffer pointed to by `buf_ptr`.
/// Returns the number of bytes actually written (always `len` on success,
/// 0 if the entropy source is not registered).
///
/// # Safety
///
/// `buf_ptr` must be a valid pointer to at least `len` bytes of writable
/// memory.
#[no_mangle]
pub unsafe extern "C" fn sim_entropy_request(id: u32, buf_ptr: *mut u8, len: u32) -> u32 {
    if buf_ptr.is_null() || len == 0 {
        return 0;
    }

    sim_devices::with_entropy_mut(id, |ent| {
        let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, len as usize) };
        ent.request_bytes(buf) as u32
    })
    .unwrap_or(0)
}

/// Reseed the virtual entropy source.
///
/// Subsequent calls to `sim_entropy_request` produce a different byte
/// sequence for the same device.  If the entropy source is not registered,
/// this is a no-op.
///
/// # Safety
///
/// Always safe — uses thread-local entropy storage.
#[no_mangle]
pub unsafe extern "C" fn sim_entropy_seed(id: u32, seed: u64) {
    sim_devices::with_entropy_mut(id, |ent| {
        ent.seed(seed);
    });
}

// ── Virtual Display C ABI ─────────────────────────────────────────────

/// Initialize a virtual display with the given dimensions and color mode.
/// color_mode: 0=RGB565, 1=RGB888, 2=ARGB8888
#[no_mangle]
pub extern "C" fn sim_display_init(id: u32, width: u16, height: u16, color_mode: u32) -> u32 {
    let mode = match color_mode {
        0 => sim_devices::DisplayColorMode::Rgb565,
        1 => sim_devices::DisplayColorMode::Rgb888,
        2 => sim_devices::DisplayColorMode::Argb8888,
        _ => return 1, // error
    };
    sim_devices::display_insert(sim_devices::VirtualDisplay::new(id, width, height, mode));
    0
}

/// Set a single pixel on the display. Returns 0 on success, 1 if out of bounds.
#[no_mangle]
pub extern "C" fn sim_display_set_pixel(id: u32, x: u16, y: u16, color: u32) -> u32 {
    sim_devices::with_display_mut(id, |d| {
        if x < d.width && y < d.height {
            d.set_pixel(x, y, color);
            0
        } else {
            1
        }
    })
    .unwrap_or(1)
}

/// Fill a rectangle on the display. Returns 0 on success.
#[no_mangle]
pub extern "C" fn sim_display_fill_rect(
    id: u32,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    color: u32,
) -> u32 {
    sim_devices::with_display_mut(id, |d| {
        d.fill_rect(x, y, w, h, color);
        0
    })
    .unwrap_or(1)
}

/// Draw a bitmap on the display. Returns bytes copied.
///
/// # Safety
///
/// `data` must point to at least `data_len` bytes of valid memory.
#[no_mangle]
pub unsafe extern "C" fn sim_display_draw_bitmap(
    id: u32,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    data: *const u8,
    data_len: u32,
) -> u32 {
    if data.is_null() {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(data, data_len as usize) };
    sim_devices::with_display_mut(id, |d| {
        d.draw_bitmap(x, y, w, h, slice);
        data_len
    })
    .unwrap_or(0)
}

/// Enable or disable the display. 0=disable, 1=enable.
#[no_mangle]
pub extern "C" fn sim_display_enable(id: u32, enable: u32) {
    sim_devices::with_display_mut(id, |d| {
        d.enabled = enable != 0;
    });
}

/// Set the backlight level (0-100).
#[no_mangle]
pub extern "C" fn sim_display_set_backlight(id: u32, level: u32) {
    sim_devices::with_display_mut(id, |d| {
        d.backlight = (level.min(100)) as u8;
    });
}

/// Get display width, or 0 if not found.
#[no_mangle]
pub extern "C" fn sim_display_get_width(id: u32) -> u16 {
    sim_devices::with_display(id, |d| d.width).unwrap_or(0)
}

/// Get display height, or 0 if not found.
#[no_mangle]
pub extern "C" fn sim_display_get_height(id: u32) -> u16 {
    sim_devices::with_display(id, |d| d.height).unwrap_or(0)
}

// ── Virtual Touch Screen C ABI ────────────────────────────────────────

/// Initialize a touch screen. Returns 0 on success.
#[no_mangle]
pub extern "C" fn sim_touch_init(id: u32, display_id: u32) -> u32 {
    sim_devices::touch_insert(sim_devices::VirtualTouchScreen::new(id, display_id));
    0
}

/// Read the next touch event. Returns 1 if an event was read, 0 if queue empty.
/// Writes to out_point_id, out_x, out_y, out_pressure, out_type.
/// out_type: 0=Press, 1=Release, 2=Move
///
/// # Safety
///
/// All output pointers must be valid for writing.
#[no_mangle]
pub unsafe extern "C" fn sim_touch_get_event(
    id: u32,
    out_point_id: *mut u32,
    out_x: *mut u16,
    out_y: *mut u16,
    out_pressure: *mut u8,
    out_type: *mut u32,
) -> u32 {
    let mut event = sim_devices::TouchEvent {
        point_id: 0,
        x: 0,
        y: 0,
        pressure: 0,
        event_type: sim_devices::TouchEventType::Press,
    };
    let got = sim_devices::with_touch_mut(id, |t| t.get_event(&mut event)).unwrap_or(false);
    // SAFETY: Caller guarantees pointer arguments are valid for writing.
    if got {
        unsafe {
            if let Some(p) = out_point_id.as_mut() {
                *p = event.point_id;
            }
            if let Some(p) = out_x.as_mut() {
                *p = event.x;
            }
            if let Some(p) = out_y.as_mut() {
                *p = event.y;
            }
            if let Some(p) = out_pressure.as_mut() {
                *p = event.pressure;
            }
            if let Some(p) = out_type.as_mut() {
                *p = match event.event_type {
                    sim_devices::TouchEventType::Press => 0,
                    sim_devices::TouchEventType::Release => 1,
                    sim_devices::TouchEventType::Move => 2,
                };
            }
        }
        1
    } else {
        0
    }
}

/// Get the number of pending touch events.
#[no_mangle]
pub extern "C" fn sim_touch_pending_count(id: u32) -> u32 {
    sim_devices::with_touch(id, |t| t.pending_count() as u32).unwrap_or(0)
}

// ── Virtual Block Device C ABI ──────────────────────────────────────

/// Create a new virtual block device. Returns 0 on success.
#[no_mangle]
pub extern "C" fn sim_block_create(
    id: u32,
    page_size: u32,
    page_count: u32,
    erase_value: u8,
) -> u32 {
    sim_devices::block_insert(sim_devices::FlatMemoryStore::new(
        id,
        page_size,
        page_count,
        erase_value,
    ));
    0
}

/// Read from the block device at an absolute offset.
/// Writes up to `len` bytes into `buf`. Returns bytes actually read.
///
/// # Safety
///
/// `buf` must be a valid pointer with at least `len` bytes of writable memory.
#[no_mangle]
pub unsafe extern "C" fn sim_block_read(id: u32, offset: u32, buf: *mut u8, len: u32) -> u32 {
    if buf.is_null() {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(buf, len as usize) };
    sim_devices::with_block(id, |b| b.read(offset, out)).unwrap_or(0)
}

/// Write to the block device at an absolute offset.
/// Target locations must be erased before writing.
/// Returns the number of bytes actually written.
///
/// # Safety
///
/// `data` must be a valid pointer with at least `len` bytes of readable memory.
#[no_mangle]
pub unsafe extern "C" fn sim_block_write(id: u32, offset: u32, data: *const u8, len: u32) -> u32 {
    if data.is_null() {
        return 0;
    }
    let input = unsafe { std::slice::from_raw_parts(data, len as usize) };
    sim_devices::with_block_mut(id, |b| b.write(offset, input)).unwrap_or(0)
}

/// Erase the page containing the given absolute offset.
/// Sets all bytes in that page to the erase_value.
#[no_mangle]
pub extern "C" fn sim_block_erase_page(id: u32, offset: u32) {
    sim_devices::with_block_mut(id, |b| {
        if offset < b.total_size() {
            b.erase_page(offset);
        }
    });
}

/// Get geometry of the block device.
/// Writes page_size and page_count to the output pointers.
///
/// # Safety
///
/// `out_page_size` and `out_page_count` must be valid pointers for writing.
#[no_mangle]
pub unsafe extern "C" fn sim_block_get_geometry(
    id: u32,
    out_page_size: *mut u32,
    out_page_count: *mut u32,
) {
    if let Some(geometry) = sim_devices::with_block(id, |b| (b.page_size, b.page_count)) {
        if !out_page_size.is_null() {
            unsafe {
                *out_page_size = geometry.0;
            }
        }
        if !out_page_count.is_null() {
            unsafe {
                *out_page_count = geometry.1;
            }
        }
    }
}
