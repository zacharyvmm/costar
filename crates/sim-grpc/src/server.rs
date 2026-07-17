//! gRPC service implementation for the costar simulator.
//!
//! Implements the `Simulator` trait generated from `simulator.proto`.
//! Handles session lifecycle, scenario loading, board configuration,
//! device inspection, keyframes, and the bidirectional Run stream.

use std::collections::HashMap;
use std::sync::{mpsc, Arc};

use tokio::sync::mpsc as tokio_mpsc;
use tonic::codegen::tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::proto::simulator_server::Simulator;
use crate::proto::*;
use crate::session::{SessionMap, RUNNING_ERR, SESSION_DONE_ERR, SESSION_ERROR_ERR};
use sim_world::firmware::FirmwareFactory;
use sim_world::{drive_world, BoardConfig, RunLimit, RunTermination, SessionState, World};

/// Commands sent from the gRPC client stream to the simulation thread.
enum ClientCommand {
    Touch {
        machine_id: Option<u64>,
        device_id: u32,
        events: Vec<sim_devices::TouchEvent>,
    },
    Adc {
        machine_id: Option<u64>,
        device_id: u32,
        channel: u32,
        value: u32,
    },
    DisplayFill {
        machine_id: Option<u64>,
        device_id: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        color: u32,
    },
    Pause,
    Resume,
    Stop,
    TimerArm {
        machine_id: Option<u64>,
        device_id: u32,
        delay_ticks: u64,
        period_ticks: u64,
    },
}

/// Registry mapping firmware paths to factories for loading guest firmware.
pub struct FirmwareRegistry {
    factories: HashMap<String, FirmwareFactory>,
}

impl FirmwareRegistry {
    /// Create an empty firmware registry.
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a firmware factory for a given path.
    pub fn register(&mut self, path: &str, factory: FirmwareFactory) {
        self.factories.insert(path.to_string(), factory);
    }

    /// Look up a factory by firmware path.
    pub fn get(&self, path: &str) -> Option<&FirmwareFactory> {
        self.factories.get(path)
    }
}

impl Default for FirmwareRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SimulatorServiceImpl {
    pub sessions: Arc<SessionMap>,
    firmware_registry: Option<FirmwareRegistry>,
}

impl SimulatorServiceImpl {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(SessionMap::new()),
            firmware_registry: None,
        }
    }

    /// Attach a firmware registry so that machines with a `firmware` field in
    /// the scenario get their firmware loaded automatically.
    pub fn with_firmware_registry(mut self, registry: FirmwareRegistry) -> Self {
        self.firmware_registry = Some(registry);
        self
    }
}

#[tonic::async_trait]
impl Simulator for SimulatorServiceImpl {
    async fn create_session(
        &self,
        _req: Request<CreateSessionRequest>,
    ) -> Result<Response<CreateSessionResponse>, Status> {
        let id = self.sessions.create().map_err(Status::resource_exhausted)?;
        Ok(Response::new(CreateSessionResponse { session_id: id }))
    }

    async fn destroy_session(
        &self,
        req: Request<DestroySessionRequest>,
    ) -> Result<Response<DestroySessionResponse>, Status> {
        let r = req.into_inner();
        let destroyed = self.sessions.destroy(r.session_id);
        Ok(Response::new(DestroySessionResponse { destroyed }))
    }

    async fn clone_session(
        &self,
        req: Request<CloneSessionRequest>,
    ) -> Result<Response<CloneSessionResponse>, Status> {
        let r = req.into_inner();
        match self.sessions.clone_session(r.session_id) {
            Ok(new_id) => Ok(Response::new(CloneSessionResponse {
                new_session_id: new_id,
            })),
            Err(e) => Err(map_session_err(e)),
        }
    }

