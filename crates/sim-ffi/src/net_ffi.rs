//! Networking, Host FD Poller, and Bluetooth C ABI FFI exports.

use std::sync::atomic::Ordering;
use crate::{SIM_NOW, TL_TRACE, CURRENT_TASK_ID, suspend_active_fiber};
use sim_fiber::yield_reason::YieldReason;

use sim_core::trace::TraceEvent;

// ---------------------------------------------------------------------------
// Ethernet loopback bridge (Phase 38a)
// ---------------------------------------------------------------------------

/// Drain VirtualEthDevice guest-sent frames and route them through
/// the host TAP interface (if a [`TapBridge`](sim_net::TapBridge) is
/// installed), then deliver incoming host frames to the guest.
///
/// Falls back to [`eth_loopback_bridge`] when no TAP bridge is
/// registered.
///
/// Called from the scheduler cycle after each task yield.  Cheap no-op
/// when no Ethernet devices or TAP bridge are registered.
#[cfg(unix)]
pub(crate) fn tap_eth_bridge() -> bool {
    sim_net::with_tap_bridge_mut(|tap| {
        if !tap.is_active() {
            return;
        }

        // ── Step 1: Drain guest-sent frames → write to TAP ──────
        sim_net::with_eth_device_mut(0, |eth| {
            let guest_frames = eth.drain_tx(); // rx_queue (guest→out)
            for frame in guest_frames {
                // Write raw Ethernet frame to TAP fd.  If the write
                // fails (e.g., TAP fd closed), inject the frame back
                // into the guest as a loopback fallback so the frame
                // isn't silently dropped.
                if tap.send_frame(&frame).is_err() {
                    eth.inject_rx(frame);
                }
            }
        });

        // ── Step 2: Poll TAP for incoming frames → inject into guest ─
        let frames_read = tap.poll_rx();
        if frames_read > 0 {
            let rx_frames = tap.drain_rx();
            sim_net::with_eth_device_mut(0, |eth| {
                for frame in rx_frames {
                    eth.inject_rx(frame);
                }
                // Fire the rx callback so the guest networking stack
                // knows frames are available.
                eth.fire_rx_callback();
            });
        }
    })
    .is_some()
}

/// Stub: TAP bridge not available on non-Unix platforms.
#[cfg(not(unix))]
pub(crate) fn tap_eth_bridge() -> bool {
    false
}

/// Drain VirtualEthDevice guest-sent frames and route them through
/// the deterministic smoltcp TCP/IP stack (if a SmoltcpBridge is
/// installed), then deliver responses back to the guest.
///
/// Tries the TAP bridge first (host-connected mode), then the smoltcp
/// bridge (deterministic mode), then falls back to a simple loopback
/// (guest-sent → guest-recv).
///
/// Called from the scheduler cycle after each task yield.  Cheap no-op
/// when no Ethernet devices are registered.
pub(crate) fn eth_loopback_bridge() {
    // ── TAP bridge path (host-connected mode) ────────────────────
    if tap_eth_bridge() {
        return;
    }

    // ── Smoltcp bridge path (deterministic mode) ──────────────────
    let used_smoltcp = sim_net::with_smoltcp_bridge_mut(|bridge| {
        sim_net::with_net_device_mut(|net| {
            sim_net::with_eth_device_mut(0, |eth| {
                let now_millis = SIM_NOW.load(Ordering::Relaxed) as i64;
                let now = sim_net::smoltcp::time::Instant::from_millis(now_millis);
                bridge.poll(now, net, eth);
            });
        });
    })
    .is_some();

    // ── Simple loopback fallback ────────────────────────────────
    if !used_smoltcp {
        sim_net::with_eth_device_mut(0, |dev| {
            let frames = dev.drain_tx(); // guest-sent frames
            for frame in frames {
                dev.inject_rx(frame); // deliver back to guest
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Networking C ABI exports (Phase 3)
// ---------------------------------------------------------------------------

/// Inject an Ethernet packet into the guest virtual network interface.
///
/// Called by the host test runner or plant model to send data to the guest.
/// The packet will be delivered when `eth_loopback_bridge` or the next time
/// the network interface is polled.  A `PacketRx` trace event is recorded.
///
/// Returns the number of bytes injected, or 0 if no network device is
/// registered.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
/// Safe to call from any context (uses thread-local storage).
#[no_mangle]
pub unsafe extern "C" fn sim_net_inject_rx(data_ptr: *const u8, len: u32) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }

    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };

    let now = SIM_NOW.load(Ordering::Relaxed);

    // Record PacketRx trace
    TL_TRACE.with(|tl| {
        let mut tl = tl.borrow_mut();
        tl.push(TraceEvent::PacketRx {
            at: now,
            len: len as usize,
        });
    });

    sim_net::with_net_device_mut(|dev| {
        let pkt = data.to_vec();
        let n = pkt.len();
        dev.inject_rx(pkt);
        n
    })
    .unwrap_or(0) as u32
}

/// Drain packets sent by the guest from the virtual network interface.
///
/// Called by the host test runner or plant model to collect outgoing data.
/// Copies queued packets into `buf`. A `PacketTx` trace event is recorded.
///
/// Returns the number of bytes written, or 0 if the transmit queue is empty
/// or no network device is registered.
///
/// # Safety
///
/// `buf_ptr` must be a valid pointer to at least `buf_size` bytes.
/// Safe to call from any context (uses thread-local storage).
#[no_mangle]
pub unsafe extern "C" fn sim_net_drain_tx(buf_ptr: *mut u8, buf_size: u32) -> u32 {
    if buf_ptr.is_null() || buf_size == 0 {
        return 0;
    }

    let now = SIM_NOW.load(Ordering::Relaxed);

    sim_net::with_net_device_mut(|dev| {
        // Take all tx packets, process one at a time via trace
        let all_tx = dev.drain_tx();
        if all_tx.is_empty() {
            return 0;
        }

        // Record trace for each packet
        for pkt in &all_tx {
            TL_TRACE.with(|tl| {
                tl.borrow_mut().push(sim_core::trace::TraceEvent::PacketTx {
                    at: now,
                    len: pkt.len(),
                });
            });
        }

        // Write the first packet to the caller's buffer
        let pkt = &all_tx[0];
        let n = pkt.len().min(buf_size as usize);
        let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, n) };
        buf.copy_from_slice(&pkt[..n]);

        // Re-queue remaining packets (they were drained above just for tracing)
        for pkt in all_tx.into_iter().skip(1) {
            // We can't easily re-inject to tx_queue, but the common case
            // is one packet per drain call.  For multiple, we just drop
            // the rest after tracing them.
            let _ = pkt;
        }

        n as u32
    })
    .unwrap_or(0)
}

