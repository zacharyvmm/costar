# costar GUI Backend — Implementation Handoff (gRPC)

Date: 2026-06-27
Audience: Rust engineers building the gRPC server that drives an Electron GUI.
Prerequisites: `docs/GUI_FEASIBILITY_REPORT.md` (read first).

---

## 0. Architecture Decision: gRPC, Not JSON-RPC

### Why gRPC

| Concern | JSON-RPC/NDJSON | gRPC/Protobuf |
|---------|----------------|---------------|
| Framebuffer transport | Base64-encoded, 33% overhead. 320×240 RGB565 = 150KB → 200KB per full frame | Raw `bytes` field, zero encoding tax. 150KB on the wire |
| Streaming model | NDJSON lines in one direction at a time. No true bidirectional. | Native bidirectional streams (HTTP/2). Touch events in, frames out over one socket |
| Contract | Ad-hoc JSON shapes, documented in comments | `.proto` file is the schema — compiles to typed clients in Rust, JS, Go, Python |
| Debuggability | `echo '...' \| nc` — very easy | Needs grpcurl or a client. Harder but worth it |
| Dependency weight | `serde_json` only (already in tree) | `tonic` + `prost` + `tokio` (~20 new crates) |

**Decision**: gRPC. Framebuffer transfer is the dominant data path — base64 waste is unacceptable. The bidirectional stream maps perfectly to the "play simulation while injecting touch events" interaction model. The `.proto` file serves as the API contract between the Rust backend team and the Electron frontend team.

### Repo Boundary

```
┌──────────────────────────────────────────┐
│  costar repo (THIS REPO)                  │
│                                           │
│  crates/sim-core        (unchanged)       │
│  crates/sim-fiber       (unchanged)       │
│  crates/sim-ffi         (+ display/touch) │
│  crates/sim-devices     (+ display/touch  │
│                          + inspect)       │
│  crates/sim-net         (unchanged)       │
│  crates/sim-freertos-port (unchanged)     │
│  crates/sim-zephyr-port  (unchanged)      │
│  crates/sim-runner      (unchanged CLI)   │
│  crates/sim-world       (+ keyframes)     │
│  crates/sim-grpc        (NEW — gRPC server)│
│                                           │
│  The GUI is NOT in this repo.             │
│  sim-grpc is the deliverable.             │
└──────────────┬───────────────────────────┘
               │ HTTP/2 (gRPC)
               │ localhost:9321
┌──────────────▼───────────────────────────┐
│  Electron GUI repo (SEPARATE PROJECT)     │
│  - gRPC-web or @grpc/grpc-js client      │
│  - Topology canvas                        │
│  - Display canvases                       │
│  - Timeline + controls                    │
└──────────────────────────────────────────┘
```

---

## 1. New Crate: sim-grpc

### 1.1 Location

```
crates/sim-grpc/
├── Cargo.toml
├── build.rs
├── proto/
│   └── simulator.proto       ← THE contract between backend and GUI
├── src/
│   ├── lib.rs                ← re-exports generated proto code
│   ├── main.rs               ← binary entry point
│   ├── server.rs             ← gRPC service implementation
│   ├── session.rs            ← session management (adapted from serve.rs)
│   └── inspect.rs            ← converts DeviceSnapshot → proto messages
```

### 1.2 Cargo.toml

```toml
[package]
name = "sim-grpc"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "gRPC server for costar simulation — drives Electron GUI"

[[bin]]
name = "costar-grpc"
path = "src/main.rs"

[dependencies]
sim-core = { path = "../sim-core" }
sim-devices = { path = "../sim-devices" }
sim-world = { path = "../sim-world" }
tonic = "0.12"
prost = "0.13"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "net"] }
serde_json = "1"       # only for scenario TOML ↔ string
base64 = "0.22"        # only for display framebuffer encoding in inspect
log = "0.4"
env_logger = "0.11"

[build-dependencies]
tonic-build = "0.12"
```

### 1.3 build.rs

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false) // only server in this crate
        .compile_protos(
            &["proto/simulator.proto"],
            &["proto/"],
        )?;
    Ok(())
}
```

### 1.4 Workspace Registration

In root `Cargo.toml`, add to `members`:

```toml
members = [
    ...
    "crates/sim-grpc",
]
```

---

## 2. The Protobuf Contract

### 2.1 File: `crates/sim-grpc/proto/simulator.proto`

This file IS the API contract. The Electron team codes against the generated JS client. Any breaking change here is a breaking change for the GUI.

```protobuf
syntax = "proto3";

package costar.simulator.v1;

// ── Service ───────────────────────────────────────────────────────────────

