//! Cockpit dogfood integration test for the sim-grpc GUI-facing gRPC
//! control plane.
//!
//! Verifies the display-frame pipeline end-to-end (UNBLOCKING.md section 5,
//! Stage G):
//!
//!   CreateSession -> LoadScenario -> ConfigureBoard (display/touch) -> Run
//!   stream (stream_display) -> framebuffer-hash determinism across
//!   sequential runs -> concurrent two-session device-0 isolation.
//!
//! A dashboard firmware renders the 320x240 RGB565 framebuffer for one
//! vehicle mode per run. The test asserts known FNV-1a 64-bit hashes
//! computed from the renderer output against the gRPC DisplayFrame stream
//! for all seven modes (boot, READY, DRIVE, LIMP, FAULT, CHARGING,
//! OTA_UPDATE).

use std::sync::Arc;

use sim_core::Tick;
use sim_grpc::proto::simulator_client::SimulatorClient;
use sim_grpc::proto::simulator_server::SimulatorServer;
use sim_grpc::proto::*;
use sim_grpc::server::{FirmwareRegistry, SimulatorServiceImpl};
use sim_world::firmware::Firmware;
use sim_world::Machine;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

// ── Board layout constants ──────────────────────────────────────────────────

const DISPLAY_ID: u32 = 0;
const DISPLAY_WIDTH: u32 = 320;
const DISPLAY_HEIGHT: u32 = 240;
const DISPLAY_COLOR_MODE: &str = "rgb565";
const TOUCH_ID: u32 = 0;
const DASHBOARD_MACHINE_ID: u64 = 4;

// ── RGB565 colors (matching microcar_dashboard.h) ───────────────────────────

const BLACK: u32 = 0x0000;
const WHITE: u32 = 0xFFFF;
const GREEN: u32 = 0x07E0;
const RED: u32 = 0xF800;
const AMBER: u32 = 0xFD20;
const BG_READY: u32 = 0x0010;
const BG_DRIVE: u32 = 0x0200;
const BG_LIMP: u32 = 0xFD20;
const BG_FAULT: u32 = 0x7800;
const BG_CHARGING: u32 = 0x4010;
const BG_OTA: u32 = 0x0008;

// ── Dashboard display dimensions ────────────────────────────────────────────

const W: u16 = 320;
const H: u16 = 240;

// ── Seven-segment digit constants ───────────────────────────────────────────

const DIGIT_W: u16 = 30;
const DIGIT_H: u16 = 46;

// Segment bitmask: bit 6=A 5=B 4=C 3=D 2=E 1=F 0=G
const DIGIT_SEGS: [u8; 10] = [
    0x7E, // 0: A B C D E F
    0x30, // 1: B C
    0x6D, // 2: A B G E D
    0x79, // 3: A B G C D
    0x33, // 4: F G B C
    0x5B, // 5: A F G C D
    0x5F, // 6: A F G E C D
    0x70, // 7: A B C
    0x7F, // 8: A B C D E F G
    0x7B, // 9: A F G B C D
];

// ── Expected FNV-1a 64-bit hashes (BE byte order, full 320×240×2 bytes) ────
// Computed from the dashboard renderer algorithm at 2026-07-13.

const HASH_BOOT: u64 = 0x1244abd00e79d825;
const HASH_READY: u64 = 0x8ca52cc1ed05e9a1;
const HASH_DRIVE: u64 = 0x8ffeb84fcef06245;
const HASH_LIMP: u64 = 0x9dba86bb28a510d5;
const HASH_FAULT: u64 = 0xb7623ea3d239c855;
const HASH_CHARGING: u64 = 0x2f2c5a1d4938ea35;
const HASH_OTA: u64 = 0x0728c1e192f3a18d;

/// Mode names and their expected hashes.
const MODES: &[(&str, u64)] = &[
    ("boot", HASH_BOOT),
    ("READY", HASH_READY),
    ("DRIVE", HASH_DRIVE),
    ("LIMP", HASH_LIMP),
    ("FAULT", HASH_FAULT),
    ("CHARGING", HASH_CHARGING),
    ("OTA_UPDATE", HASH_OTA),
];

// ── FNV-1a 64-bit ───────────────────────────────────────────────────────────

fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

// ── Dashboard renderer (Rust reimplementation of microcar_dashboard.c) ──────

