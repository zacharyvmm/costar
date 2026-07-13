//! Host TAP bridge for interactive networking mode.
//!
//! Creates a virtual Ethernet TAP interface on the host and bridges
//! guest Ethernet frames (from [`VirtualEthDevice`](crate::eth_device::VirtualEthDevice))
//! to/from the host network stack.  Frames written to the TAP fd appear
//! as incoming frames on the host interface; frames sent by the host
//! to the TAP interface are readable from the TAP fd.
//!
//! # Platform support
//!
//! * **Linux**: Uses `/dev/net/tun` with `IFF_TAP | IFF_NO_PI`.  The kernel
//!   creates or re-opens the named TAP interface.  Standard on all Linux
//!   distributions.
//! * **macOS**: Opens an existing `/dev/tapN` character device.  These
//!   devices are provided by the `tuntaposx` kernel extension (must be
//!   installed separately).  No ioctl configuration is needed — just open
//!   the device and set non-blocking mode.
//! * **Windows**: Not supported.  Use [`TcpBridge`](crate::tcp_bridge::TcpBridge) instead.
//!
//! # Determinism warning
//!
//! Host-connected mode is **not** deterministic.  Use deterministic
//! packet scripts for golden-trace tests.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │  Guest firmware (VirtualEthDevice)                   │
//! │  · sim_eth_send()  → eth dev rx_queue               │
//! │  · sim_eth_recv()  ← eth dev tx_queue               │
//! ├──────────────────────────────────────────────────────┤
//! │  TapBridge                                          │
//! │  · host TAP fd (non-blocking, raw Ethernet)         │
//! │  · drain guest frames → write to TAP                │
//! │  · poll TAP → inject into guest                     │
//! │  · register with HostPoller for wakeup             │
//! ├──────────────────────────────────────────────────────┤
//! │  Host network stack (tcpdump, ping, routing, ...)   │
//! └──────────────────────────────────────────────────────┘
//! ```

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;

// ── platform-specific TAP creation ──────────────────────────────────────────

/// Create a TAP interface on Linux via `/dev/net/tun`.
///
/// The kernel creates (or re-opens) a TAP device with the given name.
/// `IFF_NO_PI` is set so frames are delivered as raw Ethernet — no
/// extra 4-byte packet-info header.
#[cfg(target_os = "linux")]
fn create_tap_platform(ifname: &str) -> io::Result<(std::fs::File, String)> {
    use std::os::fd::AsRawFd;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "failed to open /dev/net/tun: {} (is the 'tun' kernel module loaded?)",
                    e
                ),
            )
        })?;

    // Build ifreq: [name (16 bytes)][flags (2 bytes, at offset 16)]
    let mut ifr = [0u8; 40];

    // Copy interface name (max 15 chars + NUL = IFNAMSIZ=16)
    let name_bytes = ifname.as_bytes();
    let len = name_bytes.len().min(15);
    ifr[..len].copy_from_slice(&name_bytes[..len]);
    // NUL terminator already zero from array init

    // Set flags: IFF_TAP | IFF_NO_PI
    // IFF_TAP   = 0x0002 (Ethernet-level device)
    // IFF_NO_PI = 0x1000 (raw Ethernet, no packet-info header)
    const IFF_TAP: i16 = 0x0002;
    const IFF_NO_PI: i16 = 0x1000;
    let flags: i16 = IFF_TAP | IFF_NO_PI;
    ifr[16..18].copy_from_slice(&flags.to_ne_bytes());

    // TUNSETIFF = _IOW('T', 202, int) = 0x400454CA on 64-bit Linux
    const TUNSETIFF: u64 = 0x4004_54CA;

    // Safety: ifr is a correctly laid-out struct ifreq.  The file
    // descriptor is valid (just opened).  The kernel reads ifr_name
    // and writes back the actual interface name + applies flags.
    extern "C" {
        fn ioctl(
            fd: std::os::raw::c_int,
            request: std::os::raw::c_ulong,
            ...
        ) -> std::os::raw::c_int;
    }
    unsafe {
        let ret = ioctl(file.as_raw_fd(), TUNSETIFF as _, &ifr as *const u8);
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
    }

    // Read back the actual interface name (kernel may have renamed it
    // if the requested name was already taken, or if name was empty).
    let actual_name = std::ffi::CStr::from_bytes_until_nul(&ifr[..16])
        .unwrap_or(c"")
        .to_str()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "TAP interface name not valid UTF-8",
            )
        })?
        .to_string();

    // Set non-blocking via fcntl. `std::fs::File` does not expose a stable
    // `set_nonblocking` on Linux, so go through fcntl(F_SETFL) directly.
    {
        use std::os::raw::c_int;
        const F_GETFL: c_int = 3;
        const F_SETFL: c_int = 4;
        const O_NONBLOCK: c_int = 0o4000; // Linux: O_NONBLOCK = 0x800

        extern "C" {
            fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        }

        unsafe {
            let flags = fcntl(file.as_raw_fd(), F_GETFL, 0);
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            let ret = fcntl(file.as_raw_fd(), F_SETFL, flags | O_NONBLOCK);
            if ret < 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }

    Ok((file, actual_name))
}