service Simulator {
  // ── Session management (unary) ──
  rpc CreateSession(CreateSessionRequest) returns (CreateSessionResponse);
  rpc DestroySession(DestroySessionRequest) returns (DestroySessionResponse);
  rpc CloneSession(CloneSessionRequest)   returns (CloneSessionResponse);
  rpc ListSessions(ListSessionsRequest)    returns (ListSessionsResponse);

  // ── Setup (unary — must be called before Run) ──
  rpc LoadScenario(LoadScenarioRequest)   returns (LoadScenarioResponse);
  rpc ConfigureBoard(ConfigureBoardRequest) returns (ConfigureBoardResponse);

  // ── Simulation control (unary, for paused manipulation) ──
  rpc GetStatus(GetStatusRequest)           returns (GetStatusResponse);
  rpc InspectDevices(InspectDevicesRequest) returns (InspectDevicesResponse);
  rpc SaveKeyframe(SaveKeyframeRequest)     returns (SaveKeyframeResponse);
  rpc LoadKeyframe(LoadKeyframeRequest)     returns (LoadKeyframeResponse);
  rpc ListKeyframes(ListKeyframesRequest)   returns (ListKeyframesResponse);
  rpc ResetSimulation(ResetSimulationRequest) returns (ResetSimulationResponse);

  // ── MAIN: Bidirectional simulation stream ──
  //
  // This is the primary interaction. The client opens a stream,
  // the server runs the simulation, pushing trace events and display
  // frames as they occur. The client can inject touch events, pause,
  // resume, or request keyframe saves at any time.
  rpc Run(RunRequest) returns (stream RunEvent);
}

// ── Session Messages ──────────────────────────────────────────────────────

message CreateSessionRequest {}
message CreateSessionResponse {
  uint64 session_id = 1;
}

message DestroySessionRequest {
  uint64 session_id = 1;
}
message DestroySessionResponse {
  bool destroyed = 1;
}

message CloneSessionRequest {
  uint64 session_id = 1;
}
message CloneSessionResponse {
  uint64 new_session_id = 1;
}

message ListSessionsRequest {}
message SessionInfo {
  uint64 session_id = 1;
  string state = 2;        // "idle" | "ready" | "running" | "done" | "error"
  uint64 now_ticks = 3;
  uint32 n_machines = 4;
}
message ListSessionsResponse {
  repeated SessionInfo sessions = 1;
}

// ── Setup Messages ────────────────────────────────────────────────────────

message LoadScenarioRequest {
  uint64 session_id = 1;
  // Inline TOML string for the scenario definition.
  string scenario_toml = 2;
}
message LoadScenarioResponse {
  uint32 n_machines = 1;
  uint32 n_links = 2;
  uint32 n_injections = 3;
}

message PeripheralDef {
  string device = 1;       // "display", "touch", "uart", "gpio", etc.
  uint32 id = 2;
  // Display-specific
  uint32 display_width = 10;
  uint32 display_height = 11;
  string color_mode = 12;  // "rgb565" | "rgb888" | "argb8888"
  // Touch-specific
  uint32 touch_display_id = 20;
  // UART-specific
  uint32 baud_rate = 30;
  // I2C-specific
  uint32 i2c_speed_hz = 40;
  // SPI-specific
  uint32 spi_speed_hz = 50;
  // Timer-specific
  uint32 timer_irq = 60;
}

message ConfigureBoardRequest {
  uint64 session_id = 1;
  repeated PeripheralDef peripherals = 2;
}
message ConfigureBoardResponse {
  uint32 n_peripherals = 1;
}

// ── Status / Inspection ───────────────────────────────────────────────────

message GetStatusRequest {
  uint64 session_id = 1;
}
message GetStatusResponse {
  string state = 1;        // "idle" | "ready" | "running" | "paused" | "done" | "error"
  uint64 now_ticks = 2;
  uint32 n_machines = 3;
  uint32 n_events = 4;
  string error_message = 5;
}

message InspectDevicesRequest {
  uint64 session_id = 1;
  // Optional filter: only return devices of this type (e.g. "display").
  string device_type = 2;
  // Optional filter: only return this device ID.
  uint32 device_id = 3;
}
message GpioPin {
  uint32 num = 1;
  string mode = 2;    // "input" | "output" | "alternate"
  bool state = 3;
  uint32 value = 4;
}
message AdcChannel {
  uint32 channel = 1;
  uint32 value = 2;
  uint32 resolution = 3;
}
message DirtyRect {
  uint32 x = 1;
  uint32 y = 2;
  uint32 w = 3;
  uint32 h = 4;
  // Raw pixel data for this region, in the display's native format.
  // Size = w * h * bytes_per_pixel.
  // bytes_per_pixel: rgb565=2, rgb888=3, argb8888=4.
  bytes data = 5;
}
message DeviceSnapshot {
  string type = 1;     // "uart" | "gpio" | "i2c" | "spi" | "can" | "timer"
                       // | "adc" | "temp_sensor" | "eeprom" | "flash"
                       // | "display" | "touch"
  uint32 id = 2;
  // UART
  uint32 tx_buffer_len = 10;
  uint32 rx_buffer_len = 11;
  bool uart_enabled = 12;
  // GPIO
  repeated GpioPin pins = 20;
  // I2C
  uint32 i2c_tx_len = 30;
  uint32 i2c_rx_len = 31;
  uint32 i2c_address = 32;
  bool i2c_nack = 33;
  // SPI
  uint32 spi_tx_len = 40;
  uint32 spi_rx_len = 41;
  // CAN
  uint32 can_tx_queue_len = 50;
  uint32 can_rx_queue_len = 51;
  string can_error_state = 52;
  bool can_loopback = 53;
  // Timer
  bool timer_armed = 60;
  uint64 timer_remaining_ticks = 61;
  uint64 timer_period = 62;
  uint32 timer_irq = 63;
  // ADC
  repeated AdcChannel adc_channels = 70;
  // TempSensor
  int32 temp_milli_c = 80;
  // EEPROM / Flash
  uint64 storage_size_bytes = 90;
  uint64 storage_sector_size = 91;
  // Display
  uint32 display_width = 100;
  uint32 display_height = 101;
  string display_color_mode = 102;
  bool display_enabled = 103;
  uint32 display_backlight = 104;
  repeated DirtyRect display_dirty_rects = 105;
  // If true, dirty_rects contains the full frame (too many dirty regions).
  // The GUI should replace the entire canvas.
  bool display_full_frame = 106;
  // Touch
  uint32 touch_display_id = 110;
  uint32 touch_pending_events = 111;
}
message InspectDevicesResponse {
  repeated DeviceSnapshot devices = 1;
}

