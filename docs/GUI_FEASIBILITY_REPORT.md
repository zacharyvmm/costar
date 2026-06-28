# costar → GUI Feasibility Report

Prepared: 2026-06-27
Purpose: Evaluate costar as the backend for an Electron-based GUI inspired by Cisco Packet Tracer, targeting embedded systems simulation with canvas-rendered displays.

---

## 1. Executive Summary

**Verdict: costar is a strong backend candidate (~70% ready) for a Packet Tracer-style GUI, but it needs ~4-6 weeks of focused additions before the GUI layer can be built on top.**

What works today: deterministic multi-machine simulation, a JSON-RPC 2.0 server with 14 methods, serializable trace events (JSONL), scenario DSL, CAN bus topology, Ethernet links, and firmware-in-the-loop.

What's missing for the GUI vision: no virtual display device (LCD/OLED), no touch screen, no live clock mutation, no mid-run event injection, no device state query via RPC, and no live rendering pipeline.

---

## 2. Architecture Fit: costar ↔ Electron GUI

### 2.1 How They Connect

```
┌─────────────────────────────────────────────────┐
│  Electron Frontend                               │
│  ┌───────────────────────────────────────────┐  │
│  │  React/Vue/Svelte UI                       │  │
│  │  - Topology canvas (machines, links, buses)│  │
│  │  - Per-machine device panels               │  │
│  │  - Display canvases (LCD/OLED rendering)   │  │
│  │  - Timeline scrubber / step control        │  │
│  │  - Scenario editor (drag & drop)           │  │
│  └──────────────┬────────────────────────────┘  │
│                 │ stdin/stdout (NDJSON)          │
│                 │ or TCP (127.0.0.1:9321)       │
│  ┌──────────────▼────────────────────────────┐  │
│  │  Electron Main Process                     │  │
│  │  - Spawns costar as child process          │  │
│  │  - JSON-RPC client (like mcu/client.go)    │  │
│  │  - Bridges RPC ↔ renderer via IPC         │  │
│  └──────────────┬────────────────────────────┘  │
└─────────────────┼───────────────────────────────┘
                  │ stdin/stdout or TCP
┌─────────────────▼───────────────────────────────┐
│  costar (Rust binary)                            │
│  costar serve --stdio                            │
│  ┌───────────────────────────────────────────┐  │
│  │  JSON-RPC 2.0 Server                       │  │
│  │  - session.create / destroy / clone        │  │
│  │  - scenario.load / load_inline             │  │
│  │  - sim.run / run_until / step / reset      │  │
│  │  - sim.status / stop                       │  │
│  │  - trace.get (human|jsonl) / trace.stream  │  │
│  │  - board.configure                         │  │
│  │  - server.version / shutdown               │  │
│  └──────────────────┬────────────────────────┘  │
│  ┌──────────────────▼────────────────────────┐  │
│  │  World (multi-machine orchestrator)        │  │
│  │  - Machines (event queue + fiber runtime)  │  │
│  │  - Links (FIFO channels w/ latency)        │  │
│  │  - CAN buses (broadcast topology)          │  │
│  │  - Plant models (physics co-simulation)    │  │
│  │  - Scenario DSL (TOML parsing)             │  │
│  └──────────────────┬────────────────────────┘  │
│  ┌──────────────────▼────────────────────────┐  │
│  │  Devices (18 types, thread-local maps)     │  │
│  │  UART, GPIO, Timer, I2C, SPI, CAN,        │  │
│  │  ADC, TempSensor, EEPROM, Flash,          │  │
│  │  EthDevice, FlatMemoryStore, HCI, Entropy │  │
│  └───────────────────────────────────────────┘  │
└─────────────────────────────────────────────────┘
```

### 2.2 JSON-RPC API (Existing — 14 Methods)

All available today. The Electron main process can use these to drive the simulation from JavaScript:

| Method | Purpose |
|--------|---------|
| `session.create` | Start a new simulation session |
| `session.destroy` | Tear down a session |
| `session.clone` | Fork a session (A/B testing) |
| `session.list` | List all active sessions |
| `scenario.load` | Load a TOML scenario from disk |
| `scenario.load_inline` | Load a TOML scenario from a string |
| `sim.run` | Run to completion, returns all traces |
| `sim.run_until` | Run until a virtual-time deadline |
| `sim.step` | Advance by N ticks, return new events |
| `sim.reset` | Rebuild world from stored scenario |
| `sim.status` | Query state + current virtual time |
| `sim.stop` | Signal async stop |
| `board.configure` | Initialize virtual peripherals from TOML |
| `trace.get` | Get collected traces (human or JSONL) |
| `trace.stream` | Stream traces as NDJSON (writes then runs) |
| `server.version` | Protocol negotiation |
| `server.shutdown` | Graceful server exit |

### 2.3 Trace Event Schema (Machine-Readable JSONL)

Every simulation event is serialized as JSON with a `"event"` discriminator field. The GUI can parse these in real time for display, timeline, and debugging:

```json
{"event":"TaskCreated","at":0,"task":1,"name":"Sender"}
{"event":"TaskResume","at":0,"task":1,"reason":"scheduler"}
{"event":"TaskYield","at":1,"task":1,"reason":"Cooperative"}
{"event":"InterruptRaised","at":2,"irq":5}
{"event":"InterruptDelivered","at":2,"irq":5}
{"event":"PacketRx","at":10,"len":64}
{"event":"PacketTx","at":15,"len":128}
{"event":"CanTx","at":100,"sender":0,"id":512,"len":8}
{"event":"CanRx","at":100,"receiver":2,"id":512,"len":8}
{"event":"UserU32","at":50,"label":"temperature_c","value":42}
```

Trace events already include `TaskCreated` (name-to-ID mapping), `CanTx`/`CanRx` (with sender/receiver machine IDs), and `PacketRx`/`PacketTx` (with byte lengths). The `UserU32` variant is a general-purpose hook for guest firmware to emit arbitrary numeric data into the trace — perfect for sensor readings, display pixel data, or custom metrics.

---

## 3. What's Already Good for the GUI

### 3.1 Multi-Machine Topology (✓ Ready)

World owns machines, links, and buses. The JSON-RPC `scenario.load` returns `n_machines`, `n_links`, `n_injections`. The Electron GUI can:
- Render a node-link diagram from scenario metadata
- Color-code machines by RTOS backend (FreeRTOS vs Zephyr)
- Show link latency as edge labels
- Display CAN bus as a broadcast cloud

### 3.2 Lockstep Virtual Time (✓ Ready)

All machines share one monotonic clock. `sim.run_until(deadline)` advances all machines to the same deadline. `sim.step(n_ticks)` advances by N ticks and returns exactly the new events. The GUI can implement:
- **Play/Pause/Step** — call `sim.step(1000)` in a loop, render events as they arrive
- **Timeline scrubber** — call `sim.run_until(target_time)` to jump to any point
- **Speed control** — vary step size (`n_ticks`) for fast-forward or slow-mo

### 3.3 Trace as GUI Data Source (✓ Ready)

`trace.get(format="jsonl")` returns machine-ID-prefixed JSON lines. Each event carries a virtual timestamp (`at` field). The GUI can:
- Build a timeline view of task scheduling
- Show packet flows between machines
- Highlight CAN frame routing on the topology
- Plot `UserU32` sensor data as time-series charts

### 3.4 Scenario DSL (✓ Ready)

TOML scenarios are human-writable, machine-parseable, and CI-friendly. The GUI can:
- Load a scenario, display its topology, let the user edit machines/links/injections
- Serialize edits back to TOML via `scenario.load_inline`
- Support save/load as `.toml` files

### 3.5 Device Ecosystem (✓ — 18 types, but no display)