    async fn list_sessions(
        &self,
        _req: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        let sessions: Vec<SessionInfo> = self
            .sessions
            .list()
            .into_iter()
            .map(|(id, state, now_ticks, n_machines)| SessionInfo {
                session_id: id,
                state,
                now_ticks,
                n_machines,
            })
            .collect();
        Ok(Response::new(ListSessionsResponse { sessions }))
    }

    async fn load_scenario(
        &self,
        req: Request<LoadScenarioRequest>,
    ) -> Result<Response<LoadScenarioResponse>, Status> {
        let r = req.into_inner();
        let toml = r.scenario_toml.clone();
        // Attach firmware *factories* inside the atomic load so the session
        // never becomes Ready with a World that lacks its registered factories.
        // Instantiation remains deferred until Run via `ensure_firmware_loaded`.
        let registry = self.firmware_registry.as_ref();
        match self
            .sessions
            .load_scenario_with(r.session_id, &toml, |scenario, world| {
                if let Some(registry) = registry {
                    for m in &scenario.machine {
                        if let Some(ref fw_path) = m.firmware {
                            if let Some(factory) = registry.get(fw_path) {
                                if let Some(machine) = world.machine_mut(m.id) {
                                    machine.set_firmware_factory(factory.clone());
                                }
                            }
                        }
                    }
                }
                Ok(())
            }) {
            Ok((n_machines, n_links, n_injections)) => Ok(Response::new(LoadScenarioResponse {
                n_machines,
                n_links,
                n_injections,
            })),
            Err(e) => Err(Status::invalid_argument(e)),
        }
    }

    async fn configure_board(
        &self,
        req: Request<ConfigureBoardRequest>,
    ) -> Result<Response<ConfigureBoardResponse>, Status> {
        let r = req.into_inner();
        let board = translate_peripherals(&r.peripherals).map_err(Status::invalid_argument)?;
        let machine_id = r.machine_id;
        let count = self
            .sessions
            .with_world_mut(r.session_id, |world| {
                let target = resolve_machine(world, machine_id)?;
                let machine = world
                    .machine_mut(target)
                    .ok_or_else(|| format!("machine {target} not found"))?;
                machine
                    .configure_board(board)
                    .map(|n| n as u32)
                    .map_err(|e| e.to_string())
            })
            .map_err(map_session_err)?;
        Ok(Response::new(ConfigureBoardResponse {
            n_peripherals: count,
        }))
    }

    async fn get_status(
        &self,
        req: Request<GetStatusRequest>,
    ) -> Result<Response<GetStatusResponse>, Status> {
        let r = req.into_inner();
        let status = self
            .sessions
            .status(r.session_id)
            .map_err(Status::not_found)?;
        Ok(Response::new(GetStatusResponse {
            state: status.state.as_str().to_string(),
            now_ticks: status.now_ticks,
            n_machines: status.n_machines,
            n_events: status.n_events as u32,
            error_message: status.error.unwrap_or_default(),
        }))
    }

    async fn inspect_devices(
        &self,
        req: Request<InspectDevicesRequest>,
    ) -> Result<Response<InspectDevicesResponse>, Status> {
        let r = req.into_inner();
        let machine_id = r.machine_id;
        let device_type = r.device_type.clone();
        let device_id = r.device_id;
        let devices = self
            .sessions
            .with_world(r.session_id, |world| {
                let target = resolve_machine(world, machine_id)?;
                // Collect snapshots inside the target machine's device context
                // so device id 0 resolves to *its* bank, never a sibling's.
                let snapshots = world
                    .with_machine_devices(target, sim_devices::inspect::DeviceSnapshot::collect_all)
                    .map_err(|e| e.to_string())?;
                let mut devices: Vec<DeviceSnapshot> = snapshots
                    .iter()
                    .filter(|s| {
                        let type_ok = device_type.is_empty() || s.type_str() == device_type;
                        let id_ok = device_id == 0 || s.device_id() == device_id;
                        type_ok && id_ok
                    })
                    .map(crate::inspect::to_proto)
                    .collect();
                // NetworkBank eth device 0 (not a BoardConfig peripheral).
                if let Ok(Some((rx_len, tx_len))) = world.eth_device_queue_lens(target, 0) {
                    let type_ok = device_type.is_empty() || device_type == "eth";
                    let id_ok = device_id == 0;
                    if type_ok && id_ok {
                        devices.push(DeviceSnapshot {
                            r#type: "eth".into(),
                            id: 0,
                            rx_buffer_len: rx_len as u32,
                            tx_buffer_len: tx_len as u32,
                            ..Default::default()
                        });
                    }
                }
                Ok(devices)
            })
            .map_err(map_session_err)?;
        Ok(Response::new(InspectDevicesResponse { devices }))
    }