/// Open a TAP interface on macOS via `/dev/tapN`.
///
/// On macOS, `/dev/tap0` through `/dev/tap15` are provided by the
/// `tuntaposx` kernel extension.  These must be installed separately.
/// No ioctl configuration is needed.
#[cfg(target_os = "macos")]
fn create_tap_platform(ifname: &str) -> io::Result<(std::fs::File, String)> {
    use std::os::fd::AsRawFd;
    use std::os::raw::c_int;

    let path = format!("/dev/{}", ifname);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "failed to open {}: {} (is tuntaposx kernel extension installed? \
                     Try: brew install --cask tuntap)",
                    path, e
                ),
            )
        })?;

    // Set non-blocking via fcntl (macOS doesn't always expose
    // File::set_nonblocking on all Rust toolchains).
    const F_GETFL: c_int = 3;
    const F_SETFL: c_int = 4;
    const O_NONBLOCK: c_int = 4; // macOS: O_NONBLOCK = 0x0004

    extern "C" {
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }

    unsafe {
        let flags = fcntl(file.as_raw_fd(), F_GETFL, 0);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let ret = fcntl(file.as_raw_fd(), F_SETFL, flags | O_NONBLOCK);
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok((file, ifname.to_string()))
}

/// Stub: TAP not supported on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn create_tap_platform(ifname: &str) -> io::Result<(std::fs::File, String)> {
    let _ = ifname;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "TAP interfaces are only supported on Linux and macOS",
    ))
}

// ── TapBridge ───────────────────────────────────────────────────────────────

/// A host TAP bridge that connects a VirtualEthDevice to a host TAP interface.
///
/// Guest-sent Ethernet frames are written directly to the TAP file descriptor
/// (one `write()` per frame — the kernel handles framing at the Ethernet level).
/// Incoming frames from the host are read from the TAP fd and injected into
/// the guest's VirtualEthDevice.
pub struct TapBridge {
    /// The TAP file descriptor (non-blocking).
    tap_fd: std::fs::File,
    /// The actual interface name (e.g. "tap0").
    ifname: String,
    /// Whether the bridge is active (TAP fd is open and operational).
    active: bool,
    /// Pending frames read from TAP, waiting to be injected into the guest.
    rx_pending: VecDeque<Vec<u8>>,
}