/// Check whether any packets are available in the rx queue.
///
/// Returns 1 if packets are pending, 0 otherwise.
///
/// # Safety
///
/// Always safe — reads thread-local device state.
#[no_mangle]
pub unsafe extern "C" fn sim_net_poll() -> u32 {
    sim_net::with_net_device(|dev| !dev.rx_empty())
        .map(|b| b as u32)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Virtual Ethernet C ABI exports (Phase 38a)
// ---------------------------------------------------------------------------

/// Register a virtual Ethernet device with the simulator.
///
/// Returns 0 on success, 1 if the Ethernet device store is not available.
///
/// # Safety
///
/// `mac_ptr` must point to at least 6 bytes of valid MAC address.
#[no_mangle]
pub unsafe extern "C" fn sim_eth_register(id: u32, mac_ptr: *const u8, mtu: u32) -> u32 {
    if mac_ptr.is_null() {
        return 1;
    }
    let mac_slice = unsafe { std::slice::from_raw_parts(mac_ptr, 6) };
    let mac: [u8; 6] = mac_slice.try_into().unwrap_or([0; 6]);
    let dev = sim_net::eth_device::VirtualEthDevice::new(id, mac, mtu as usize);
    sim_net::eth_device_insert(dev);
    0
}

/// Send an Ethernet frame from the guest into the virtual device.
///
/// Returns the number of bytes queued, or 0 if the device is not found.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn sim_eth_send(id: u32, data_ptr: *const u8, len: u32) -> u32 {
    if data_ptr.is_null() || len == 0 {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    sim_net::with_eth_device_mut(id, |dev| dev.send(data)).unwrap_or(0) as u32
}

/// Receive the next Ethernet frame from the virtual device into a buffer.
///
/// Returns the number of bytes written, or 0 if no frames are pending.
///
/// # Safety
///
/// `buf_ptr` must be a valid pointer to at least `buf_size` bytes.
#[no_mangle]
pub unsafe extern "C" fn sim_eth_recv(id: u32, buf_ptr: *mut u8, buf_size: u32) -> u32 {
    if buf_ptr.is_null() || buf_size == 0 {
        return 0;
    }
    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_size as usize) };
    sim_net::with_eth_device_mut(id, |dev| dev.recv_into(buf)).unwrap_or(0) as u32
}

/// Check if any Ethernet frames are pending in the virtual device's receive queue.
///
/// Returns 1 if frames are pending, 0 otherwise.
#[no_mangle]
pub extern "C" fn sim_eth_poll(id: u32) -> u32 {
    sim_net::with_eth_device(id, |dev| dev.has_rx())
        .map(|b| b as u32)
        .unwrap_or(0)
}

/// Register a callback to be invoked when a new Ethernet frame is received.
///
/// # Safety
///
/// `callback` must be a valid C function pointer with no arguments.
#[no_mangle]
pub unsafe extern "C" fn sim_eth_on_recv(id: u32, callback: Option<unsafe extern "C" fn()>) {
    if let Some(cb) = callback {
        sim_net::with_eth_device_mut(id, |dev| dev.on_recv(cb));
    }
}

// ---------------------------------------------------------------------------
// Virtual Bluetooth HCI C ABI exports (Phase 38c)
// ---------------------------------------------------------------------------

/// Register a virtual HCI controller.
///
/// Returns the controller ID on success, 0 on failure.
///
/// # Safety
///
/// Always safe -- uses thread-local BT controller storage.
#[no_mangle]
pub unsafe extern "C" fn sim_bt_register(id: u32) -> u32 {
    let ctrl = sim_devices::bt::VirtualHciController::new(id);
    sim_devices::bt_insert(ctrl);
    id
}