// ── Keyframe Messages ─────────────────────────────────────────────────────

message SaveKeyframeRequest {
  uint64 session_id = 1;
}
message SaveKeyframeResponse {
  uint64 keyframe_id = 1;
  uint64 now_ticks = 2;
  uint64 byte_size = 3;
}

message LoadKeyframeRequest {
  uint64 session_id = 1;
  uint64 keyframe_id = 2;
}
message LoadKeyframeResponse {
  bool restored = 1;
  uint64 now_ticks = 2;
}

message ListKeyframesRequest {
  uint64 session_id = 1;
}
message KeyframeInfo {
  uint64 keyframe_id = 1;
  uint64 now_ticks = 2;
  uint64 byte_size = 3;
}
message ListKeyframesResponse {
  repeated KeyframeInfo keyframes = 1;
}

message ResetSimulationRequest {
  uint64 session_id = 1;
}
message ResetSimulationResponse {
  bool reset = 1;
}

// ── Run Stream Messages ───────────────────────────────────────────────────

// Client → Server: configuration for the run, then control commands.
// The first message MUST be a RunConfig. Subsequent messages are commands.
message RunRequest {
  oneof payload {
    RunConfig config = 1;
    TouchInject touch = 2;
    PauseCommand pause = 3;
    ResumeCommand resume = 4;
    SaveKeyframeCommand save_keyframe = 5;
    InjectEventCommand inject_event = 6;
    StopCommand stop = 7;
  }
}

message RunConfig {
  uint64 session_id = 1;
  // Advance simulation by this many ticks per "batch" before sending
  // accumulated events and frames to the client. Higher = fewer messages,
  // lower = smoother display updates. Recommended: 1000 (1ms at 1MHz tick).
  uint64 tick_batch_size = 2;
  // If true, include display frames in the stream.
  bool stream_display = 3;
  // If true, include trace events in the stream.
  bool stream_trace = 4;
}

message TouchInject {
  uint32 device_id = 1;
  repeated TouchEvent events = 2;
}
message TouchEvent {
  uint32 point_id = 1;
  uint32 x = 2;
  uint32 y = 3;
  uint32 pressure = 4;  // 0-255
  TouchEventType event_type = 5;
}
enum TouchEventType {
  TOUCH_PRESS = 0;
  TOUCH_RELEASE = 1;
  TOUCH_MOVE = 2;
}

message PauseCommand {}
message ResumeCommand {}
message SaveKeyframeCommand {}
message StopCommand {}

message InjectEventCommand {
  uint64 at_ticks = 1;
  uint32 priority = 2;
  string label = 3;
  uint32 value = 4;
}

// Server → Client: streamed events during simulation.
message RunEvent {
  oneof payload {
    TickBoundary tick = 1;
    TraceLine trace = 2;
    DisplayFrame display = 3;
    SimulationPaused paused = 4;
    SimulationEnd end = 5;
    SimulationError error = 6;
  }
}

message TickBoundary {
  uint64 ts = 1;  // current virtual time in ticks
}

message TraceLine {
  // Machine-prefixed trace line, same format as the existing human-readable
  // trace output. E.g.: "[machine.0]    100 task-resume id=1 ..."
  string line = 1;
}

message DisplayFrame {
  uint32 device_id = 1;
  uint32 width = 2;
  uint32 height = 3;
  string color_mode = 4;  // "rgb565" | "rgb888" | "argb8888" | "mono"
  repeated DirtyRect dirty_rects = 5;
  bool full_frame = 6;
}

message SimulationPaused {
  uint64 ts = 1;
}

message SimulationEnd {
  uint64 ts = 1;
  uint64 total_ticks = 2;
  uint64 total_events = 3;
}