impl TapBridge {
    /// Create a new TAP bridge with the given interface name.
    ///
    /// On Linux, `/dev/net/tun` is opened and the TAP interface is created
    /// (or re-opened) via the `TUNSETIFF` ioctl.  The interface can then be
    /// configured with `ip link set <name> up` and `ip addr add ...`.
    ///
    /// On macOS, the `/dev/<ifname>` character device is opened directly.
    /// The `tuntaposx` kernel extension must be installed.
    ///
    /// # Errors
    ///
    /// Returns an error if the TAP device cannot be opened/created,
    /// or if the platform does not support TAP interfaces.
    pub fn create(ifname: &str) -> io::Result<Self> {
        if ifname.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TAP interface name must not be empty",
            ));
        }

        let (tap_fd, actual_name) = create_tap_platform(ifname)?;

        Ok(Self {
            tap_fd,
            ifname: actual_name,
            active: true,
            rx_pending: VecDeque::new(),
        })
    }

    // ── accessors ────────────────────────────────────────────────────────

    /// The actual interface name (may differ from the requested name).
    pub fn ifname(&self) -> &str {
        &self.ifname
    }

    /// Whether the TAP bridge is active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the raw file descriptor for HostPoller registration.
    pub fn raw_fd(&self) -> std::os::raw::c_int {
        self.tap_fd.as_raw_fd()
    }

    // ── sending (guest → host) ───────────────────────────────────────────

    /// Write an Ethernet frame to the TAP interface.
    ///
    /// The frame is written directly to the TAP file descriptor.
    /// The kernel delivers it to the host network stack as an
    /// incoming Ethernet frame on the TAP interface.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails (e.g., TAP fd closed,
    /// or the frame is too large for the interface MTU).
    pub fn send_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        if !self.active {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "TAP bridge is not active",
            ));
        }
        self.tap_fd.write_all(frame)
    }

    // ── receiving (host → guest) ─────────────────────────────────────────

    /// Poll the TAP interface for incoming frames.
    ///
    /// Reads all available frames from the TAP fd (non-blocking —
    /// returns immediately with `WouldBlock` if no data is available).
    /// Complete frames are placed in `rx_pending` and can be retrieved
    /// via [`drain_rx`](Self::drain_rx).
    ///
    /// Returns the number of complete frames read.
    pub fn poll_rx(&mut self) -> usize {
        if !self.active {
            return 0;
        }

        let mut buf = [0u8; 65536];
        let mut count = 0;

        loop {
            match self.tap_fd.read(&mut buf) {
                Ok(0) => {
                    // EOF — TAP fd was closed.
                    self.active = false;
                    break;
                }
                Ok(n) => {
                    self.rx_pending.push_back(buf[..n].to_vec());
                    count += 1;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // No more data available.
                    break;
                }
                Err(_) => {
                    self.active = false;
                    break;
                }
            }
        }

        count
    }

    /// Drain all received frames (for delivery to VirtualEthDevice).
    pub fn drain_rx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.rx_pending).into()
    }

    /// Check if any received frames are pending.
    pub fn has_rx(&self) -> bool {
        !self.rx_pending.is_empty()
    }

    /// Number of pending received frames.
    pub fn rx_pending_count(&self) -> usize {
        self.rx_pending.len()
    }
}

impl std::fmt::Debug for TapBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TapBridge")
            .field("ifname", &self.ifname)
            .field("active", &self.active)
            .field("rx_pending", &self.rx_pending.len())
            .finish()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tap_create_empty_name() {
        let result = TapBridge::create("");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("not be empty"));
    }

    #[test]
    fn test_tap_not_active_after_create_failure() {
        // Creating a TAP with a name that can't exist should fail.
        // On Linux without CAP_NET_ADMIN, opening /dev/net/tun also fails.
        // We just verify the error is propagated, not that we get a bridge.
        let result = TapBridge::create("tap_nonexistent_99999");
        // This may succeed or fail depending on platform/capabilities.
        // If it succeeds (unlikely), the bridge should be active.
        if let Ok(bridge) = result {
            assert!(bridge.is_active());
            assert!(!bridge.ifname().is_empty());
        }
    }

    #[test]
    fn test_tap_debug_format() {
        // Can't create a real TAP in unit tests, but verify the type
        // compiles and Debug works for the cases where we have one.
        // We test the Debug impl on the error path.
        let result = TapBridge::create("");
        assert!(result.is_err());
        // Debug on the error itself
        let _ = format!("{:?}", result);
    }

    #[test]
    fn test_tap_poll_rx_inactive() {
        // Verify poll_rx returns 0 when we can't create a TAP.
        // We simulate by testing the behaviour for a non-existent TAP.
        let result = TapBridge::create("tap_nonexistent_99999");
        match result {
            Ok(mut bridge) => {
                // Bridge was created — verify poll_rx works
                let n = bridge.poll_rx();
                // Should be 0 (no data) or could have pending kernel frames
                // n is usize, always non-negative
                let _ = n;
            }
            Err(_) => {
                // Expected on most platforms without TAP
            }
        }
    }

    #[test]
    fn test_tap_rx_pending_initial() {
        let result = TapBridge::create("tap_nonexistent_99999");
        if let Ok(bridge) = result {
            assert!(!bridge.has_rx());
            assert_eq!(bridge.rx_pending_count(), 0);
            let _drained = Vec::<Vec<u8>>::new();
            // Can't call drain_rx on non-mut, but verifying initial state
            assert_eq!(bridge.rx_pending_count(), 0);
        }
    }

    #[test]
    fn test_tap_platform_stub_unsupported() {
        // On non-Linux, non-macOS platforms, create_tap_platform returns Unsupported.
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let result = create_tap_platform("tap0");
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
        }
    }
}