    async fn save_keyframe(
        &self,
        req: Request<SaveKeyframeRequest>,
    ) -> Result<Response<SaveKeyframeResponse>, Status> {
        let r = req.into_inner();
        match self.sessions.save_keyframe(r.session_id) {
            Ok((kf_id, now_ticks, byte_size)) => Ok(Response::new(SaveKeyframeResponse {
                keyframe_id: kf_id,
                now_ticks,
                byte_size,
            })),
            Err(e) => Err(Status::internal(e)),
        }
    }

    async fn load_keyframe(
        &self,
        req: Request<LoadKeyframeRequest>,
    ) -> Result<Response<LoadKeyframeResponse>, Status> {
        let r = req.into_inner();
        match self.sessions.load_keyframe(r.session_id, r.keyframe_id) {
            Ok((restored, now_ticks)) => Ok(Response::new(LoadKeyframeResponse {
                restored,
                now_ticks,
            })),
            Err(e) => Err(Status::not_found(e)),
        }
    }

    async fn list_keyframes(
        &self,
        req: Request<ListKeyframesRequest>,
    ) -> Result<Response<ListKeyframesResponse>, Status> {
        let r = req.into_inner();
        match self.sessions.list_keyframes(r.session_id) {
            Ok(keyframes) => {
                let kfs: Vec<KeyframeInfo> = keyframes
                    .into_iter()
                    .map(|(id, now_ticks, byte_size)| KeyframeInfo {
                        keyframe_id: id,
                        now_ticks,
                        byte_size,
                    })
                    .collect();
                Ok(Response::new(ListKeyframesResponse { keyframes: kfs }))
            }
            Err(e) => Err(Status::not_found(e)),
        }
    }

    async fn reset_simulation(
        &self,
        req: Request<ResetSimulationRequest>,
    ) -> Result<Response<ResetSimulationResponse>, Status> {
        let r = req.into_inner();
        match self.sessions.reset(r.session_id) {
            Ok(()) => Ok(Response::new(ResetSimulationResponse { reset: true })),
            Err(e) => Err(Status::internal(e)),
        }
    }

    type RunStream = ReceiverStream<Result<RunEvent, Status>>;