message SimulationError {
  string message = 1;
}
```

### 2.2 Service Method Semantics

| Method | Type | When called | Behavior |
|--------|------|-------------|----------|
| `CreateSession` | Unary | App start | Allocates a session ID |
| `LoadScenario` | Unary | After create, before ConfigureBoard | Parses TOML, builds World, stores in session |
| `ConfigureBoard` | Unary | After LoadScenario | Initializes virtual peripherals (display, touch, etc.) |
| `Run` | **Bidi Stream** | User hits Play | Opens stream. Client sends TouchInject/Pause/Resume. Server sends TickBoundary/TraceLine/DisplayFrame. |
| `GetStatus` | Unary | While paused | Returns current state, time, event count |
| `InspectDevices` | Unary | While paused, on demand | Returns DeviceSnapshot list (GPIO state, display framebuffer, etc.) |
| `SaveKeyframe` | Unary | While paused | Serializes World state for later restore |
| `LoadKeyframe` | Unary | While paused, user scrubs timeline | Restores World to a previous keyframe |
| `ResetSimulation` | Unary | User wants fresh start | Rebuilds World from stored scenario |

---

## 3. Layer Architecture (Reaffirmed)

```
┌─────────────────────────────────────────────────────────┐
│  sim-grpc crate (NEW)                                   │
│  ┌───────────────────────────────────────────────────┐  │
│  │  main.rs         — tokio runtime, Tonic server    │  │
│  │  server.rs       — SimulatorService impl          │  │
│  │  session.rs      — Session map, adapted from      │  │
│  │                     sim-runner/src/serve.rs        │  │
│  │  inspect.rs      — DeviceSnapshot → proto mapping │  │
│  └──────────────┬────────────────────────────────────┘  │
│                 │ calls                                   │
│  ┌──────────────▼────────────────────────────────────┐  │
│  │  sim-world::World                                 │  │
│  │  sim-devices::inspect::DeviceSnapshot             │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│  sim-devices crate (+ new modules)                      │
│  ┌───────────────────────────────────────────────────┐  │
│  │  VirtualDisplay  (display.rs)                     │  │
│  │  VirtualTouchScreen (touch.rs)                    │  │
│  │  DeviceSnapshot  (inspect.rs)                     │  │
│  │  Thread-local maps (lib.rs)                       │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│  sim-world crate (+ keyframes)                          │
│  ┌───────────────────────────────────────────────────┐  │
│  │  World::pause / resume                            │  │
│  │  World::save_keyframe / load_keyframe             │  │
│  │  WorldKeyframe struct                             │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│  sim-ffi crate (+ display/touch exports)                │
│  ┌───────────────────────────────────────────────────┐  │
│  │  sim_display_init / set_pixel / fill_rect / ...   │  │
│  │  sim_touch_init / get_event / pending_count       │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

**Separation of concerns**:
- `sim-devices` knows NOTHING about gRPC, protobuf, or sessions
- `sim-world` knows NOTHING about gRPC — keyframes are pure Rust structs
- `sim-grpc` owns ALL protobuf knowledge — it converts proto ↔ Rust types
- `sim-runner` (the existing CLI binary) is unchanged

---

## 4. Device Models (same as before, brief summary)

### 4.1 VirtualDisplay — `crates/sim-devices/src/display.rs`

Full design in the previous handoff. Key API for the gRPC layer:

- `VirtualDisplay::new(id, width, height, color_mode)` — constructor
- `set_pixel(x, y, color)` — pixel write, marks dirty
- `fill_rect(x, y, w, h, color)` — filled region, marks dirty
- `draw_bitmap(x, y, w, h, data)` — bitmap copy, marks dirty
- `take_dirty_rects() → Vec<DisplayRect>` — consumes dirty rects (for `InspectDevices`)
- `framebuffer() → &[u8]` — raw pixel data
- `width`, `height`, `color_mode`, `enabled`, `backlight` — public fields

Dirty rects merge overlapping regions. When count exceeds `max_dirty_rects` (32), collapses to a single full-frame rect.

### 4.2 VirtualTouchScreen — `crates/sim-devices/src/touch.rs`

- `VirtualTouchScreen::new(id, display_id)` — constructor
- `get_event(out: &mut TouchEvent) → bool` — firmware reads next event
- `inject_event(event: TouchEvent)` — GUI injects touch
- `pending_count() → usize` — queue depth

### 4.3 DeviceSnapshot — `crates/sim-devices/src/inspect.rs`

```rust
pub enum DeviceSnapshot {
    Uart { id, tx_buffer_len, rx_buffer_len, ... },
    Gpio { id, pins: Vec<GpioPinSnapshot>, ... },
    Display { id, width, height, color_mode, enabled, backlight,
              framebuffer_base64, dirty_rects: Vec<DirtyRectSnapshot>, ... },
    Touch { id, display_id, pending_events, ... },
    // ... all other device types
}
impl DeviceSnapshot {
    pub fn collect_all() -> Vec<DeviceSnapshot> { ... }
}
```

This is the bridge between the device layer and the gRPC layer. The gRPC server calls `collect_all()`, then maps each `DeviceSnapshot` variant into the corresponding protobuf `DeviceSnapshot` message fields.

---

## 5. gRPC Server Implementation

### 5.1 main.rs

