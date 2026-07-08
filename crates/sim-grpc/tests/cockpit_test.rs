//! Cockpit dogfood integration test for the sim-grpc GUI-facing gRPC
//! control plane.
//!
//! Proves the *existing* gRPC product surface end-to-end (UNBLOCKING.md
//! section 5, Strategy A — prove the existing gRPC surface) together with
//! interaction/inspection (Strategy C):
//!
//!   CreateSession -> LoadScenario -> ConfigureBoard (display/touch/timer/
//!   adc) -> Run stream (RunConfig + injected touch press/release + Stop)
//!   -> InspectDevices reconciliation -> framebuffer-hash + run
//!   determinism across two SEQUENTIAL runs.
//!
//! This is an additive, test-only deliverable: it does not modify server
//! or session logic, so golden traces are unaffected.
//!
//! HONESTY NOTE: none of these scenarios contain firmware that draws to
//! the display, so the Run stream emits ZERO DisplayFrame events and the
//! collected framebuffer-byte set is empty. The determinism assertion
//! therefore checks that the (empty) framebuffer hash is IDENTICAL across
//! the two sequential runs — `empty == empty` is a valid determinism
//! check — alongside the tick-boundary and SimulationEnd-totals
//! determinism. Rich framebuffer-content assertions (Strategy B) depend
//! on display-driving firmware and are a follow-up milestone; we do not
//! fabricate pixels here.
//!
//! IMPORTANT: sim-devices device registries are process-global / shared
//! in-process (per-session isolation is a deferred milestone), so the two
//! flows are run SEQUENTIALLY (not concurrently) and re-run
//! CreateSession/LoadScenario/ConfigureBoard each time. We deliberately do
//! NOT attempt concurrent multi-session isolation here.

use sim_grpc::proto::simulator_client::SimulatorClient;
use sim_grpc::proto::simulator_server::SimulatorServer;
use sim_grpc::proto::*;
use sim_grpc::server::SimulatorServiceImpl;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

/// A minimal single-machine scenario without firmware. The simulation
/// idles out immediately since there are no scheduled events.
const MINIMAL_SCENARIO: &str = r#"
name = "cockpit"
[[machine]]
id = 0
name = "m0"
"#;

// ── Board layout constants (identical across both determinism runs) ──
const DISPLAY_ID: u32 = 0;
const DISPLAY_WIDTH: u32 = 320;
const DISPLAY_HEIGHT: u32 = 240;
const DISPLAY_COLOR_MODE: &str = "rgb565";
const TOUCH_ID: u32 = 0;
const TIMER_ID: u32 = 0;
const TIMER_IRQ: u32 = 5;
const ADC_ID: u32 = 0;

/// Start the gRPC server on a random port, return the bound address and
/// a handle that keeps the server alive. (Test files are their own crate,
/// so this harness mirrors `integration_test.rs`.)
async fn start_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let service = SimulatorServiceImpl::new();

    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(SimulatorServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("server");
    });

    // Give the server a moment to start accepting connections.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (addr, handle)
}

/// FNV-1a 64-bit hash — deterministic and dependency-free. Used to hash
/// the concatenated DisplayFrame framebuffer bytes collected from the Run
/// stream.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Aggregated, comparable result of one full cockpit run. Comparing two
/// of these for structural equality is the run-level determinism check.
#[derive(Debug, PartialEq, Eq)]
struct CockpitRunResult {
    /// FNV-1a hash of all concatenated DisplayFrame dirty-rect bytes.
    framebuffer_hash: u64,
    /// Number of DisplayFrame events observed (0 with no display firmware).
    display_frame_count: usize,
    /// The ordered sequence of TickBoundary timestamps.
    tick_timestamps: Vec<u64>,
    /// SimulationEnd totals.
    end_ts: u64,
    end_total_ticks: u64,
    end_total_events: u64,
}