| Device | C ABI | Thread-Local Map | Unit Tests |
|--------|-------|-----------------|------------|
| UART | sim_uart_write | UARTS | 9 |
| GPIO | sim_gpio_set | GPIOS | 11 |
| Timer | sim_timer_arm | TIMERS | 5 |
| IRQ Controller | sim_irq_raise | N/A (global) | 10 |
| I2C | sim_i2c_write/read | I2CS | 9 |
| SPI | sim_spi_transfer | SPIS | 11 |
| CAN | sim_can_send/recv | CANS | 9 |
| ADC | sim_adc_read | ADCS | 7 |
| TempSensor | sim_temp_read | TEMP_SENSORS | 4 |
| EEPROM | sim_eeprom_read/write | EEPROMS | 8 |
| Flash | sim_flash_read/write | FLASHES | 9 |
| FaultInjector | sim_fault_inject | FAULT_INJECTOR | 7 |
| Entropy | sim_entropy_u32 | ENTROPY_SOURCES | 5 |
| VirtualEthDevice | sim_eth_send/recv | (in sim-net) | 7 |
| FlatMemoryStore | sim_block_read/write | BLOCKS | 9 |
| VirtualHciController | sim_bt_send_cmd/recv_evt | BT_CTRLS | 9 |
| SmoltcpBridge | (internal) | (in sim-net) | 5 |
| TcpBridge/TapBridge | (internal) | (in sim-net) | 10 |

Every device follows the same pattern: thread-local `RefCell<BTreeMap<u32, Device>>` with `device_insert()` / `with_device_mut(id, closure)` accessors. Adding a new device (like a display) is a well-established recipe.

### 3.6 Dashboard Data via UserU32 (✓ Ready)

Guest firmware can call `sim_trace_u32("label", value)` from C, which emits `TraceEvent::UserU32 { at, label, value }` into the JSONL trace. An Electron GUI can parse these for live dashboards — temperature gauges, packet counters, button states, etc.

---

## 4. Missing Features — Blockers and Gaps

### 4.1 No Virtual Display Device (✗ — Blocker)

**There is no VirtualDisplay, VirtualLCD, VirtualOLED, or framebuffer device in the current codebase.** A search for "display", "lcd", "screen", "touch", "framebuffer" across all crates returns zero device models — only unrelated hits in FreeRTOS submodule docs.

To render firmware display output on an Electron canvas, costar needs:

```
New device: VirtualDisplay
├── width, height (pixels)
├── color_mode (RGB565, RGB888, Monochrome, etc.)
├── framebuffer: Vec<u8> (width × height × bytes_per_pixel)
├── dirty_rects: for partial updates (performance)
├── C ABI exports:
│   ├── sim_display_init(id, width, height, color_mode)
│   ├── sim_display_set_pixel(id, x, y, color)
│   ├── sim_display_fill_rect(id, x, y, w, h, color)
│   ├── sim_display_draw_bitmap(id, x, y, w, h, data)
│   ├── sim_display_get_framebuffer(id) → &[u8]
│   └── sim_display_get_dirty_rects(id) → [(x, y, w, h)]
├── JSON-RPC method: device.display.read(session_id, machine_id, device_id)
│   → returns { framebuffer: base64, dirty_rects: [...] }
└── Trace events: DisplayUpdate { at, machine, device, x, y, w, h }
```

**Effort estimate: ~3 days** (1 day for Rust model + tests, 1 day for C ABI + demo, 1 day for JSON-RPC integration).

### 4.2 No Touch Screen Simulation (✗ — Blocker)

No touch input device exists. For a Packet Tracer experience where the user can tap/click on a rendered display and those coordinates flow back to firmware:

```
New device: VirtualTouchScreen
├── paired_display_id (which display it overlays)
├── max_points (1-10 concurrent touches)
├── pending_touches: VecDeque<(x, y, pressure, event_type)>
├── C ABI exports:
│   ├── sim_touch_get_event(id) → Option<TouchEvent>
│   └── sim_touch_inject_input(id, x, y, event_type)
├── JSON-RPC method: device.touch.inject(session_id, machine_id, device_id, events)
│   → Electron sends click/tap/drag coordinates to firmware
└── Trace events: TouchEvent { at, machine, device, x, y, type }
```

**Effort estimate: ~2 days** (lightweight compared to display — touch input is just a FIFO queue).

### 4.3 No Live Virtual Clock Mutation (✗ — Partial Blocker)