    async fn run(
        &self,
        req: Request<Streaming<RunRequest>>,
    ) -> Result<Response<Self::RunStream>, Status> {
        let mut client_stream = req.into_inner();

        // Read the first message — MUST be RunConfig.
        let config = match client_stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {}", e)))?
        {
            Some(msg) => {
                if let Some(run_request::Payload::Config(config)) = msg.payload {
                    config
                } else {
                    return Err(Status::invalid_argument("first message must be RunConfig"));
                }
            }
            None => {
                return Err(Status::invalid_argument("first message must be RunConfig"));
            }
        };

        let session_id = config.session_id;
        let tick_batch = config.tick_batch_size.max(1);
        let deadline_ticks = (config.deadline_ticks != 0).then_some(config.deadline_ticks);
        let stream_display = config.stream_display;
        let stream_trace = config.stream_trace;

        let mut world = self
            .sessions
            .take_world(session_id)
            .map_err(map_session_err)?;

        if world.is_paused() {
            world.resume();
        }

        let (event_tx, event_rx) = tokio_mpsc::channel::<Result<RunEvent, Status>>(256);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ClientCommand>();
        let cmd_tx_clone = cmd_tx.clone();

        tokio::spawn(async move {
            while let Ok(Some(msg)) = client_stream.message().await {
                let cmd = match msg.payload {
                    Some(run_request::Payload::Touch(t)) => ClientCommand::Touch {
                        machine_id: t.machine_id,
                        device_id: t.device_id,
                        events: t
                            .events
                            .into_iter()
                            .map(|te| sim_devices::TouchEvent {
                                point_id: te.point_id,
                                x: te.x as u16,
                                y: te.y as u16,
                                pressure: te.pressure as u8,
                                event_type: match te.event_type() {
                                    TouchEventType::TouchPress => {
                                        sim_devices::TouchEventType::Press
                                    }
                                    TouchEventType::TouchRelease => {
                                        sim_devices::TouchEventType::Release
                                    }
                                    TouchEventType::TouchMove => sim_devices::TouchEventType::Move,
                                },
                            })
                            .collect(),
                    },
                    Some(run_request::Payload::Adc(a)) => ClientCommand::Adc {
                        machine_id: a.machine_id,
                        device_id: a.device_id,
                        channel: a.channel,
                        value: a.value,
                    },
                    Some(run_request::Payload::DisplayFill(f)) => ClientCommand::DisplayFill {
                        machine_id: f.machine_id,
                        device_id: f.device_id,
                        x: f.x,
                        y: f.y,
                        w: f.w,
                        h: f.h,
                        color: f.color,
                    },
                    Some(run_request::Payload::Pause(_)) => ClientCommand::Pause,
                    Some(run_request::Payload::Resume(_)) => ClientCommand::Resume,
                    Some(run_request::Payload::Stop(_)) => ClientCommand::Stop,
                    Some(run_request::Payload::TimerArm(t)) => ClientCommand::TimerArm {
                        machine_id: t.machine_id,
                        device_id: t.device_id,
                        delay_ticks: t.delay_ticks,
                        period_ticks: t.period_ticks,
                    },
                    _ => continue,
                };
                if cmd_tx_clone.send(cmd).is_err() {
                    break;
                }
            }
        });

        let sessions = Arc::clone(&self.sessions);

        std::thread::spawn(move || {
            let mut world = world;
            let mut n_events_sent: u64 = 0;

            // Boot firmware only now — after any ConfigureBoard RPCs that ran
            // while the session was Ready.
            ensure_firmware_loaded(&mut world);

            // The worker body funnels every batch through `drive_world` (which
            // catches guest panics inside `World::step`); the outer catch_unwind
            // is a backstop for panics in touch injection / display draining.
            let driven = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_worker_loop(
                    &mut world,
                    &cmd_rx,
                    &event_tx,
                    tick_batch,
                    deadline_ticks,
                    stream_trace,
                    stream_display,
                    &mut n_events_sent,
                )
            }));

            let (state, error) = match driven {
                Ok(res) => res,
                Err(p) => {
                    let msg = panic_to_string(p);
                    let _ = event_tx.blocking_send(Ok(RunEvent {
                        payload: Some(run_event::Payload::Error(SimulationError {
                            message: msg.clone(),
                        })),
                    }));
                    (SessionState::Error, Some(msg))
                }
            };

            if let Err(e) = sessions.return_world(session_id, world, state, n_events_sent, error) {
                log::warn!("failed to return world to session {}: {}", session_id, e);
            }
        });

        Ok(Response::new(ReceiverStream::new(event_rx)))
    }
}