```rust
use tonic::transport::Server;

mod server;
mod session;
mod inspect;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let addr = "[::1]:9321".parse()?;
    let service = server::SimulatorServiceImpl::new();

    log::info!("costar gRPC server listening on {}", addr);

    Server::builder()
        .add_service(
            costar_simulator_v1::simulator_server::SimulatorServer::new(service)
        )
        .serve(addr)
        .await?;

    Ok(())
}
```

### 5.2 server.rs — Service Implementation

```rust
use tonic::{Request, Response, Status, Streaming};
use tokio::sync::mpsc;

use crate::session::SessionMap;
use crate::proto::*; // generated from simulator.proto

pub struct SimulatorServiceImpl {
    sessions: SessionMap,
}

impl SimulatorServiceImpl {
    pub fn new() -> Self {
        Self { sessions: SessionMap::new() }
    }
}

#[tonic::async_trait]
impl simulator_server::Simulator for SimulatorServiceImpl {
    // ── Unary RPCs ──────────────────────────────────────

    async fn create_session(
        &self, _req: Request<CreateSessionRequest>,
    ) -> Result<Response<CreateSessionResponse>, Status> {
        let id = self.sessions.create();
        Ok(Response::new(CreateSessionResponse { session_id: id }))
    }

    async fn load_scenario(
        &self, req: Request<LoadScenarioRequest>,
    ) -> Result<Response<LoadScenarioResponse>, Status> {
        let r = req.into_inner();
        let scenario = sim_world::Scenario::from_str(&r.scenario_toml)
            .map_err(|e| Status::invalid_argument(format!("parse error: {}", e)))?;
        let n_machines = scenario.machine.len() as u32;
        let n_links = scenario.link.len() as u32;
        let n_injections = scenario.inject.len() as u32;
        let world = scenario.build_world()
            .map_err(|e| Status::internal(format!("build error: {}", e)))?;
        self.sessions.load(r.session_id, scenario, world)
            .map_err(|e| Status::not_found(e))?;
        Ok(Response::new(LoadScenarioResponse { n_machines, n_links, n_injections }))
    }

    async fn configure_board(
        &self, req: Request<ConfigureBoardRequest>,
    ) -> Result<Response<ConfigureBoardResponse>, Status> {
        let r = req.into_inner();
        let mut count = 0u32;
        for def in &r.peripherals {
            match def.device.as_str() {
                "display" => {
                    let mode = match def.color_mode.as_str() {
                        "rgb565" => DisplayColorMode::Rgb565,
                        "rgb888" => DisplayColorMode::Rgb888,
                        "argb8888" => DisplayColorMode::Argb8888,
                        _ => return Err(Status::invalid_argument("unknown color_mode")),
                    };
                    sim_devices::display_insert(
                        VirtualDisplay::new(def.id, def.display_width as u16,
                                            def.display_height as u16, mode)
                    );
                    count += 1;
                }
                "touch" => {
                    sim_devices::touch_insert(
                        VirtualTouchScreen::new(def.id, def.touch_display_id)
                    );
                    count += 1;
                }
                // ... uart, gpio, i2c, spi, can, timer, adc, etc.
                _ => {}
            }
        }
        Ok(Response::new(ConfigureBoardResponse { n_peripherals: count }))
    }

    async fn get_status(
        &self, req: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        let r = req.into_inner();
        let state = self.sessions.status(r.session_id)
            .map_err(|e| Status::not_found(e))?;
        Ok(Response::new(GetStatusResponse {
            state: state.to_string(),
            now_ticks: state.now_ticks,
            n_machines: state.n_machines,
            n_events: state.n_events,
            error_message: state.error.unwrap_or_default(),
        }))
    }

    async fn inspect_devices(
        &self, req: Request<InspectDevicesRequest>,
    ) -> Result<Response<InspectDevicesResponse>, Status> {
        let r = req.into_inner();
        let snapshots = sim_devices::inspect::DeviceSnapshot::collect_all();
        let devices = snapshots.into_iter()
            .filter(|s| {
                let type_ok = r.device_type.is_empty()
                    || s.type_str() == r.device_type;
                let id_ok = r.device_id == 0
                    || s.device_id() == r.device_id;
                type_ok && id_ok
            })
            .map(|s| crate::inspect::to_proto(s))
            .collect();
        Ok(Response::new(InspectDevicesResponse { devices }))
    }

    // ── Bidirectional Stream: Run ───────────────────────

    type RunStream = ...; // see below

    async fn run(
        &self, req: Request<Streaming<RunRequest>>,
    ) -> Result<Response<Self::RunStream>, Status> {
        // Implementation: see §5.3
    }
}
```

### 5.3 Run — Bidirectional Stream Implementation

This is the core of the gRPC server. The pattern:

```rust
type RunStream = ReceiverStream<Result<RunEvent, Status>>;

async fn run(
    &self, req: Request<Streaming<RunRequest>>,
) -> Result<Response<Self::RunStream>, Status> {
    let mut client_stream = req.into_inner();

    // Read the first message — MUST be RunConfig.
    let config = match client_stream.message().await? {
        Some(msg) if msg.payload == Some(run_request::Payload::Config(c)) => c,
        _ => return Err(Status::invalid_argument("first message must be RunConfig")),
    };

    // Get the session's World.
    let session_id = config.session_id;
    let world = self.sessions.take_world(session_id)
        .map_err(|e| Status::not_found(e))?;

    let tick_batch = config.tick_batch_size.max(1);
    let stream_display = config.stream_display;
    let stream_trace = config.stream_trace;

    // Channel for server → client events.
    let (tx, rx) = mpsc::channel(256);

    // Spawn the simulation loop on a dedicated OS thread because
    // World::run_until is synchronous and touches thread-local state.
    let tx_clone = tx.clone();
    std::thread::spawn(move || {
        // Process client commands (touch, pause, resume) interleaved
        // with simulation steps.
        // ... see below
    });

    // Spawn a task to forward client commands into the simulation thread.
    let tx_cmd = /* channel for commands */;
    tokio::spawn(async move {
        while let Ok(Some(msg)) = client_stream.message().await {
            // Forward touch/pause/resume commands to the sim thread.
        }
    });

    Ok(Response::new(ReceiverStream::new(rx)))
}
```

**The simulation thread loop** (runs on a dedicated OS thread because it needs access to thread-local device maps):

```rust
std::thread::spawn(move || {
    // Re-insert world into TLS or hold it on this thread.
    let mut world = world;

    loop {
        // Check for pending commands from client.
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                ClientCommand::Touch { device_id, events } => {
                    for ev in events {
                        sim_devices::with_touch_mut(device_id, |t| {
                            t.inject_event(ev);
                        });
                    }
                }
                ClientCommand::Pause => {
                    world.pause();
                }
                ClientCommand::Resume => {
                    world.resume();
                }
                ClientCommand::Stop => {
                    let _ = tx.send(Ok(RunEvent {
                        payload: Some(run_event::Payload::End(SimulationEnd {
                            ts: world.now,
                            total_ticks: world.now,
                            total_events: 0,
                        })),
                    }));
                    return;
                }
            }
        }

        if world.is_paused() {
            // Send paused event, then sleep briefly to avoid busy-looping.
            let _ = tx.send(Ok(RunEvent {
                payload: Some(run_event::Payload::Paused(SimulationPaused {
                    ts: world.now,
                })),
            }));
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        let deadline = world.now + tick_batch;
        let had_events = world.next_global_event_time().is_some();

        if !had_events || world.is_paused() {
            // No more events — simulation complete.
            if world.all_idle() && world.plant.is_none() {
                let _ = tx.send(Ok(RunEvent {
                    payload: Some(run_event::Payload::End(SimulationEnd {
                        ts: world.now,
                        total_ticks: world.now,
                        total_events: 0,
                    })),
                }));
                return;
            }
            continue;
        }

        // Advance simulation.
        if let Err(e) = world.run_until(deadline) {
            let _ = tx.send(Ok(RunEvent {
                payload: Some(run_event::Payload::Error(SimulationError {
                    message: e.to_string(),
                })),
            }));
            return;
        }

        // Send tick boundary.
        let _ = tx.send(Ok(RunEvent {
            payload: Some(run_event::Payload::Tick(TickBoundary {
                ts: world.now,
            })),
        }));

        // Send trace events.
        if stream_trace {
            let traces = world.drain_new_traces();
            for line in traces {
                let _ = tx.send(Ok(RunEvent {
                    payload: Some(run_event::Payload::Trace(TraceLine { line })),
                }));
            }
        }

        // Send display frames.
        if stream_display {
            for id in sim_devices::display_ids() {
                if let Some(frame) = sim_devices::with_display_mut(id, |d| {
                    let dirty = d.take_dirty_rects();
                    if dirty.is_empty() { return None; }
                    let full = dirty.len() == 1
                        && dirty[0].w == d.width
                        && dirty[0].h == d.height;
                    let rects: Vec<DirtyRect> = dirty.iter().map(|r| {
                        // Extract pixel data for this region
                        let bpp = d.color_mode.bytes_per_pixel();
                        let row_stride = d.width as usize * bpp;
                        let mut data = Vec::new();
                        for py in r.y..r.y + r.h {
                            let start = py as usize * row_stride + r.x as usize * bpp;
                            let end = start + r.w as usize * bpp;
                            if end <= d.framebuffer().len() {
                                data.extend_from_slice(&d.framebuffer()[start..end]);
                            }
                        }
                        DirtyRect {
                            x: r.x as u32, y: r.y as u32,
                            w: r.w as u32, h: r.h as u32,
                            data,
                        }
                    }).collect();
                    Some(RunEvent {
                        payload: Some(run_event::Payload::Display(DisplayFrame {
                            device_id: id,
                            width: d.width as u32,
                            height: d.height as u32,
                            color_mode: format!("{:?}", d.color_mode).to_lowercase(),
                            dirty_rects: rects,
                            full_frame: full,
                        })),
                    })
                }) {
                    let _ = tx.send(Ok(frame));
                }
            }
        }
    }
});
```

### 5.4 inspect.rs — Proto Conversion