**The virtual clock cannot be modified from outside during a running simulation.** The World run loop is synchronous:

```rust
// In world.rs — the run loop owns time advancement
pub fn run(&mut self) -> Result<(), SimError> {
    while self.running {
        let next_time = self.next_global_event_time();
        // ... advances self.now atomically, dispatches events
    }
}
```

For the GUI to let the user scrub the timeline while paused, or inject events at arbitrary virtual times, the following needs to exist:

1. **Event injection during run pause**: The World needs a `pause` state where it stops the event loop but preserves all internal state. Events injected via JSON-RPC during pause are queued and dispatched when the user hits Play.
2. **Clock scrubbing**: `sim.run_until(target)` exists, but the world must support backward scrubbing (replay). Currently `sim.reset()` rebuilds from scratch — O(scenario_parse + world_build) — not suitable for smooth scrubbing.
3. **Snapshot/checkpoint**: To scrub backward efficiently, the World needs `save_checkpoint() → Vec<u8>` and `restore_checkpoint(data)`. This serializes all machine event queues, fiber states, link buffers, and device states.

**Options:**
- **Minimal (1 day)**: Expose `sim.inject_event` RPC that queues events into a machine's schedule during pause. Scrubbing forward only (no rewind).
- **Full snapshot (4-6 days)**: serde-serializable World state + deterministic replay from initial state to any checkpoint. This is the proper solution for timeline scrubbing.

### 4.4 No Mid-Run Device State Query (✗ — Partial Blocker)

The JSON-RPC server can return traces but **cannot query live device state** during or after a run. For example, the GUI cannot ask "what is the current value of GPIO pin 3 on machine 2?"

All device state lives in thread-local `RefCell<BTreeMap<u32, T>>` maps. These are accessible within the Rust process but not exposed over the JSON-RPC wire.

**What's needed:**
```
New JSON-RPC methods:
├── device.list(session_id, machine_id)
│   → [{type: "uart", id: 0}, {type: "gpio", id: 0}, ...]
├── device.read(session_id, machine_id, device_type, device_id)
│   → UART: {tx_buffer_len, rx_buffer_len}
│   → GPIO: {pins: [{num, mode, state, value}]}
│   → CAN: {tx_queue_len, rx_queue_len, error_state}
│   → Timer: {armed, remaining_ticks, period}
│   → ADC: {channels: [{num, value, resolution}]}
│   → Display: {width, height, framebuffer_base64, dirty_rects}
│   → Touch: {pending_events: [{x, y, type}]}
└── device.write(session_id, machine_id, device_type, device_id, data)
    → Inject input into a device (e.g., touch coordinates, I2C slave data)
```

**Effort estimate: ~4-6 days** (add Serialize derives to all device structs, implement RPC handlers, add tests).

### 4.5 No True Real-Time Streaming (✗ — Minor)

`trace.stream` currently runs the entire simulation, then writes all traces as NDJSON and returns. For a GUI that wants per-tick progressive rendering:

**What's needed:** A `sim.stream` mode where the World advances one tick at a time, writes the events for that tick as NDJSON, then yields control back to the caller (or the RPC loop). Today `sim.step(n_ticks)` is close but not streaming — it collects all events across N ticks then returns them. A simple modification: add `sim.step_streaming(session_id, n_ticks)` that writes NDJSON events interleaved with `"event": "tick_boundary"` markers.

**Effort estimate: ~1 day.**

### 4.6 No Graphical Scenario Editor (✗ — Not in Scope for Backend)

The scenario DSL is TOML files. The GUI will need its own editor to build these. costar's role: parse the TOML, validate it, build the world, run the simulation. The Electron frontend owns the drag-and-drop topology editor entirely — costar just consumes the resulting TOML.

---

## 5. Questions Answered Directly

### Q: Is this repo good enough to build a Cisco Packet Tracer-like GUI for embedded systems?

**A: Yes, with additions.** costar provides 70% of what's needed as a backend. The deterministic virtual-time engine, multi-machine World, JSON-RPC server, trace system, and 18 device models form a solid foundation. The missing 30% is primarily the virtual display device, touch screen, live device state query, and snapshot/scrub infrastructure — all well-scoped additions that follow existing patterns.