impl Default for SimulatorServiceImpl {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Instantiate firmware from each machine's factory if not already loaded.
/// Called at Run start so ConfigureBoard can provision peripherals first.
fn ensure_firmware_loaded(world: &mut World) {
    let ids: Vec<u64> = world.machine_ids().collect();
    for id in ids {
        let Some(machine) = world.machine_mut(id) else {
            continue;
        };
        if machine.has_firmware() {
            continue;
        }
        let Some(factory) = machine.firmware_factory() else {
            continue;
        };
        machine.load_firmware(factory());
    }
}

/// Resolve the target machine for a request.
///
/// 1. `machine_id` present → require that machine.
/// 2. absent + exactly one machine → select it (compatibility).
/// 3. absent + zero or multiple machines → error (never pick the first).
fn resolve_machine(world: &World, machine_id: Option<u64>) -> Result<u64, String> {
    match machine_id {
        Some(id) => {
            if world.machine(id).is_some() {
                Ok(id)
            } else {
                Err(format!("machine {id} not found"))
            }
        }
        None => {
            let mut ids = world.machine_ids();
            match (ids.next(), ids.next()) {
                (Some(only), None) => Ok(only),
                _ => Err("machine_id required: world has zero or multiple machines".to_string()),
            }
        }
    }
}

/// Map a session-layer error string to the appropriate gRPC status.
fn map_session_err(e: String) -> Status {
    if e == RUNNING_ERR || e == SESSION_DONE_ERR || e == SESSION_ERROR_ERR {
        Status::failed_precondition(e)
    } else if e.contains("not found") {
        Status::not_found(e)
    } else if e.contains("session limit reached") {
        Status::resource_exhausted(e)
    } else {
        Status::invalid_argument(e)
    }
}

/// Translate a gRPC `PeripheralDef` list into a [`BoardConfig`].
///
/// Ported device types (uart/i2c/spi) are configured by id over gRPC and have
/// no real pin wiring, so placeholder port labels are synthesized to satisfy
/// board validation.
fn translate_peripherals(defs: &[PeripheralDef]) -> Result<BoardConfig, String> {
    use sim_world::board::PeripheralDef as BoardPeripheral;
    let mut board = BoardConfig::default();
    for def in defs {
        let mut p = BoardPeripheral {
            device: def.device.clone(),
            id: def.id,
            tx: None,
            rx: None,
            sda: None,
            scl: None,
            mosi: None,
            miso: None,
            sck: None,
            speed_hz: None,
            irq: None,
            display_width: None,
            display_height: None,
            color_mode: None,
            touch_display_id: None,
        };
        match def.device.as_str() {
            "uart" => {
                p.tx = Some("_".to_string());
                p.rx = Some("_".to_string());
                if def.baud_rate > 0 {
                    p.speed_hz = Some(def.baud_rate);
                }
            }
            "i2c" => {
                p.sda = Some("_".to_string());
                p.scl = Some("_".to_string());
                if def.i2c_speed_hz > 0 {
                    p.speed_hz = Some(def.i2c_speed_hz);
                }
            }
            "spi" => {
                p.mosi = Some("_".to_string());
                p.miso = Some("_".to_string());
                p.sck = Some("_".to_string());
                if def.spi_speed_hz > 0 {
                    p.speed_hz = Some(def.spi_speed_hz);
                }
            }
            "timer" => {
                if def.timer_irq > 0 {
                    p.irq = Some(def.timer_irq);
                }
            }
            "display" => {
                if def.display_width > 0 {
                    p.display_width = Some(def.display_width as u16);
                }
                if def.display_height > 0 {
                    p.display_height = Some(def.display_height as u16);
                }
                if !def.color_mode.is_empty() {
                    p.color_mode = Some(def.color_mode.clone());
                }
            }
            "touch" => {
                p.touch_display_id = Some(def.touch_display_id);
            }
            _ => {}
        }
        let label = format!("{}{}", def.device, def.id);
        board.peripherals.insert(label, p);
    }
    board.validate().map_err(|e| e.to_string())?;
    Ok(board)
}

/// Reduce a caught panic payload to a message string.
fn panic_to_string(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

/// Collect dirty-rect display frames for every display in the active bank,
/// tagging each frame with `machine_id`. Runs inside a machine device context.
fn collect_display_frames(machine_id: u64) -> Vec<RunEvent> {
    let mut frames = Vec::new();
    for id in sim_devices::display_ids() {
        let built = sim_devices::with_display_mut(id, |d| {
            let dirty = d.take_dirty_rects();
            if dirty.is_empty() {
                return None;
            }
            let full = dirty.len() == 1 && dirty[0].w == d.width && dirty[0].h == d.height;
            let bpp = d.color_mode.bytes_per_pixel();
            let row_stride = d.width as usize * bpp;
            let fb = d.framebuffer();
            let rects: Vec<DirtyRect> = dirty
                .iter()
                .filter_map(|r| {
                    if r.w == 0 || r.h == 0 {
                        return None;
                    }
                    let mut data = Vec::new();
                    for py in r.y..r.y + r.h {
                        let start = py as usize * row_stride + r.x as usize * bpp;
                        let end = start + r.w as usize * bpp;
                        if end <= fb.len() {
                            data.extend_from_slice(&fb[start..end]);
                        }
                    }
                    Some(DirtyRect {
                        x: r.x as u32,
                        y: r.y as u32,
                        w: r.w as u32,
                        h: r.h as u32,
                        data,
                    })
                })
                .collect();
            Some(RunEvent {
                payload: Some(run_event::Payload::Display(DisplayFrame {
                    device_id: id,
                    width: d.width as u32,
                    height: d.height as u32,
                    color_mode: format!("{}", d.color_mode),
                    dirty_rects: rects,
                    full_frame: full,
                    machine_id,
                })),
            })
        });
        if let Some(Some(frame)) = built {
            frames.push(frame);
        }
    }
    frames
}

/// The gRPC run worker loop. Returns the terminal session state and any error.
#[allow(clippy::too_many_arguments)]
fn run_worker_loop(
    world: &mut World,
    cmd_rx: &mpsc::Receiver<ClientCommand>,
    event_tx: &tokio_mpsc::Sender<Result<RunEvent, Status>>,
    tick_batch: u64,
    deadline_ticks: Option<u64>,
    stream_trace: bool,
    stream_display: bool,
    n_events_sent: &mut u64,
) -> (SessionState, Option<String>) {
    let send = |event| -> bool { event_tx.blocking_send(Ok(event)).is_ok() };
    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                ClientCommand::Touch {
                    machine_id,
                    device_id,
                    events,
                } => {
                    if let Ok(target) = resolve_machine(world, machine_id) {
                        let _ = world.with_machine_devices(target, || {
                            for ev in events {
                                sim_devices::with_touch_mut(device_id, |t| t.inject_event(ev));
                            }
                        });
                    }
                }
                ClientCommand::Adc {
                    machine_id,
                    device_id,
                    channel,
                    value,
                } => {
                    if let Ok(target) = resolve_machine(world, machine_id) {
                        let _ = world.with_machine_devices(target, || {
                            sim_devices::with_adc_mut(device_id, |adc| {
                                adc.inject_reading(channel as usize, value as u16);
                            });
                        });
                    }
                }
                ClientCommand::DisplayFill {
                    machine_id,
                    device_id,
                    x,
                    y,
                    w,
                    h,
                    color,
                } => {
                    if let Ok(target) = resolve_machine(world, machine_id) {
                        let _ = world.with_machine_devices(target, || {
                            sim_devices::with_display_mut(device_id, |d| {
                                d.fill_rect(x as u16, y as u16, w as u16, h as u16, color);
                            });
                        });
                    }
                }
                ClientCommand::Pause => world.pause(),
                ClientCommand::Resume => world.resume(),
                ClientCommand::Stop => {
                    world.stop();
                    let _ = send(RunEvent {
                        payload: Some(run_event::Payload::End(SimulationEnd {
                            ts: world.now,
                            total_ticks: world.now,
                            total_events: *n_events_sent,
                        })),
                    });
                    return (SessionState::Done, None);
                }
                ClientCommand::TimerArm {
                    machine_id,
                    device_id,
                    delay_ticks,
                    period_ticks,
                } => {
                    if let Ok(target) = resolve_machine(world, machine_id) {
                        let now = world.now;
                        let fire_at = now.saturating_add(delay_ticks);
                        let period = if period_ticks == 0 {
                            None
                        } else {
                            Some(period_ticks)
                        };
                        let _ = world.with_machine_devices(target, || {
                            sim_devices::with_timer_mut(device_id, |timer| {
                                timer.period = period;
                                timer.arm(now, delay_ticks);
                            });
                        });
                        if let Some(machine) = world.machine_mut(target) {
                            machine.schedule_at(fire_at, 10, "grpc_timer_expiry", Box::new(|_| {}));
                        }
                    }
                }
            }
        }

