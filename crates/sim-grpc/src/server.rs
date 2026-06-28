//! gRPC service implementation for the costar simulator.
//!
//! Implements the `Simulator` trait generated from `simulator.proto`.
//! Handles session lifecycle, scenario loading, board configuration,
//! device inspection, keyframes, and the bidirectional Run stream.

use std::sync::{mpsc, Arc};

use tokio::sync::mpsc as tokio_mpsc;
use tonic::codegen::tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::proto::simulator_server::Simulator;
use crate::proto::*;

use crate::session::SessionMap;
use sim_world::SessionState;

/// Commands sent from the gRPC client stream to the simulation thread.
enum ClientCommand {
    Touch {
        device_id: u32,
        events: Vec<sim_devices::TouchEvent>,
    },
    Pause,
    Resume,
    Stop,
}

pub struct SimulatorServiceImpl {
    pub sessions: Arc<SessionMap>,
}

impl SimulatorServiceImpl {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(SessionMap::new()),
        }
    }
}

#[tonic::async_trait]
impl Simulator for SimulatorServiceImpl {
    async fn create_session(
        &self,
        _req: Request<CreateSessionRequest>,
    ) -> Result<Response<CreateSessionResponse>, Status> {
        let id = self.sessions.create();
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
            Some(new_id) => Ok(Response::new(CloneSessionResponse {
                new_session_id: new_id,
            })),
            None => Err(Status::not_found(format!(
                "session {} not found",
                r.session_id
            ))),
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
        match self.sessions.load_scenario(r.session_id, &r.scenario_toml) {
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

        // Verify session exists and is ready.
        {
            let status = self
                .sessions
                .status(r.session_id)
                .map_err(Status::not_found)?;
            if status.state != SessionState::Ready && status.state != SessionState::Idle {
                return Err(Status::failed_precondition(
                    "session must be idle or ready to configure board",
                ));
            }
        }

        let mut count = 0u32;
        for def in &r.peripherals {
            match def.device.as_str() {
                "display" => {
                    let mode = match def.color_mode.as_str() {
                        "rgb565" => sim_devices::DisplayColorMode::Rgb565,
                        "rgb888" => sim_devices::DisplayColorMode::Rgb888,
                        "argb8888" => sim_devices::DisplayColorMode::Argb8888,
                        "" => sim_devices::DisplayColorMode::Rgb565,
                        other => {
                            return Err(Status::invalid_argument(format!(
                                "unknown color_mode: {}",
                                other
                            )))
                        }
                    };
                    let width = if def.display_width > 0 {
                        def.display_width as u16
                    } else {
                        320
                    };
                    let height = if def.display_height > 0 {
                        def.display_height as u16
                    } else {
                        240
                    };
                    sim_devices::display_insert(sim_devices::VirtualDisplay::new(
                        def.id, width, height, mode,
                    ));
                    count += 1;
                }
                "touch" => {
                    sim_devices::touch_insert(sim_devices::VirtualTouchScreen::new(
                        def.id,
                        def.touch_display_id,
                    ));
                    count += 1;
                }
                "uart" => {
                    let baud = if def.baud_rate > 0 {
                        def.baud_rate
                    } else {
                        115200
                    };
                    sim_devices::uart_insert(sim_devices::VirtualUart::new(def.id, baud));
                    count += 1;
                }
                "gpio" => {
                    sim_devices::gpio_insert(sim_devices::VirtualGpio::new(def.id));
                    count += 1;
                }
                "i2c" => {
                    let speed = if def.i2c_speed_hz > 0 {
                        def.i2c_speed_hz
                    } else {
                        100_000
                    };
                    sim_devices::i2c_insert(sim_devices::VirtualI2c::new(def.id, speed));
                    count += 1;
                }
                "spi" => {
                    let speed = if def.spi_speed_hz > 0 {
                        def.spi_speed_hz
                    } else {
                        1_000_000
                    };
                    sim_devices::spi_insert(sim_devices::VirtualSpi::new(def.id, speed));
                    count += 1;
                }
                "can" => {
                    sim_devices::can_insert(sim_devices::VirtualCan::new(def.id, 500_000));
                    count += 1;
                }
                "adc" => {
                    sim_devices::adc_insert(sim_devices::VirtualAdc::new(def.id));
                    count += 1;
                }
                "temp_sensor" => {
                    sim_devices::temp_sensor_insert(sim_devices::VirtualTempSensor::new(def.id));
                    count += 1;
                }
                "entropy" => {
                    sim_devices::entropy_insert(sim_devices::VirtualEntropy::new(def.id));
                    count += 1;
                }
                "eeprom" => {
                    sim_devices::eeprom_insert(sim_devices::VirtualEeprom::new(def.id));
                    count += 1;
                }
                "flash" => {
                    sim_devices::flash_insert(sim_devices::VirtualFlash::new(def.id));
                    count += 1;
                }
                "timer" => {
                    let irq = if def.timer_irq > 0 { def.timer_irq } else { 0 };
                    sim_devices::timer_insert(sim_devices::VirtualTimer::new_oneshot(def.id, irq));
                    count += 1;
                }
                unknown => {
                    return Err(Status::invalid_argument(format!(
                        "unknown device type: {}",
                        unknown
                    )))
                }
            }
        }
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
        let snapshots = sim_devices::inspect::DeviceSnapshot::collect_all();
        let devices: Vec<DeviceSnapshot> = snapshots
            .iter()
            .filter(|s| {
                let type_ok = r.device_type.is_empty() || s.type_str() == r.device_type;
                let id_ok = r.device_id == 0 || s.device_id() == r.device_id;
                type_ok && id_ok
            })
            .map(crate::inspect::to_proto)
            .collect();
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
        let stream_display = config.stream_display;
        let stream_trace = config.stream_trace;

        let mut world = self
            .sessions
            .take_world(session_id)
            .map_err(Status::not_found)?;

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
                    Some(run_request::Payload::Pause(_)) => ClientCommand::Pause,
                    Some(run_request::Payload::Resume(_)) => ClientCommand::Resume,
                    Some(run_request::Payload::Stop(_)) => ClientCommand::Stop,
                    _ => continue,
                };
                if cmd_tx_clone.send(cmd).is_err() {
                    break;
                }
            }
        });

        let sessions = Arc::clone(&self.sessions);

        std::thread::spawn(move || {
            let sessions = sessions;
            let session_id = session_id;
            let mut world = world;
            let mut n_events_sent: u64 = 0;

            loop {
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        ClientCommand::Touch { device_id, events } => {
                            for ev in events {
                                sim_devices::with_touch_mut(device_id, |t| {
                                    t.inject_event(ev);
                                });
                            }
                        }
                        ClientCommand::Pause => world.pause(),
                        ClientCommand::Resume => world.resume(),
                        ClientCommand::Stop => {
                            let _ = event_tx.blocking_send(Ok(RunEvent {
                                payload: Some(run_event::Payload::End(SimulationEnd {
                                    ts: world.now,
                                    total_ticks: world.now,
                                    total_events: n_events_sent,
                                })),
                            }));
                            let _ = sessions.return_world(
                                session_id,
                                world,
                                SessionState::Done,
                                n_events_sent,
                                None,
                            );
                            return;
                        }
                    }
                }

                if world.is_paused() {
                    let _ = event_tx.blocking_send(Ok(RunEvent {
                        payload: Some(run_event::Payload::Paused(SimulationPaused {
                            ts: world.now,
                        })),
                    }));
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }

                let had_events = world.next_global_event_time().is_some();
                if !had_events || world.all_idle() {
                    let _ = event_tx.blocking_send(Ok(RunEvent {
                        payload: Some(run_event::Payload::End(SimulationEnd {
                            ts: world.now,
                            total_ticks: world.now,
                            total_events: n_events_sent,
                        })),
                    }));
                    let _ = sessions.return_world(
                        session_id,
                        world,
                        SessionState::Done,
                        n_events_sent,
                        None,
                    );
                    return;
                }

                let deadline = world.now + tick_batch;
                if let Err(e) = world.run_until(deadline) {
                    let _ = event_tx.blocking_send(Ok(RunEvent {
                        payload: Some(run_event::Payload::Error(SimulationError {
                            message: e.to_string(),
                        })),
                    }));
                    let _ = sessions.return_world(
                        session_id,
                        world,
                        SessionState::Error,
                        n_events_sent,
                        Some(e.to_string()),
                    );
                    return;
                }

                let _ = event_tx.blocking_send(Ok(RunEvent {
                    payload: Some(run_event::Payload::Tick(TickBoundary { ts: world.now })),
                }));

                if stream_trace {
                    let traces = world.drain_new_traces();
                    for line in traces {
                        let _ = event_tx.blocking_send(Ok(RunEvent {
                            payload: Some(run_event::Payload::Trace(TraceLine { line })),
                        }));
                        n_events_sent += 1;
                    }
                }

                if stream_display {
                    for id in sim_devices::display_ids() {
                        if let Some(Some(frame)) = sim_devices::with_display_mut(id, |d| {
                            let dirty = d.take_dirty_rects();
                            if dirty.is_empty() {
                                return None;
                            }
                            let full =
                                dirty.len() == 1 && dirty[0].w == d.width && dirty[0].h == d.height;
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
                                })),
                            })
                        }) {
                            let _ = event_tx.blocking_send(Ok(frame));
                            n_events_sent += 1;
                        }
                    }
                }
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