### Q: What are the missing features?

See Section 4 above. Priority-ordered:
1. VirtualDisplay device (3 days)
2. VirtualTouchScreen device (2 days)
3. Live device state query via JSON-RPC (4-6 days)
4. Snapshot/checkpoint for timeline scrubbing (4-6 days)
5. Per-tick streaming mode (1 day)
6. Event injection during pause (1 day)

### Q: Is it easy to hook into the events for GUI display?

**A: Yes.** The `TraceEvent` enum already derives `serde::Serialize` and emits self-describing JSONL. The JSON-RPC server has `trace.get`, `trace.stream`, and `sim.step` that return structured JSON event arrays. An Electron main process can spawn `costar serve --stdio`, send JSON-RPC requests, parse the JSONL trace output, and forward events to the renderer via IPC. The `UserU32` trace variant provides a universal channel for firmware-to-GUI data without modifying the trace schema.

### Q: Can you modify the virtual clock LIVE?

**A: Not today.** The World run loop owns time advancement synchronously. You can call `sim.run_until(target)` to advance to an arbitrary time, or `sim.reset()` to restart from zero, but you cannot pause mid-run, mutate the clock, inject events, and resume. Adding this requires a snapshot/checkpoint system (see §4.3) — the World must be pausable and event-injectable at arbitrary virtual times.

### Q: Does the current system simulate displays?

**A: No.** There is no LCD, OLED, framebuffer, or display device of any kind. This is the single biggest gap for a Packet Tracer-like GUI. See §4.1 for the proposed `VirtualDisplay` design.

### Q: Does it simulate a display with a touch screen?

**A: No.** Neither display nor touch input exist. Both need to be built. See §4.1 and §4.2.

### Q: The display should render to a canvas. Is this feasible?

**A: Yes, once VirtualDisplay exists.** The flow would be:
1. Firmware writes pixels via `sim_display_set_pixel()` or `sim_display_draw_bitmap()`
2. VirtualDisplay tracks dirty rectangles in its framebuffer
3. Electron GUI calls `device.read(session_id, machine_id, "display", 0)` periodically (or after each tick)
4. The response includes the framebuffer as base64-encoded bytes
5. Electron renders to an HTML Canvas via `putImageData()` or `drawImage()`
6. Touch events flow back: Canvas click → Electron → `device.touch.inject(...)` → firmware `sim_touch_get_event()`

This is a clean two-way data flow — no hacks needed, just standard canvas rendering from a pixel buffer.

---

## 6. Recommended Implementation Plan

### Phase A: Backend Prerequisites (costar changes, ~2-3 weeks)

| Week | Task | Effort |
|------|------|--------|
| 1 | VirtualDisplay device + C ABI + golden trace demo | 3 days |
| 1 | VirtualTouchScreen device + C ABI | 2 days |
| 1 | JSON-RPC `device.list` / `device.read` / `device.write` | 4 days |
| 2 | Per-tick streaming mode (`sim.stream` RPC) | 1 day |
| 2 | Event injection during pause (`sim.pause` / `sim.inject_event`) | 1 day |
| 2 | Snapshot/checkpoint system for timeline scrubbing | 4 days |
| 3 | Integration tests, docs, golden traces for display + touch | 2 days |

### Phase B: Electron Frontend (separate repo, ~4-6 weeks)

| Week | Task |
|------|------|
| 4 | Electron scaffold — main process spawns costar, JSON-RPC client |
| 5 | Topology canvas — render machines/links/buses from scenario |
| 6 | Display canvases — framebuffer rendering + touch injection |
| 7 | Timeline + step controls — play/pause/scrub with streaming |
| 8 | Scenario editor — drag-drop machines, configure links, save as TOML |
| 9 | Device panels — GPIO pin states, UART terminal, CAN monitor, ADC gauges |
| 10 | Polish — themes, keyboard shortcuts, save/load, export |

### Phase C: Advanced Features (stretch)