fn render_mode(mode: &str) {
    sim_devices::with_display_mut(DISPLAY_ID, |d| match mode {
        "boot" => {
            d.fill_rect(0, 0, W, H, BLACK);
            d.fill_rect(40, 108, 240, 24, WHITE);
        }
        "READY" => {
            d.fill_rect(0, 0, W, H, BG_READY);
            draw_border(d, 0, 0, 320, 40, WHITE);
            d.fill_rect(1, 1, 318, 38, GREEN);
            draw_number(d, 20, 70, 120, 80, 0, WHITE, BG_READY);
        }
        "DRIVE" => {
            d.fill_rect(0, 0, W, H, BG_DRIVE);
            draw_border(d, 0, 0, 320, 40, WHITE);
            d.fill_rect(1, 1, 318, 38, GREEN);
            draw_number(d, 20, 70, 120, 80, 0, WHITE, BG_DRIVE);
            d.fill_rect(180, 70, 120, 80, BG_DRIVE);
            draw_number(d, 180, 70, 120, 80, 0, WHITE, BG_DRIVE);
        }
        "LIMP" => {
            d.fill_rect(0, 0, W, H, BG_LIMP);
            draw_border(d, 0, 0, 320, 40, WHITE);
            d.fill_rect(1, 1, 318, 38, AMBER);
            draw_number(d, 20, 70, 120, 80, 0, WHITE, BG_LIMP);
            d.fill_rect(180, 70, 120, 80, BG_LIMP);
            draw_number(d, 180, 70, 120, 80, 0, WHITE, BG_LIMP);
        }
        "FAULT" => {
            d.fill_rect(0, 0, W, H, BG_FAULT);
            draw_border(d, 0, 170, 320, 70, WHITE);
            d.fill_rect(1, 171, 318, 68, RED);
        }
        "CHARGING" => {
            d.fill_rect(0, 0, W, H, BG_CHARGING);
            draw_bar(d, 20, 90, 280, 24, 0, 100, BG_CHARGING);
            draw_bar(d, 20, 140, 280, 24, 0, 100, BG_CHARGING);
        }
        "OTA_UPDATE" => {
            d.fill_rect(0, 0, W, H, BG_OTA);
            draw_bar(d, 20, 110, 280, 24, 0, 100, BG_OTA);
        }
        _ => {}
    });
}

fn draw_border(d: &mut sim_devices::VirtualDisplay, x: u16, y: u16, w: u16, h: u16, color: u32) {
    if w <= 1 || h <= 1 {
        return;
    }
    for col in 0..w {
        d.set_pixel(x + col, y, color);
        d.set_pixel(x + col, y + h - 1, color);
    }
    for row in 1..(h - 1) {
        d.set_pixel(x, y + row, color);
        d.set_pixel(x + w - 1, y + row, color);
    }
}

fn draw_bar(
    d: &mut sim_devices::VirtualDisplay,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    value: u8,
    max_val: u8,
    bg: u32,
) {
    draw_border(d, x, y, w, h, WHITE);
    if max_val == 0 || w <= 2 || h <= 2 {
        return;
    }
    let inner_w = w - 2;
    let inner_h = h - 2;
    let fill_w = ((inner_w as u32 * value as u32) / max_val as u32) as u16;
    let fill_w = fill_w.min(inner_w);
    d.fill_rect(x + 1, y + 1, inner_w, inner_h, bg);
    d.fill_rect(x + 1, y + 1, fill_w, inner_h, GREEN);
}

fn draw_digit(d: &mut sim_devices::VirtualDisplay, x: u16, y: u16, digit: u8, color: u32) {
    if digit > 9 {
        return;
    }
    let mask = DIGIT_SEGS[digit as usize];
    if mask & 0x40 != 0 {
        d.fill_rect(x + 1, y, 28, 8, color);
    } // A
    if mask & 0x20 != 0 {
        d.fill_rect(x + 22, y + 1, 8, 18, color);
    } // B
    if mask & 0x10 != 0 {
        d.fill_rect(x + 22, y + 20, 8, 18, color);
    } // C
    if mask & 0x08 != 0 {
        d.fill_rect(x + 1, y + 38, 28, 8, color);
    } // D
    if mask & 0x04 != 0 {
        d.fill_rect(x, y + 20, 8, 18, color);
    } // E
    if mask & 0x02 != 0 {
        d.fill_rect(x, y + 1, 8, 18, color);
    } // F
    if mask & 0x01 != 0 {
        d.fill_rect(x + 1, y + 19, 28, 8, color);
    } // G
}

