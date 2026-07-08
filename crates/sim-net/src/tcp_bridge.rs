//! Host-connected TCP bridge for interactive networking mode.
//!
//! When enabled, VirtualEthDevice frames are bridged to a remote TCP
//! endpoint using non-blocking I/O.  This allows the simulated firmware
//! to communicate with real network services without root privileges
//! or kernel modules.
//!
//! # Protocol
//!
//! Ethernet frames are framed with a 2-byte big-endian length prefix:
//!
//! ```text
//! ┌──────────────────┬─────────────────────────┐
//! │ len (2 bytes BE) │ Ethernet frame (0-65535) │
//! └──────────────────┴─────────────────────────┘
//! ```
//!
//! # Determinism warning
//!
//! Host-connected mode is **not** deterministic.  Use deterministic
//! packet scripts for golden-trace tests.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │  Guest firmware (VirtualEthDevice)           │
//! ├──────────────────────────────────────────────┤
//! │  TcpBridge                                   │
//! │  · non-blocking TCP socket                   │
//! │  · read/write framed Ethernet frames         │
//! │  · register with HostPoller for wakeup      │
//! ├──────────────────────────────────────────────┤
//! │  Host network (TCP endpoint / bridge server) │
//! └──────────────────────────────────────────────┘
//! ```

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;

/// A TCP bridge that connects a VirtualEthDevice to a remote endpoint.
pub struct TcpBridge {
    /// The TCP connection (non-blocking).
    stream: TcpStream,
    /// Pending frames to send to the remote endpoint (from VirtualEthDevice).
    tx_pending: VecDeque<Vec<u8>>,
    /// Pending frames received from the remote endpoint (for VirtualEthDevice).
    rx_pending: VecDeque<Vec<u8>>,
    /// Partial read buffer for reassembling frames.
    read_buf: Vec<u8>,
    /// Whether we're currently reading a frame length (2 bytes) or payload.
    reading_length: bool,
    /// Expected frame length (set after reading 2-byte header).
    expected_len: usize,
    /// Serialized bytes (`[len_be16][payload]`) of the frame currently being
    /// written. Non-empty while a frame is mid-flight after a partial write.
    out_buf: Vec<u8>,
    /// Offset of the next unwritten byte within `out_buf`.
    out_pos: usize,
    /// Whether the bridge is connected.
    connected: bool,
}

impl TcpBridge {
    /// Connect to a TCP bridge endpoint at `addr` (e.g. "127.0.0.1:9999").
    ///
    /// The socket is set to non-blocking mode.
    pub fn connect(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?; // low latency for Ethernet frames
        Ok(Self {
            stream,
            tx_pending: VecDeque::new(),
            rx_pending: VecDeque::new(),
            read_buf: Vec::new(),
            reading_length: true,
            expected_len: 0,
            out_buf: Vec::new(),
            out_pos: 0,
            connected: true,
        })
    }