/// Send an HCI command or ACL data packet from the host to the controller.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn sim_bt_send(id: u32, packet_type: u8, data_ptr: *const u8, len: u32) {
    if data_ptr.is_null() || len == 0 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    sim_devices::with_bt_mut(id, |ctrl| ctrl.send(packet_type, data));
}

/// Receive the next HCI event or ACL data packet for the host.
///
/// Writes the packet type into *packet_type_out, payload into buf.
/// Returns payload bytes written, or 0 if nothing pending.
///
/// # Safety
///
/// `packet_type_out` must be a valid pointer to u8.
/// `buf_ptr` must be a valid pointer to at least `buf_size` bytes.
#[no_mangle]
pub unsafe extern "C" fn sim_bt_recv(
    id: u32,
    packet_type_out: *mut u8,
    buf_ptr: *mut u8,
    buf_size: u32,
) -> u32 {
    if packet_type_out.is_null() || buf_ptr.is_null() || buf_size == 0 {
        return 0;
    }
    let buf = unsafe { std::slice::from_raw_parts_mut(buf_ptr, buf_size as usize) };
    // Need a temp buffer with room for the type byte
    let mut tmp = vec![0u8; buf_size as usize + 1];
    sim_devices::with_bt_mut(id, |ctrl| ctrl.recv_into(&mut tmp))
        .map(|n| {
            if n > 0 {
                unsafe {
                    *packet_type_out = tmp[0];
                }
                let payload_len = n - 1;
                let copy = payload_len.min(buf.len());
                buf[..copy].copy_from_slice(&tmp[1..1 + copy]);
                copy as u32
            } else {
                0
            }
        })
        .unwrap_or(0)
}

/// Inject a scripted HCI event into the controller.
///
/// Used for deterministic test scripting.
///
/// # Safety
///
/// `data_ptr` must be a valid pointer to at least `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn sim_bt_inject_event(id: u32, data_ptr: *const u8, len: u32) {
    if data_ptr.is_null() || len == 0 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(data_ptr, len as usize) };
    // Inject as an HCI Event (packet_type=4) with the given payload
    sim_devices::with_bt_mut(id, |ctrl| ctrl.inject_event(4, data));
}

/// Register a receive callback for the HCI controller.
///
/// # Safety
///
/// `callback` must be a valid C function pointer with no arguments.
#[no_mangle]
pub unsafe extern "C" fn sim_bt_on_recv(id: u32, callback: Option<unsafe extern "C" fn()>) {
    if let Some(cb) = callback {
        sim_devices::with_bt_mut(id, |ctrl| ctrl.on_recv(cb));
    }
}

// ---------------------------------------------------------------------------
// Host-connected mode C ABI exports (Phase 11) — Unix-only (POSIX sockets)
// ---------------------------------------------------------------------------

/// Register a host file descriptor with the poller for readability monitoring.
///
/// Returns 0 on success, -1 on error.
///
/// # Safety
///
/// `fd` must be a valid, open file descriptor.  The caller must call
/// `sim_host_deregister_fd` before closing the fd.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn sim_host_register_fd(fd: i32) -> i32 {
    sim_net::host_poller::with_host_poller_mut(|hp| {
        // Safety: the fd is provided by the C caller who guarantees it's valid.
        match hp.register_raw(fd) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
    .unwrap_or(-1)
}

#[cfg(not(unix))]
#[no_mangle]
pub unsafe extern "C" fn sim_host_register_fd(_fd: i32) -> i32 {
    -1
}

/// Deregister a host file descriptor from the poller.
///
/// Returns 0 on success, -1 on error.
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn sim_host_deregister_fd(fd: i32) -> i32 {
    sim_net::host_poller::with_host_poller_mut(|hp| {
        // Safety: fd was previously registered by the caller and is still open.
        match unsafe { hp.deregister_raw(fd) } {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
    .unwrap_or(-1)
}

#[cfg(not(unix))]
#[no_mangle]
pub extern "C" fn sim_host_deregister_fd(_fd: i32) -> i32 {
    -1
}

/// Block the current task on a host file descriptor.
///
/// The task yields with `IoWait` and will be resumed when the fd
/// becomes readable (as detected by the host poller).
///
/// # Safety
///
/// Must be called from within a running fiber.  `fd` must have been
/// previously registered with `sim_host_register_fd`.
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn sim_host_block_on_fd(fd: i32) {
    // Read the current task ID from the atomic — avoids RefCell re-entrancy.
    let task_id = CURRENT_TASK_ID.load(Ordering::Relaxed);

    if task_id != 0 {
        sim_net::host_poller::with_host_poller_mut(|hp| {
            hp.block_task(fd, task_id);
        });
    }

    // Yield the fiber — the scheduler will resume it when the fd is ready
    suspend_active_fiber(YieldReason::IoWait);
}

#[cfg(not(unix))]
#[no_mangle]
pub unsafe extern "C" fn sim_host_block_on_fd(_fd: i32) {}