fn draw_number(
    d: &mut sim_devices::VirtualDisplay,
    rx: u16,
    ry: u16,
    rw: u16,
    rh: u16,
    value: i32,
    color: u32,
    bg: u32,
) {
    let mut digits: [u8; 7] = [0; 7];
    let mut nd = 0usize;
    if value == 0 {
        digits[0] = 0;
        nd = 1;
    } else {
        let mut v = value;
        while v > 0 && nd < 7 {
            digits[nd] = (v % 10) as u8;
            nd += 1;
            v /= 10;
        }
        digits[..nd].reverse();
    }

    let total_w = nd as u16 * DIGIT_W;
    let start_x = rx + rw - total_w;
    let start_y = ry + (rh - DIGIT_H) / 2;

    d.fill_rect(rx, ry, rw, rh, bg);

    for i in 0..nd {
        draw_digit(d, start_x + i as u16 * DIGIT_W, start_y, digits[i], color);
    }
}

// ── Single-mode dashboard firmware ──────────────────────────────────────────

/// Renders one dashboard mode and stops.
struct SingleModeFirmware {
    mode_name: &'static str,
    done: bool,
}

impl SingleModeFirmware {
    fn new(mode_name: &'static str) -> Self {
        Self {
            mode_name,
            done: false,
        }
    }
}

impl Firmware for SingleModeFirmware {
    fn init(&mut self, machine: &mut Machine) {
        // Schedule a tick event so the simulation has something to advance toward.
        machine.schedule_at(0, 0, "tick", Box::new(|_| {}));
    }

    fn step(&mut self, _now: Tick, machine: &mut Machine) {
        if self.done {
            return;
        }
        self.done = true;
        let mode = self.mode_name;
        let _ = machine.with_device_context(|| {
            render_mode(mode);
        });
    }
}

// ── Server helpers ──────────────────────────────────────────────────────────

/// Start the gRPC server with a registry containing one firmware for the given
/// mode.
async fn start_server_for_mode(
    mode: &'static str,
    firmware_path: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));

    let mut registry = FirmwareRegistry::new();
    let m = mode;
    registry.register(
        firmware_path,
        Arc::new(move || Box::new(SingleModeFirmware::new(m))),
    );

    let service = SimulatorServiceImpl::new().with_firmware_registry(registry);

    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(SimulatorServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("server");
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (addr, handle)
}

// ── Collected frame data ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CollectedFrame {
    machine_id: u64,
    device_id: u32,
    width: u32,
    height: u32,
    full_frame: bool,
    hash: u64,
    dirty_rects: Vec<(u32, u32, u32, u32)>,
}

// ── Helper: run the cockpit flow for one mode and collect display frames ────

async fn collect_frames_for_mode(
    addr: &str,
    machine_id: u64,
    firmware_path: &str,
) -> Vec<CollectedFrame> {
    let mut client = SimulatorClient::connect(addr.to_string())
        .await
        .expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    assert!(sess.session_id > 0);

    let scenario_toml = format!(
        r#"name = "cockpit"
[[machine]]
id = {machine_id}
name = "dashboard"
firmware = "{firmware_path}"
"#,
    );

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml,
        })
        .await
        .expect("load");

    let cfg = client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
            machine_id: Some(machine_id),
            peripherals: vec![
                PeripheralDef {
                    device: "display".into(),
                    id: DISPLAY_ID,
                    display_width: DISPLAY_WIDTH,
                    display_height: DISPLAY_HEIGHT,
                    color_mode: DISPLAY_COLOR_MODE.into(),
                    ..Default::default()
                },
                PeripheralDef {
                    device: "touch".into(),
                    id: TOUCH_ID,
                    touch_display_id: DISPLAY_ID,
                    ..Default::default()
                },
            ],
        })
        .await
        .expect("configure")
        .into_inner();
    assert_eq!(cfg.n_peripherals, 2, "expected 2 configured peripherals");

    let messages = vec![RunRequest {
        payload: Some(run_request::Payload::Config(RunConfig {
            session_id: sess.session_id,
            tick_batch_size: 1,
            stream_display: true,
            stream_trace: false,
        })),
    }];

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(messages)))
        .await
        .expect("run")
        .into_inner();

    let mut frames: Vec<CollectedFrame> = Vec::new();

    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::Display(frame)) => {
                let mut fb_bytes: Vec<u8> = Vec::new();
                let mut rects: Vec<(u32, u32, u32, u32)> = Vec::new();
                for rect in &frame.dirty_rects {
                    fb_bytes.extend_from_slice(&rect.data);
                    rects.push((rect.x, rect.y, rect.w, rect.h));
                }
                frames.push(CollectedFrame {
                    machine_id: frame.machine_id,
                    device_id: frame.device_id,
                    width: frame.width,
                    height: frame.height,
                    full_frame: frame.full_frame,
                    hash: fnv1a_64(&fb_bytes),
                    dirty_rects: rects,
                });
            }
            Some(run_event::Payload::End(_)) => break,
            _ => {}
        }
    }

    frames
}