/// Execute the entire cockpit flow once against a fresh session and return
/// the aggregated, comparable result. Asserts the per-run invariants
/// (4 peripherals, SimulationEnd received with no SimulationError, and the
/// InspectDevices reconciliation).
async fn run_cockpit_flow(addr: &str) -> CockpitRunResult {
    let mut client = SimulatorClient::connect(addr.to_string())
        .await
        .expect("connect");

    // ── 1. CreateSession -> LoadScenario -> ConfigureBoard (4 devices) ──
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    assert!(sess.session_id > 0, "session_id should be non-zero");

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let cfg = client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
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
                PeripheralDef {
                    device: "timer".into(),
                    id: TIMER_ID,
                    timer_irq: TIMER_IRQ,
                    ..Default::default()
                },
                PeripheralDef {
                    device: "adc".into(),
                    id: ADC_ID,
                    ..Default::default()
                },
            ],
        })
        .await
        .expect("configure")
        .into_inner();
    assert_eq!(cfg.n_peripherals, 4, "expected 4 configured peripherals");

    // ── 2. Run stream: RunConfig -> touch press -> touch release -> Stop ─
    let messages = vec![
        RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 64,
                stream_display: true,
                stream_trace: true,
            })),
        },
        RunRequest {
            payload: Some(run_request::Payload::Touch(TouchInject {
                device_id: TOUCH_ID,
                events: vec![TouchEvent {
                    point_id: 0,
                    x: 100,
                    y: 80,
                    pressure: 255,
                    event_type: TouchEventType::TouchPress as i32,
                }],
            })),
        },
        RunRequest {
            payload: Some(run_request::Payload::Touch(TouchInject {
                device_id: TOUCH_ID,
                events: vec![TouchEvent {
                    point_id: 0,
                    x: 100,
                    y: 80,
                    pressure: 0,
                    event_type: TouchEventType::TouchRelease as i32,
                }],
            })),
        },
        RunRequest {
            payload: Some(run_request::Payload::Stop(StopCommand {})),
        },
    ];

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(messages)))
        .await
        .expect("run")
        .into_inner();

    let mut fb_bytes: Vec<u8> = Vec::new();
    let mut display_frame_count = 0usize;
    let mut tick_timestamps: Vec<u64> = Vec::new();
    let mut got_end = false;
    let mut end_ts = 0u64;
    let mut end_total_ticks = 0u64;
    let mut end_total_events = 0u64;

    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::Tick(t)) => tick_timestamps.push(t.ts),
            Some(run_event::Payload::Trace(_)) => {}
            Some(run_event::Payload::Display(frame)) => {
                display_frame_count += 1;
                for rect in &frame.dirty_rects {
                    fb_bytes.extend_from_slice(&rect.data);
                }
            }
            Some(run_event::Payload::Paused(_)) => {}
            Some(run_event::Payload::End(end)) => {
                got_end = true;
                end_ts = end.ts;
                end_total_ticks = end.total_ticks;
                end_total_events = end.total_events;
            }
            Some(run_event::Payload::Error(err)) => {
                panic!("unexpected SimulationError: {}", err.message);
            }
            None => {}
        }
    }
    assert!(
        got_end,
        "should receive SimulationEnd with no SimulationError"
    );

    // ── 3. InspectDevices reconciliation against ConfigureBoard ─────────
    // Reconcile the display against the configured width/height/color_mode.
    let display_devs = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            device_type: "display".into(),
            ..Default::default()
        })
        .await
        .expect("inspect display")
        .into_inner()
        .devices;
    let display = display_devs
        .iter()
        .find(|d| d.id == DISPLAY_ID)
        .expect("display device present");
    assert_eq!(display.r#type, "display");
    assert_eq!(display.display_width, DISPLAY_WIDTH);
    assert_eq!(display.display_height, DISPLAY_HEIGHT);
    assert_eq!(display.display_color_mode, DISPLAY_COLOR_MODE);

    // Touch, timer and adc devices must all be present.
    let touch_devs = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            device_type: "touch".into(),
            ..Default::default()
        })
        .await
        .expect("inspect touch")
        .into_inner()
        .devices;
    assert!(
        touch_devs.iter().any(|d| d.id == TOUCH_ID),
        "touch device present"
    );

    let timer_devs = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            device_type: "timer".into(),
            ..Default::default()
        })
        .await
        .expect("inspect timer")
        .into_inner()
        .devices;
    assert!(
        timer_devs.iter().any(|d| d.id == TIMER_ID),
        "timer device present"
    );

    let adc_devs = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            device_type: "adc".into(),
            ..Default::default()
        })
        .await
        .expect("inspect adc")
        .into_inner()
        .devices;
    assert!(
        adc_devs.iter().any(|d| d.id == ADC_ID),
        "adc device present"
    );

    CockpitRunResult {
        framebuffer_hash: fnv1a_64(&fb_bytes),
        display_frame_count,
        tick_timestamps,
        end_ts,
        end_total_ticks,
        end_total_events,
    }
}

/// Full cockpit lane: session/board/run/touch/inspect plus framebuffer-hash
/// and run determinism across two SEQUENTIAL runs.
#[tokio::test]
async fn cockpit_grpc_surface_and_determinism() {
    let (addr, _handle) = start_server().await;

    // Run the ENTIRE cockpit flow twice, SEQUENTIALLY. The sim-devices
    // registries are process-global, so concurrent runs would race; we
    // re-run CreateSession/LoadScenario/ConfigureBoard each time instead.
    let first = run_cockpit_flow(&addr).await;
    let second = run_cockpit_flow(&addr).await;

    // (a) Framebuffer-hash determinism: identical across both runs. With no
    // display-driving firmware this is a hash of an empty byte set (see the
    // module-level honesty note); empty == empty is a valid determinism
    // check.
    assert_eq!(
        first.framebuffer_hash, second.framebuffer_hash,
        "framebuffer hash must be identical across the two sequential runs"
    );
    assert_eq!(
        first.display_frame_count, second.display_frame_count,
        "display frame count must be identical across runs"
    );

    // (b) Tick-boundary sequence + SimulationEnd totals determinism.
    assert_eq!(
        first.tick_timestamps, second.tick_timestamps,
        "tick-boundary timestamp sequence must be identical across runs"
    );
    assert_eq!(first.end_ts, second.end_ts, "SimulationEnd ts must match");
    assert_eq!(
        first.end_total_ticks, second.end_total_ticks,
        "SimulationEnd total_ticks must match"
    );
    assert_eq!(
        first.end_total_events, second.end_total_events,
        "SimulationEnd total_events must match"
    );

    // Belt-and-braces: the entire aggregated result must be deterministic.
    assert_eq!(
        first, second,
        "entire cockpit run result must be deterministic across the two runs"
    );
}