    /// Create a bridge from an already-connected non-blocking TcpStream.
    pub fn from_stream(stream: TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            tx_pending: VecDeque::new(),
            rx_pending: VecDeque::new(),
            read_buf: Vec::new(),
            reading_length: true,
            expected_len: 0,
            out_buf: Vec::new(),
            out_pos: 0,
            connected: true,
        })
    }

    /// Whether the bridge is still connected.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Get the raw file descriptor for HostPoller registration.
    pub fn raw_fd(&self) -> i32 {
        self.stream.as_raw_fd()
    }

    /// Queue an Ethernet frame to be sent to the remote endpoint.
    ///
    /// The frame will be sent on the next `flush_tx()` call
    /// (called from the scheduler cycle).
    pub fn send_frame(&mut self, frame: &[u8]) {
        self.tx_pending.push_back(frame.to_vec());
    }

    /// Flush all pending tx frames to the TCP socket.
    ///
    /// Returns the number of frames successfully sent.
    /// Frames that couldn't be sent (due to EWOULDBLOCK) remain in
    /// the tx_pending queue for the next cycle.
    pub fn flush_tx(&mut self) -> usize {
        if !self.connected {
            return 0;
        }

        let mut sent = 0;
        while let Some(frame) = self.tx_pending.pop_front() {
            // Write frame with 2-byte big-endian length prefix.
            let len = frame.len() as u16;
            let header = len.to_be_bytes();
            match self.stream.write(&header) {
                Ok(2) => {}
                Ok(n) if n < 2 => {
                    // Partial write — put back and try next cycle.
                    self.tx_pending.push_front(frame);
                    break;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.tx_pending.push_front(frame);
                    break;
                }
                Err(_) => {
                    self.connected = false;
                    self.tx_pending.push_front(frame);
                    break;
                }
                _ => {
                    self.tx_pending.push_front(frame);
                    break;
                }
            }
            match self.stream.write(&frame) {
                Ok(n) if n == frame.len() => {
                    sent += 1;
                }
                Ok(_) => {
                    // Partial write — put back.
                    self.tx_pending.push_front(frame);
                    break;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.tx_pending.push_front(frame);
                    break;
                }
                Err(_) => {
                    self.connected = false;
                    self.tx_pending.push_front(frame);
                    break;
                }
            }
        }
        sent
    }

    /// Read available data from the TCP socket and reassemble frames.
    ///
    /// Reassembled frames are placed in `rx_pending`.  Returns the
    /// number of complete frames read.
    pub fn poll_rx(&mut self) -> usize {
        if !self.connected {
            return 0;
        }

        let mut frames_read = 0;
        let mut buf = [0u8; 2048];

        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    // Connection closed by remote.
                    self.connected = false;
                    break;
                }
                Ok(n) => {
                    self.read_buf.extend_from_slice(&buf[..n]);
                    frames_read += self.reassemble_frames();
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    break;
                }
                Err(_) => {
                    self.connected = false;
                    break;
                }
            }
        }

        frames_read
    }

    /// Reassemble complete frames from the read buffer.
    ///
    /// Each frame is prefixed with a 2-byte big-endian length.
    fn reassemble_frames(&mut self) -> usize {
        let mut count = 0;

        while self.connected {
            if self.reading_length {
                if self.read_buf.len() < 2 {
                    break;
                }
                let len_bytes: [u8; 2] = [self.read_buf[0], self.read_buf[1]];
                self.expected_len = u16::from_be_bytes(len_bytes) as usize;
                self.read_buf.drain(..2);
                self.reading_length = false;
            }

            if !self.reading_length {
                if self.read_buf.len() < self.expected_len {
                    break;
                }
                let frame: Vec<u8> = self.read_buf.drain(..self.expected_len).collect();
                self.rx_pending.push_back(frame);
                self.reading_length = true;
                count += 1;
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

    /// Check if any tx frames are pending.
    pub fn has_tx(&self) -> bool {
        !self.tx_pending.is_empty()
    }

    /// Number of pending tx frames.
    pub fn tx_pending_count(&self) -> usize {
        self.tx_pending.len()
    }

    /// Number of pending rx frames.
    pub fn rx_pending_count(&self) -> usize {
        self.rx_pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn echo_server() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        (listener, addr)
    }

    #[test]
    fn test_bridge_connect_and_send() {
        let (listener, addr) = echo_server();

        // Spawn an echo server that reads framed data and echoes back.
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_nonblocking(false).unwrap();
            // Read frame: 2-byte length + payload
            let mut len_buf = [0u8; 2];
            stream.read_exact(&mut len_buf).unwrap();
            let len = u16::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            stream.read_exact(&mut payload).unwrap();
            // Echo back the same frame
            stream.write_all(&len_buf).unwrap();
            stream.write_all(&payload).unwrap();
        });

        let mut bridge = TcpBridge::connect(&addr).unwrap();
        assert!(bridge.is_connected());

        // Send a frame
        let frame = b"hello ethernet bridge";
        bridge.send_frame(frame);
        assert_eq!(bridge.flush_tx(), 1);

        // Wait for echo
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Poll for response
        let frames = bridge.poll_rx();
        assert!(frames > 0 || bridge.has_rx());
        let rx = bridge.drain_rx();
        assert_eq!(rx.len(), 1);
        assert_eq!(&rx[0], frame);

        handle.join().unwrap();
    }

    #[test]
    fn test_bridge_multiple_frames() {
        let (listener, addr) = echo_server();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_nonblocking(false).unwrap();
            // Read two frames
            for _ in 0..2 {
                let mut len_buf = [0u8; 2];
                if stream.read_exact(&mut len_buf).is_err() {
                    break;
                }
                let len = u16::from_be_bytes(len_buf) as usize;
                let mut payload = vec![0u8; len];
                if stream.read_exact(&mut payload).is_err() {
                    break;
                }
                // Echo back
                let _ = stream.write_all(&len_buf);
                let _ = stream.write_all(&payload);
            }
        });

        let mut bridge = TcpBridge::connect(&addr).unwrap();

        bridge.send_frame(b"frame1");
        bridge.send_frame(b"frame2");
        assert_eq!(bridge.flush_tx(), 2);

        std::thread::sleep(std::time::Duration::from_millis(50));
        bridge.poll_rx();

        let rx = bridge.drain_rx();
        assert_eq!(rx.len(), 2);
        assert_eq!(rx[0], b"frame1");
        assert_eq!(rx[1], b"frame2");

        handle.join().unwrap();
    }

    #[test]
    fn test_bridge_disconnect_detection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Accept and immediately close
            stream.shutdown(std::net::Shutdown::Both).ok();
        });

        let mut bridge = TcpBridge::connect(&addr).unwrap();
        assert!(bridge.is_connected());

        // Give the server time to close the connection
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Poll should detect the closed connection
        bridge.poll_rx();
        assert!(!bridge.is_connected());

        handle.join().unwrap();
    }

    #[test]
    fn test_bridge_partial_read_reassembly() {
        let (listener, addr) = echo_server();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_nonblocking(false).unwrap();
            stream.set_nodelay(true).unwrap();
            // Send a frame followed by another frame in one write
            let frame1 = b"hello";
            let frame2 = b"world!!!";
            // Frame 1: len=5, "hello"
            stream.write_all(&5u16.to_be_bytes()).unwrap();
            stream.write_all(frame1).unwrap();
            // Frame 2: len=8, "world!!!"
            stream.write_all(&8u16.to_be_bytes()).unwrap();
            stream.write_all(frame2).unwrap();
            // Wait for client to acknowledge by sending something
            let mut buf = [0u8; 1];
            let _ = stream.read(&mut buf); // ignore WouldBlock/error
        });

        let mut bridge = TcpBridge::connect(&addr).unwrap();

        // Send a dummy frame to unblock the server
        bridge.send_frame(b"x");
        bridge.flush_tx();

        // Wait for server to echo the data back.
        std::thread::sleep(std::time::Duration::from_millis(200));
        bridge.poll_rx();

        let rx = bridge.drain_rx();
        assert_eq!(rx.len(), 2);
        assert_eq!(rx[0], b"hello");
        assert_eq!(rx[1], b"world!!!");

        handle.join().unwrap();
    }
}