        if world.is_paused() {
            let _ = send(RunEvent {
                payload: Some(run_event::Payload::Paused(SimulationPaused {
                    ts: world.now,
                })),
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }

        let had_events = world.next_global_event_time().is_some();
        if !had_events || world.all_idle() {
            let _ = send(RunEvent {
                payload: Some(run_event::Payload::End(SimulationEnd {
                    ts: world.now,
                    total_ticks: world.now,
                    total_events: *n_events_sent,
                })),
            });
            return (SessionState::Done, None);
        }

        if deadline_ticks.is_some_and(|deadline| world.now >= deadline) {
            let _ = send(RunEvent {
                payload: Some(run_event::Payload::Paused(SimulationPaused {
                    ts: world.now,
                })),
            });
            return (SessionState::Paused, None);
        }

        let batch_deadline = world.now.saturating_add(tick_batch);
        let deadline = deadline_ticks.map_or(batch_deadline, |limit| limit.min(batch_deadline));
        let outcome = drive_world(world, RunLimit::Until(deadline));
        if matches!(
            outcome.termination,
            RunTermination::Error | RunTermination::Panic
        ) {
            let msg = outcome
                .error
                .unwrap_or_else(|| "simulation error".to_string());
            let _ = send(RunEvent {
                payload: Some(run_event::Payload::Error(SimulationError {
                    message: msg.clone(),
                })),
            });
            return (SessionState::Error, Some(msg));
        }

        // Drain expired virtual timers after advancing virtual time so runtime
        // TimerArm injections fire without guest firmware.
        for mid in world.machine_ids().collect::<Vec<_>>() {
            let now = world.now;
            let _ = world.with_machine_devices(mid, || {
                sim_devices::drain_expired_timers(now);
            });
        }

        // Advance across empty virtual time up to the batch/deadline boundary
        // only when the next event is strictly after that boundary.
        if deadline_ticks.is_some()
            && world.now < deadline
            && world.next_global_event_time().is_some_and(|t| t > deadline)
            && !world.all_idle()
        {
            world.now = deadline;
        }

        if !send(RunEvent {
            payload: Some(run_event::Payload::Tick(TickBoundary { ts: world.now })),
        }) {
            return (SessionState::Paused, None);
        }

        if stream_trace {
            for line in world.drain_new_traces() {
                if !send(RunEvent {
                    payload: Some(run_event::Payload::Trace(TraceLine { line })),
                }) {
                    return (SessionState::Paused, None);
                }
                *n_events_sent += 1;
            }
        }

        if stream_display {
            let ids: Vec<u64> = world.machine_ids().collect();
            for mid in ids {
                let frames = world
                    .with_machine_devices(mid, || collect_display_frames(mid))
                    .unwrap_or_default();
                for frame in frames {
                    if !send(frame) {
                        return (SessionState::Paused, None);
                    }
                    *n_events_sent += 1;
                }
            }
        }
    }
}