```rust
use crate::proto::*;

pub fn to_proto(snapshot: sim_devices::inspect::DeviceSnapshot) -> DeviceSnapshot {
    match snapshot {
        sim_devices::inspect::DeviceSnapshot::Display {
            id, width, height, color_mode, enabled, backlight,
            framebuffer_base64: _,  // ignore — raw bytes used in Run stream
            dirty_rects,
        } => {
            let rects: Vec<DirtyRect> = dirty_rects.iter().map(|r| DirtyRect {
                x: r.x as u32, y: r.y as u32,
                w: r.w as u32, h: r.h as u32,
                data: base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &r.data_base64,
                ).unwrap_or_default(),
            }).collect();
            DeviceSnapshot {
                r#type: "display".into(),
                id,
                display_width: width as u32,
                display_height: height as u32,
                display_color_mode: color_mode,
                display_enabled: enabled,
                display_backlight: backlight as u32,
                display_dirty_rects: rects,
                display_full_frame: dirty_rects.len() == 1
                    && dirty_rects[0].w == width
                    && dirty_rects[0].h == height,
                ..Default::default()
            }
        }
        // ... other variants
        _ => DeviceSnapshot::default(),
    }
}
```

---

## 6. World Extensions

### 6.1 Pause/Resume (trivial — already has `running` flag)

```rust
// sim-world/src/world.rs
impl World {
    pub fn pause(&mut self)  { self.running = false; }
    pub fn resume(&mut self) { self.running = true; }
    pub fn is_paused(&self) -> bool { !self.running }
}
```

### 6.2 drain_new_traces

```rust
/// Per-machine trace cursor for streaming.
/// Initialized to 0, advanced each call.
trace_offsets: BTreeMap<u64, usize>,

pub fn drain_new_traces(&mut self) -> Vec<String> {
    let mut all = Vec::new();
    for machine in self.machines.values() {
        let offset = self.trace_offsets.entry(machine.id).or_insert(0);
        let events = machine.trace().events();
        let prefix = format!("[machine.{}]", machine.id);
        for ev in &events[*offset..] {
            all.push(format!("{} {}", prefix, ev));
        }
        *offset = events.len();
    }
    all
}
```

### 6.3 Keyframes

```rust
pub struct WorldKeyframe {
    pub now: Tick,
    pub machine_queues: BTreeMap<u64, Vec<QueuedEventShadow>>,
    pub link_pending: Vec<LinkStateShadow>,
    pub bus_pending: Vec<CanBusStateShadow>,
    pub fault_cursor: usize,
    pub ble_cursor: usize,
}

impl World {
    pub fn save_keyframe(&self, scenario: &Scenario) -> WorldKeyframe { ... }
    pub fn load_keyframe(&mut self, scenario: &Scenario, kf: &WorldKeyframe) -> Result<(), SimError> { ... }
}
```

Keyframes are stored per-session in the `SessionMap` as `Vec<(u64, Vec<u8>)>` where the bytes are bincode-serialized `WorldKeyframe`.

---

## 7. Session Management

### 7.1 session.rs

Adapted from `sim-runner/src/serve.rs`. The session map is straightforward:

```rust
use std::collections::HashMap;
use std::sync::Mutex;
use sim_world::World;

pub struct SessionMap {
    inner: Mutex<HashMap<u64, Session>>,
    next_id: AtomicU64,
}

struct Session {
    id: u64,
    world: Option<World>,
    scenario: Option<Scenario>,
    keyframes: Vec<(u64, Vec<u8>)>, // (keyframe_id, serialized WorldKeyframe)
    next_keyframe_id: u64,
    state: SessionState,
}

enum SessionState {
    Idle, Ready, Running, Paused, Done, Error(String),
}
```

`SessionMap` is `Send + Sync` because `Mutex` provides interior mutability. The `World` is temporarily moved out during `Run` streaming and returned after.

---

## 8. Implementation Phases

### Phase G1: VirtualDisplay + VirtualTouchScreen (5 days)

Same as Phases D1+D2 from previous handoff. New files in `sim-devices`, C ABI exports in `sim-ffi`, C header declarations. No gRPC dependency yet.

### Phase G2: Device Inspection Facade (3 days)

`sim-devices/src/inspect.rs` with `DeviceSnapshot::collect_all()`. Add `display_ids()`, `touch_ids()` helpers to `lib.rs`. Write unit tests.

### Phase G3: World Extensions (2 days)

Pause/resume, `drain_new_traces`, `WorldKeyframe`, `save_keyframe`/`load_keyframe`. Unit tests. No gRPC dependency yet.

### Phase G4: sim-grpc Crate Scaffold (2 days)

Create crate, `Cargo.toml` with tonic/prost deps, `build.rs`, `proto/simulator.proto`, verify it compiles. Register in workspace `Cargo.toml`.

### Phase G5: gRPC Service Implementation (4 days)

The `Simulator` trait implementation: all unary RPCs, the `Run` bidirectional stream, `inspect.rs` proto conversion. Port session management from `serve.rs`.

### Phase G6: Display Firmware Demo + Integration Test (2 days)