- Oscilloscope-style signal viewer (GPIO/SPI/I2C traces over time)
- Logic analyzer from UART/SPI byte streams
- Network packet inspector (Wireshark-like for Ethernet/CAN frames)
- Multi-session side-by-side diff
- Record/replay of complete GUI interactions

---

## 7. Technical Notes for Electron Integration

### 7.1 Spawning costar

```javascript
// Electron main process (Node.js)
const { spawn } = require('child_process');

const costar = spawn('cargo', ['run', '--', 'serve', '--stdio'], {
  cwd: '/Users/zmm/projects/costar',
  stdio: ['pipe', 'pipe', 'pipe'], // stdin, stdout, stderr
});

let requestId = 0;
const pending = new Map();

costar.stdout.on('data', (chunk) => {
  for (const line of chunk.toString().split('\n').filter(Boolean)) {
    const msg = JSON.parse(line);
    if (pending.has(msg.id)) {
      pending.get(msg.id).resolve(msg);
      pending.delete(msg.id);
    } else {
      // Streaming event (trace.stream output)
      mainWindow.webContents.send('rpc:stream', msg);
    }
  }
});

function rpcCall(method, params = {}) {
  const id = ++requestId;
  const req = { jsonrpc: '2.0', id, method, params };
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    costar.stdin.write(JSON.stringify(req) + '\n');
  });
}
```

### 7.2 Display Canvas Rendering

```javascript
// Renderer process (browser)
// After each sim.step() or on polling interval:
const resp = await ipcRenderer.invoke('rpc', 'device.read', {
  session_id: sessionId,
  machine_id: 0,
  device_type: 'display',
  device_id: 0,
});

const canvas = document.getElementById('display-canvas');
const ctx = canvas.getContext('2d');
const imageData = ctx.createImageData(width, height);
// Decode base64 framebuffer → imageData.data (RGBA)
imageData.data.set(framebuffer);
ctx.putImageData(imageData, 0, 0);
```

### 7.3 Touch Injection

```javascript
canvas.addEventListener('click', (e) => {
  const rect = canvas.getBoundingClientRect();
  const x = Math.floor((e.clientX - rect.left) * (displayWidth / rect.width));
  const y = Math.floor((e.clientY - rect.top) * (displayHeight / rect.height));

  ipcRenderer.invoke('rpc', 'device.write', {
    session_id: sessionId,
    machine_id: 0,
    device_type: 'touch',
    device_id: 0,
    data: { type: 'press', x, y },
  });
});
```

---

## 8. Risk Assessment

| Risk | Probability | Impact | Mitigation |
|------|------------|--------|------------|
| Snapshot system is harder than estimated | Medium | High | Fall back to forward-only scrubbing with reset-to-zero |
| Display framebuffer too large for JSON-RPC | Medium | Medium | Use dirty rects only; compress with PNG in base64 |
| Thread-local device maps not accessible from RPC server thread | Low | High | RPC runs on main thread (stdio mode) — no threading issue. TCP mode spawns per-connection threads but sessions are isolated. |
| Electron ↔ Rust process IPC overhead | Low | Low | NDJSON is efficient; batch updates per tick |
| FreeRTOS/Zephyr firmware doesn't know about VirtualDisplay | Medium | Medium | Provide `sim_display.h` C header + demo firmware; same pattern as all other virtual devices |

---

## 9. Conclusion

costar is a uniquely well-positioned backend for a Packet Tracer-style embedded systems GUI. It already has deterministic multi-machine simulation, a JSON-RPC server, a rich trace system, and a growing device ecosystem. The missing pieces (virtual display, touch screen, live device query, snapshot/scrub) are each well-understood additions that follow the existing device model patterns.

The recommended approach: build the display + touch devices first (~1 week), then add the RPC query layer (~1 week), then the snapshot system (~1 week). At that point, the Electron GUI can begin in parallel — the RPC protocol is stable and the data shapes are defined.

Total backend work: ~3 weeks. Total Electron frontend: ~6 weeks. A working MVP GUI with canvas display rendering, touch injection, topology view, and step controls is achievable in ~9 weeks.