// ── Tests ───────────────────────────────────────────────────────────────────

/// For each mode: run two sequential sessions, verify nonempty frames,
/// known hash, dirty-rect determinism.
#[tokio::test]
async fn cockpit_display_frames_and_determinism() {
    for &(mode_name, expected_hash) in MODES {
        let fw_path = "dashboard";
        let (addr, _handle) = start_server_for_mode(mode_name, fw_path).await;

        let first = collect_frames_for_mode(&addr, DASHBOARD_MACHINE_ID, fw_path).await;
        let second = collect_frames_for_mode(&addr, DASHBOARD_MACHINE_ID, fw_path).await;

        // ── Assert nonempty ──────────────────────────────────────────
        assert!(
            !first.is_empty(),
            "mode {mode_name}: first run should have display frames"
        );
        assert!(
            !second.is_empty(),
            "mode {mode_name}: second run should have display frames"
        );
        assert_eq!(
            first.len(),
            second.len(),
            "mode {mode_name}: both runs same frame count"
        );

        // ── Assert per-frame invariants ──────────────────────────────
        for (i, f) in first.iter().enumerate() {
            assert_eq!(
                f.machine_id, DASHBOARD_MACHINE_ID,
                "mode {mode_name} frame {i}: machine_id"
            );
            assert_eq!(
                f.device_id, DISPLAY_ID,
                "mode {mode_name} frame {i}: device_id"
            );
            assert_eq!(f.width, DISPLAY_WIDTH);
            assert_eq!(f.height, DISPLAY_HEIGHT);
        }

        // ── Assert known hash ────────────────────────────────────────
        assert_eq!(
            first[0].hash, expected_hash,
            "mode {mode_name}: hash mismatch"
        );

        // ── Assert dirty rects identical across repeats ──────────────
        for (i, (a, b)) in first.iter().zip(second.iter()).enumerate() {
            assert_eq!(
                a.hash, b.hash,
                "mode {mode_name} frame {i}: hash must match across runs"
            );
            assert_eq!(
                a.dirty_rects, b.dirty_rects,
                "mode {mode_name} frame {i}: dirty rects must match"
            );
            assert_eq!(
                a.full_frame, b.full_frame,
                "mode {mode_name} frame {i}: full_frame flag must match"
            );
        }
    }
}

/// Two concurrent sessions must isolate device 0: session A (machine 4,
/// boot mode) and session B (machine 5, READY mode) produce distinct
/// non-overlapping frames.
#[tokio::test]
async fn cockpit_concurrent_sessions_isolate_device_zero() {
    // Use two servers to avoid registry conflicts with different firmware paths.
    let (addr_a, _ha) = start_server_for_mode("boot", "dash_a").await;
    let (addr_b, _hb) = start_server_for_mode("READY", "dash_b").await;

    let (frames_a, frames_b) = tokio::join!(
        collect_frames_for_mode(&addr_a, 4, "dash_a"),
        collect_frames_for_mode(&addr_b, 5, "dash_b"),
    );

    // Session A: boot screen
    assert_eq!(frames_a.len(), 1, "session A: one boot frame");
    assert_eq!(frames_a[0].hash, HASH_BOOT, "session A: boot hash");
    assert_eq!(frames_a[0].machine_id, 4);

    // Session B: READY screen
    assert_eq!(frames_b.len(), 1, "session B: one READY frame");
    assert_eq!(frames_b[0].hash, HASH_READY, "session B: READY hash");
    assert_eq!(frames_b[0].machine_id, 5);

    // Hashes must differ (sessions don't cross-observe).
    assert_ne!(
        frames_a[0].hash, frames_b[0].hash,
        "concurrent sessions must not cross-observe pixels"
    );
}