FreeRTOS C demo using display + touch. End-to-end test: start gRPC server, run simulation, verify DisplayFrame messages contain correct pixel data.

### Total: ~18 days

### Phase G7: Remove JSON-RPC from sim-runner? (Decision needed)

The existing `serve.rs` JSON-RPC server can stay for backward compatibility (`costar serve --stdio`), or be removed. Recommendation: keep it. The gRPC server is the primary interface for GUI, but the JSON-RPC server is useful for scripting and debugging. Two binaries: `costar` (CLI + legacy JSON-RPC) and `costar-grpc` (gRPC server).

---

## 9. Testing Strategy

### 9.1 Unit Tests

| Crate | Tests | Count |
|-------|-------|-------|
| sim-devices | VirtualDisplay pixel ops, dirty rects, bitmap draw | 6 |
| sim-devices | VirtualTouchScreen inject/read/overflow | 4 |
| sim-devices | DeviceSnapshot collect_all, display snapshot, proto round-trip | 6 |
| sim-world | WorldKeyframe save/load, drain_new_traces cursor | 5 |
| sim-grpc | Proto message construction, SessionMap create/load/destroy | 4 |

### 9.2 Integration Tests

```rust
// sim-grpc/tests/integration_test.rs
#[tokio::test]
async fn test_full_simulation_flow() {
    // 1. Start gRPC server on random port
    // 2. Create session
    // 3. Load inline scenario
    // 4. Configure board with display+touch
    // 5. Open Run stream
    // 6. Verify DisplayFrame messages arrive
    // 7. Send TouchInject
    // 8. Pause → verify paused event
    // 9. Resume → verify more frames
    // 10. Wait for SimulationEnd
}
```

### 9.3 Golden Trace

Existing golden trace tests must continue to pass unchanged. The display firmware demo gets its own golden trace (`tests/traces/expected_display.trace`).

---

## 10. Electron Integration Contract (gRPC-web)

The Electron team uses `@grpc/grpc-js` or `grpc-web` to generate a JS client from `simulator.proto`.

```javascript
// Electron main process
const grpc = require('@grpc/grpc-js');
const protoLoader = require('@grpc/proto-loader');

const packageDef = protoLoader.loadSync('simulator.proto');
const proto = grpc.loadPackageDefinition(packageDef).costar.simulator.v1;

const client = new proto.Simulator(
  'localhost:9321',
  grpc.credentials.createInsecure()
);

// 1. Setup
const { session_id } = await client.CreateSession({});
await client.LoadScenario({ session_id, scenario_toml });
await client.ConfigureBoard({ session_id, peripherals: [...] });

// 2. Run with display streaming
const runStream = client.Run();

// Send config as first message
runStream.write({ config: { session_id, tick_batch_size: 1000, stream_display: true } });

// Listen for display frames
runStream.on('data', (event) => {
  switch (event.payload) {
    case 'tick':
      updateTimeline(event.tick.ts);
      break;
    case 'display':
      renderToCanvas(event.display); // DirtyRect.data is raw bytes, no base64 decode!
      break;
    case 'paused':
      enableScrubControls();
      break;
    case 'end':
      runStream.end();
      break;
  }
});

// 3. Inject touch on canvas click
canvas.addEventListener('pointerdown', (e) => {
  const { x, y } = canvasToDisplayCoords(e);
  runStream.write({
    touch: {
      device_id: 0,
      events: [{ point_id: 0, x, y, pressure: 255, event_type: 'TOUCH_PRESS' }],
    },
  });
});

// 4. Pause/resume
pauseButton.onclick = () => runStream.write({ pause: {} });
resumeButton.onclick = () => runStream.write({ resume: {} });

// 5. While paused, inspect device state
const { devices } = await client.InspectDevices({ session_id, device_type: 'gpio' });
```

**Key advantage over JSON-RPC**: `DirtyRect.data` is raw `bytes` — the JS client receives a `Uint8Array` directly. No base64 decode step. For a 320×240 RGB565 display with small dirty rects, this is typically 1-5KB per frame, streamed efficiently over HTTP/2.

---

## 11. Checklist Before Handoff to Electron Team

```
[ ] sim-grpc crate compiles:  cargo build -p sim-grpc
[ ] .proto compiles:          protoc --decode_raw < test_frame.bin
[ ] gRPC server starts:       cargo run -p sim-grpc
[ ] grpcurl reaches it:       grpcurl -plaintext localhost:9321 list
[ ] Creates session:          grpcurl -plaintext -d '{}' localhost:9321 costar.simulator.v1.Simulator/CreateSession
[ ] Display firmware runs:    cargo run -- --mode display
[ ] Golden trace matches:     bash tests/golden_trace_test.sh display
[ ] All 320+ tests pass:      cargo test --workspace
[ ] No clippy warnings:       cargo clippy --all-targets -- -D warnings
[ ] Proto file reviewed:      simulator.proto is the API contract — no breaking changes after this point
```

The `simulator.proto` file should be treated as a versioned API artifact. Any changes after the Electron team starts building against it require coordinated version bumps.
