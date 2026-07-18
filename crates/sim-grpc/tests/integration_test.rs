//! Integration tests for the costar gRPC server.
//!
//! Tests exercise the full gRPC stack: session management, scenario
//! loading, board configuration, device inspection, keyframes, and
//! the bidirectional Run stream.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sim_core::Tick;
use sim_grpc::proto::simulator_client::SimulatorClient;
use sim_grpc::proto::simulator_server::SimulatorServer;
use sim_grpc::proto::*;
use sim_grpc::server::{FirmwareRegistry, SimulatorServiceImpl};
use sim_world::firmware::Firmware;
use sim_world::machine::Machine;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Server;

/// A minimal single-machine scenario without firmware.
/// The simulation will immediately idle out since there are no events.
const MINIMAL_SCENARIO: &str = r#"
name = "minimal"
[[machine]]
id = 0
name = "m0"
"#;

/// Start the gRPC server on a random port, return the bound address and
/// a handle that keeps the server alive.
async fn start_server_with_shared_sessions() -> (
    String,
    tokio::task::JoinHandle<()>,
    Arc<sim_grpc::session::SessionMap>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let sessions = Arc::new(sim_grpc::session::SessionMap::new());
    let service = SimulatorServiceImpl::with_session_map(Arc::clone(&sessions));

    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(SimulatorServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("server");
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (addr, handle, sessions)
}

async fn start_server() -> (String, tokio::task::JoinHandle<()>) {
    let (addr, handle, _sessions) = start_server_with_shared_sessions().await;
    (addr, handle)
}

// ── Session lifecycle ──────────────────────────────────────────────

#[tokio::test]
async fn test_create_and_destroy_session() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let resp = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    let sess_id = resp.session_id;
    assert!(sess_id > 0, "session_id should be non-zero");

    let resp = client
        .destroy_session(DestroySessionRequest {
            session_id: sess_id,
        })
        .await
        .expect("destroy")
        .into_inner();
    assert!(resp.destroyed);

    // Destroying again should return false.
    let resp = client
        .destroy_session(DestroySessionRequest {
            session_id: sess_id,
        })
        .await
        .expect("destroy2")
        .into_inner();
    assert!(!resp.destroyed);
}

#[tokio::test]
async fn test_clone_session() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    // Create a session and load a scenario first.
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load")
        .into_inner();

    let resp = client
        .clone_session(CloneSessionRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("clone")
        .into_inner();
    assert_ne!(resp.new_session_id, sess.session_id);
    assert!(resp.new_session_id > 0);
}

#[tokio::test]
async fn test_list_sessions() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create1");
    client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create2");

    let resp = client
        .list_sessions(ListSessionsRequest {})
        .await
        .expect("list")
        .into_inner();
    assert_eq!(resp.sessions.len(), 2);
}

// ── Scenario loading ────────────────────────────────────────────────

#[tokio::test]
async fn test_load_scenario() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    let resp = client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load")
        .into_inner();

    assert_eq!(resp.n_machines, 1);
    assert_eq!(resp.n_links, 0);
    assert_eq!(resp.n_injections, 0);

    // Status should now be "ready".
    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.state, "ready");
    assert_eq!(status.n_machines, 1);
}

// ── Board configuration ─────────────────────────────────────────────

#[tokio::test]
async fn test_configure_board_display_and_touch() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    // Load scenario first (required for board config).
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let resp = client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
            machine_id: None,
            peripherals: vec![
                PeripheralDef {
                    device: "display".into(),
                    id: 0,
                    display_width: 320,
                    display_height: 240,
                    color_mode: "rgb565".into(),
                    ..Default::default()
                },
                PeripheralDef {
                    device: "touch".into(),
                    id: 0,
                    touch_display_id: 0,
                    ..Default::default()
                },
            ],
        })
        .await
        .expect("configure")
        .into_inner();
    assert_eq!(resp.n_peripherals, 2);
}

#[tokio::test]
async fn test_configure_board_multiple_devices() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let resp = client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
            machine_id: None,
            peripherals: vec![
                PeripheralDef {
                    device: "uart".into(),
                    id: 0,
                    baud_rate: 115200,
                    ..Default::default()
                },
                PeripheralDef {
                    device: "gpio".into(),
                    id: 0,
                    ..Default::default()
                },
                PeripheralDef {
                    device: "i2c".into(),
                    id: 0,
                    ..Default::default()
                },
                PeripheralDef {
                    device: "spi".into(),
                    id: 0,
                    ..Default::default()
                },
            ],
        })
        .await
        .expect("configure")
        .into_inner();
    assert_eq!(resp.n_peripherals, 4);
}

// ── Device inspection ───────────────────────────────────────────────

#[tokio::test]
async fn test_inspect_devices() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    // Configure a display.
    client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
            machine_id: None,
            peripherals: vec![PeripheralDef {
                device: "display".into(),
                id: 0,
                display_width: 320,
                display_height: 240,
                color_mode: "rgb565".into(),
                ..Default::default()
            }],
        })
        .await
        .expect("configure");

    // Inspect all devices.
    let resp = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            ..Default::default()
        })
        .await
        .expect("inspect")
        .into_inner();
    assert!(!resp.devices.is_empty(), "should find at least one device");

    // Filter by type.
    let resp = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            device_type: "display".into(),
            ..Default::default()
        })
        .await
        .expect("inspect_type")
        .into_inner();
    assert_eq!(resp.devices.len(), 1);
    assert_eq!(resp.devices[0].r#type, "display");
    assert_eq!(resp.devices[0].id, 0);
    assert_eq!(resp.devices[0].display_width, 320);
    assert_eq!(resp.devices[0].display_height, 240);
    assert_eq!(resp.devices[0].display_color_mode, "rgb565");
}

// ── Run stream (basic) ──────────────────────────────────────────────

#[tokio::test]
async fn test_run_stream_basic() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    // Use tonic bidirectional streaming.
    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run")
        .into_inner();

    // The simulation should immediately end (no events).
    let mut got_end = false;
    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::End(end)) => {
                assert_eq!(end.ts, 0);
                got_end = true;
            }
            Some(run_event::Payload::Error(err)) => {
                panic!("unexpected error: {}", err.message);
            }
            _ => {}
        }
    }
    assert!(got_end, "should receive SimulationEnd");
}

#[tokio::test]
async fn test_run_stream_pause_resume() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    // With an empty world (no events), the simulation ends immediately.
    // Pause/Resume are tested via the RunConfig + Pause + Resume messages;
    // the sim sends End since there are no events to process.
    let messages = vec![
        RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        },
        RunRequest {
            payload: Some(run_request::Payload::Pause(PauseCommand {})),
        },
        RunRequest {
            payload: Some(run_request::Payload::Resume(ResumeCommand {})),
        },
    ];

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(messages)))
        .await
        .expect("run")
        .into_inner();

    let mut got_end = false;
    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::End(_)) => {
                got_end = true;
            }
            Some(run_event::Payload::Error(err)) => {
                panic!("unexpected error: {}", err.message);
            }
            _ => {}
        }
    }
    assert!(got_end, "should receive SimulationEnd");
}

#[tokio::test]
async fn test_run_stream_stop() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let messages = vec![
        RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 10000,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
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

    let mut got_end = false;
    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::End(_)) => {
                got_end = true;
            }
            Some(run_event::Payload::Error(err)) => {
                panic!("unexpected error: {}", err.message);
            }
            _ => {}
        }
    }
    assert!(got_end, "should receive SimulationEnd after stop");
}

#[tokio::test]
async fn test_run_cannot_restart_after_stop_until_reset() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let messages = vec![
        RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 10000,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        },
        RunRequest {
            payload: Some(run_request::Payload::Stop(StopCommand {})),
        },
    ];

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(messages)))
        .await
        .expect("first run")
        .into_inner();

    let mut got_end = false;
    while let Ok(Some(event)) = stream.message().await {
        if matches!(event.payload, Some(run_event::Payload::End(_))) {
            got_end = true;
        }
    }
    assert!(got_end, "first run must end after stop");

    for _ in 0..20 {
        let status = client
            .get_status(GetStatusRequest {
                session_id: sess.session_id,
            })
            .await
            .expect("status")
            .into_inner();
        if status.state == "done" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status after stop")
        .into_inner();
    assert_eq!(status.state, "done", "stop must leave session terminal");

    let second = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await;
    let err = second.expect_err("second run on done session must fail");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("done"),
        "unexpected error message: {}",
        err.message()
    );

    client
        .reset_simulation(ResetSimulationRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("reset")
        .into_inner();

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status after reset")
        .into_inner();
    assert_eq!(status.state, "ready");

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run after reset")
        .into_inner();

    let mut got_end = false;
    while let Ok(Some(event)) = stream.message().await {
        if matches!(event.payload, Some(run_event::Payload::End(_))) {
            got_end = true;
        }
    }
    assert!(got_end, "run after reset must complete");
}

struct HoldRunFirmware;

impl Firmware for HoldRunFirmware {
    fn init(&mut self, machine: &mut Machine) {
        machine.schedule_at(1_000_000, 0, "hold", Box::new(|_| {}));
    }
}

const TIMER_SCENARIO: &str = r#"
name = "timer_hold"
[[machine]]
id = 0
name = "m0"
firmware = "hold_fw"
"#;

async fn start_server_with_hold_firmware() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let mut registry = FirmwareRegistry::new();
    registry.register(
        "hold_fw",
        Arc::new(|| Box::new(HoldRunFirmware) as Box<dyn Firmware>),
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

#[tokio::test]
async fn test_run_stream_timer_arm_fires() {
    let (addr, _handle) = start_server_with_hold_firmware().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: TIMER_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
            machine_id: None,
            peripherals: vec![PeripheralDef {
                device: "timer".into(),
                id: 0,
                timer_irq: 16,
                ..Default::default()
            }],
        })
        .await
        .expect("configure");

    let messages = vec![
        RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 100,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 5_000,
            })),
        },
        RunRequest {
            payload: Some(run_request::Payload::TimerArm(TimerArm {
                machine_id: None,
                device_id: 0,
                delay_ticks: 100,
                period_ticks: 0,
            })),
        },
    ];

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(messages)))
        .await
        .expect("run")
        .into_inner();

    let mut got_paused = false;
    while let Ok(Some(event)) = stream.message().await {
        if matches!(event.payload, Some(run_event::Payload::Paused(_))) {
            got_paused = true;
        }
    }
    assert!(got_paused, "timer run must pause at deadline");

    let resp = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            device_type: "timer".into(),
            device_id: 0,
            ..Default::default()
        })
        .await
        .expect("inspect timer")
        .into_inner();
    assert_eq!(resp.devices.len(), 1);
    assert!(
        resp.devices[0].timer_fire_count >= 1,
        "timer must fire at least once, got {}",
        resp.devices[0].timer_fire_count
    );
    assert!(
        resp.devices[0].timer_last_fire_tick >= 100,
        "fire tick must reach deadline, got {}",
        resp.devices[0].timer_last_fire_tick
    );
}

async fn wait_for_session_state(
    client: &mut SimulatorClient<tonic::transport::Channel>,
    session_id: u64,
    want: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(resp) = client.get_status(GetStatusRequest { session_id }).await {
            if resp.into_inner().state == want {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test]
async fn destroy_running_session_is_rejected() {
    let (addr, _handle) = start_server_with_hold_firmware().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: TIMER_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let (run_tx, run_rx) = tokio::sync::mpsc::channel(4);
    run_tx
        .send(RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 100,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        })
        .await
        .expect("send run config");

    let mut run_stream = client
        .run(tonic::Request::new(ReceiverStream::new(run_rx)))
        .await
        .expect("run")
        .into_inner();

    assert!(
        wait_for_session_state(
            &mut client,
            sess.session_id,
            "running",
            Duration::from_secs(5)
        )
        .await,
        "session must reach Running before destroy is tested"
    );

    let destroy_err = client
        .destroy_session(DestroySessionRequest {
            session_id: sess.session_id,
        })
        .await
        .expect_err("destroy must fail while Running");
    assert_eq!(
        destroy_err.code(),
        tonic::Code::FailedPrecondition,
        "destroy while Running must be FailedPrecondition: {}",
        destroy_err.message()
    );
    assert!(
        destroy_err.message().contains("running"),
        "unexpected destroy error: {}",
        destroy_err.message()
    );

    let listed = client
        .list_sessions(ListSessionsRequest {})
        .await
        .expect("list")
        .into_inner();
    assert!(
        listed
            .sessions
            .iter()
            .any(|s| s.session_id == sess.session_id),
        "Running session must remain listed after rejected destroy"
    );

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status while Running")
        .into_inner();
    assert_eq!(status.state, "running");

    run_tx
        .send(RunRequest {
            payload: Some(run_request::Payload::Stop(StopCommand {})),
        })
        .await
        .expect("send stop");

    let mut got_end = false;
    while let Ok(Some(event)) = run_stream.message().await {
        if matches!(event.payload, Some(run_event::Payload::End(_))) {
            got_end = true;
        }
    }
    assert!(got_end, "stop must terminate the run stream");

    assert!(
        wait_for_session_state(&mut client, sess.session_id, "done", Duration::from_secs(5)).await,
        "session must become Done after stop"
    );

    let destroyed = client
        .destroy_session(DestroySessionRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("destroy after stop")
        .into_inner();
    assert!(destroyed.destroyed, "Done session must be destroyed");

    let listed = client
        .list_sessions(ListSessionsRequest {})
        .await
        .expect("list after destroy")
        .into_inner();
    assert!(
        !listed
            .sessions
            .iter()
            .any(|s| s.session_id == sess.session_id),
        "destroyed session must disappear from list"
    );

    let gone = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await;
    assert!(gone.is_err(), "destroyed session must not be queryable");
}

#[tokio::test]
async fn destroy_vs_run_race_stress() {
    let (addr, _handle) = start_server_with_hold_firmware().await;

    for trial in 0..100 {
        let mut client = SimulatorClient::connect(addr.clone())
            .await
            .expect("connect");

        let sess = client
            .create_session(CreateSessionRequest {})
            .await
            .expect("create")
            .into_inner();

        client
            .load_scenario(LoadScenarioRequest {
                session_id: sess.session_id,
                scenario_toml: TIMER_SCENARIO.to_string(),
            })
            .await
            .expect("load");

        let session_id = sess.session_id;
        let (run_tx, run_rx) = tokio::sync::mpsc::channel(4);
        run_tx
            .send(RunRequest {
                payload: Some(run_request::Payload::Config(RunConfig {
                    session_id,
                    tick_batch_size: 100,
                    stream_display: false,
                    stream_trace: false,
                    deadline_ticks: 0,
                })),
            })
            .await
            .expect("send run config");

        let mut run_client = client.clone();
        let run_handle = tokio::spawn(async move {
            run_client
                .run(tonic::Request::new(ReceiverStream::new(run_rx)))
                .await
        });

        let mut destroy_client = client.clone();
        let destroy_handle = tokio::spawn(async move {
            destroy_client
                .destroy_session(DestroySessionRequest { session_id })
                .await
        });

        let run_result = run_handle.await.expect("run task join");
        let destroy_result = destroy_handle.await.expect("destroy task join");

        match (run_result, destroy_result) {
            (Ok(run_resp), Err(status)) => {
                assert_eq!(
                    status.code(),
                    tonic::Code::FailedPrecondition,
                    "trial {trial}: destroy must fail when run wins"
                );
                run_tx
                    .send(RunRequest {
                        payload: Some(run_request::Payload::Stop(StopCommand {})),
                    })
                    .await
                    .ok();
                let mut stream = run_resp.into_inner();
                while stream.message().await.ok().flatten().is_some() {}
            }
            (Err(status), Ok(resp)) if status.code() == tonic::Code::NotFound => {
                assert!(
                    resp.into_inner().destroyed,
                    "trial {trial}: destroy should remove session when it wins the race"
                );
            }
            (Err(status), Ok(resp)) if status.code() == tonic::Code::FailedPrecondition => {
                assert!(
                    !resp.into_inner().destroyed,
                    "trial {trial}: unexpected destroy success alongside run precondition failure"
                );
            }
            (Ok(_), Ok(resp)) => {
                panic!(
                    "trial {trial}: forbidden double success (run ok, destroy ok destroyed={})",
                    resp.into_inner().destroyed
                );
            }
            other => panic!("trial {trial}: unexpected race outcome: {:?}", other.0),
        }

        if client
            .list_sessions(ListSessionsRequest {})
            .await
            .map(|r| {
                r.into_inner()
                    .sessions
                    .iter()
                    .any(|s| s.session_id == session_id)
            })
            .unwrap_or(false)
        {
            let status = client
                .get_status(GetStatusRequest { session_id })
                .await
                .expect("trial {trial}: listed session must remain status-queryable")
                .into_inner();
            if status.state == "running" || status.state == "paused" {
                let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
                stop_tx
                    .send(RunRequest {
                        payload: Some(run_request::Payload::Stop(StopCommand {})),
                    })
                    .await
                    .ok();
                let mut drain = client
                    .run(tonic::Request::new(ReceiverStream::new(stop_rx)))
                    .await
                    .expect("stop run")
                    .into_inner();
                while drain.message().await.ok().flatten().is_some() {}
            }
            let destroyed = client
                .destroy_session(DestroySessionRequest { session_id })
                .await
                .expect("trial {trial}: cleanup destroy")
                .into_inner();
            assert!(destroyed.destroyed, "trial {trial}: cleanup must destroy");
        }

        let listed = client
            .list_sessions(ListSessionsRequest {})
            .await
            .expect("trial {trial}: list after cleanup")
            .into_inner();
        assert!(
            !listed.sessions.iter().any(|s| s.session_id == session_id),
            "trial {trial}: session must not survive iteration"
        );
    }
}

// ── Keyframes ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_save_and_list_keyframes() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    // Save first keyframe.
    let kf1 = client
        .save_keyframe(SaveKeyframeRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("save1")
        .into_inner();
    assert!(kf1.keyframe_id > 0);
    assert_eq!(kf1.now_ticks, 0);

    // Save second keyframe.
    let kf2 = client
        .save_keyframe(SaveKeyframeRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("save2")
        .into_inner();
    assert!(kf2.keyframe_id > kf1.keyframe_id);

    // List keyframes.
    let list = client
        .list_keyframes(ListKeyframesRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("list")
        .into_inner();
    assert_eq!(list.keyframes.len(), 2);
}

#[tokio::test]
async fn test_load_keyframe() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let kf = client
        .save_keyframe(SaveKeyframeRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("save")
        .into_inner();

    let resp = client
        .load_keyframe(LoadKeyframeRequest {
            session_id: sess.session_id,
            keyframe_id: kf.keyframe_id,
        })
        .await
        .expect("load")
        .into_inner();
    assert!(resp.restored);
    assert_eq!(resp.now_ticks, 0);
}

#[tokio::test]
async fn test_load_nonexistent_keyframe() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let result = client
        .load_keyframe(LoadKeyframeRequest {
            session_id: sess.session_id,
            keyframe_id: 99999,
        })
        .await;
    assert!(result.is_err(), "should fail for nonexistent keyframe");
}

// ── Reset ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_reset_simulation() {
    let (addr, _handle) = start_server().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();

    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    // Run the simulation (it ends immediately).
    let messages = vec![RunRequest {
        payload: Some(run_request::Payload::Config(RunConfig {
            session_id: sess.session_id,
            tick_batch_size: 10,
            stream_display: false,
            stream_trace: false,
            deadline_ticks: 0,
        })),
    }];
    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(messages)))
        .await
        .expect("run")
        .into_inner();
    // Drain stream to end.
    while let Ok(Some(_)) = stream.message().await {}

    // Reset should succeed (scenario was stored).
    let resp = client
        .reset_simulation(ResetSimulationRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("reset")
        .into_inner();
    assert!(resp.reset);

    // Status should be "ready" again.
    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.state, "ready");
}

// ── Run deadline ────────────────────────────────────────────────────

/// Firmware with pending work beyond the deadline, so the world remains live.
struct DeadlineFirmware;

impl Firmware for DeadlineFirmware {
    fn init(&mut self, machine: &mut Machine) {
        machine.schedule_at(100, 0, "after_deadline", Box::new(|_| {}));
    }
}

const DEADLINE_SCENARIO: &str = r#"
name = "deadline_fw"
[[machine]]
id = 0
name = "m0"
firmware = "deadline_fw"
"#;

async fn start_server_with_deadline_firmware() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));

    let mut registry = FirmwareRegistry::new();
    registry.register(
        "deadline_fw",
        Arc::new(|| Box::new(DeadlineFirmware) as Box<dyn Firmware>),
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

#[tokio::test]
async fn run_deadline_pauses_at_requested_virtual_tick() {
    let (addr, _handle) = start_server_with_deadline_firmware().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: DEADLINE_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 100,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 50,
            })),
        }])))
        .await
        .expect("run")
        .into_inner();

    let mut paused_at = None;
    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::Paused(paused)) => paused_at = Some(paused.ts),
            Some(run_event::Payload::End(end)) => {
                panic!("deadline must pause a live world, got end at {}", end.ts)
            }
            Some(run_event::Payload::Error(err)) => panic!("unexpected error: {}", err.message),
            _ => {}
        }
    }
    assert_eq!(paused_at, Some(50));

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.state, "paused");
    assert_eq!(status.now_ticks, 50);
}

// ── Sparse cooperative batch (WS5) ───────────────────────────────────

struct SparseEventFirmware {
    event_at: u64,
    fired: Arc<AtomicU64>,
}

impl Firmware for SparseEventFirmware {
    fn init(&mut self, machine: &mut Machine) {
        let fired = Arc::clone(&self.fired);
        let at = self.event_at;
        machine.schedule_at(
            at,
            0,
            "sparse_event",
            Box::new(move |_| {
                fired.fetch_add(1, Ordering::SeqCst);
            }),
        );
    }
}

const SPARSE_SCENARIO: &str = r#"
name = "sparse_fw"
[[machine]]
id = 0
name = "m0"
firmware = "sparse_fw"
"#;

async fn start_server_with_sparse_firmware(
    event_at: u64,
    fired: Arc<AtomicU64>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let mut registry = FirmwareRegistry::new();
    registry.register(
        "sparse_fw",
        Arc::new(move || {
            Box::new(SparseEventFirmware {
                event_at,
                fired: Arc::clone(&fired),
            }) as Box<dyn Firmware>
        }),
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

#[tokio::test]
async fn unbounded_sparse_event_makes_progress() {
    let fired = Arc::new(AtomicU64::new(0));
    let (addr, _handle) = start_server_with_sparse_firmware(10_000, Arc::clone(&fired)).await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: SPARSE_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let started = Instant::now();
    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 1_000,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run")
        .into_inner();

    let mut tick_timestamps = Vec::new();
    let mut got_end = false;
    while let Ok(Some(event)) = stream.message().await {
        if let Some(run_event::Payload::Tick(tick)) = event.payload {
            tick_timestamps.push(tick.ts);
        }
        if matches!(event.payload, Some(run_event::Payload::End(_))) {
            got_end = true;
            break;
        }
        if let Some(run_event::Payload::Error(err)) = event.payload {
            panic!("unexpected error: {}", err.message);
        }
    }

    assert!(got_end, "unbounded sparse run must complete");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "must not spin: elapsed {:?}",
        started.elapsed()
    );
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "sparse event must fire once"
    );
    assert!(
        tick_timestamps.iter().any(|&t| t >= 10_000),
        "ticks must reach event time: {tick_timestamps:?}"
    );
    for window in tick_timestamps.windows(2) {
        assert!(
            window[1] >= window[0],
            "tick timestamps must be monotonic: {tick_timestamps:?}"
        );
    }
    let stagnant = tick_timestamps.iter().filter(|&&t| t == 0).count();
    assert!(
        stagnant <= 1,
        "must not emit unbounded tick=0 sequence: {tick_timestamps:?}"
    );
}

#[tokio::test]
async fn sparse_bounded_event_pauses_then_resumes() {
    let fired = Arc::new(AtomicU64::new(0));
    let (addr, _handle) = start_server_with_sparse_firmware(10_000, Arc::clone(&fired)).await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: SPARSE_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let mut first = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 1_000,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 5_000,
            })),
        }])))
        .await
        .expect("run bounded")
        .into_inner();

    let mut paused_at = None;
    while let Ok(Some(event)) = first.message().await {
        match event.payload {
            Some(run_event::Payload::Paused(paused)) => paused_at = Some(paused.ts),
            Some(run_event::Payload::End(_)) => panic!("bounded sparse run must pause, not end"),
            Some(run_event::Payload::Error(err)) => panic!("unexpected error: {}", err.message),
            _ => {}
        }
    }
    assert_eq!(paused_at, Some(5_000));
    assert_eq!(fired.load(Ordering::SeqCst), 0, "event must not fire yet");

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.state, "paused");
    assert_eq!(status.now_ticks, 5_000);

    let mut second = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 1_000,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 20_000,
            })),
        }])))
        .await
        .expect("run resume")
        .into_inner();

    let mut got_end = false;
    while let Ok(Some(event)) = second.message().await {
        if matches!(event.payload, Some(run_event::Payload::End(_))) {
            got_end = true;
        }
        if let Some(run_event::Payload::Error(err)) = event.payload {
            panic!("unexpected error on resume: {}", err.message);
        }
    }
    assert!(got_end, "resumed run must complete");
    assert_eq!(fired.load(Ordering::SeqCst), 1, "event must fire once");
}

// ── Atomic factory attachment ────────────────────────────────────────

static FACTORY_CALLS: AtomicU64 = AtomicU64::new(0);
static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);
/// Serializes marker-firmware tests that share process-wide counters without
/// holding a `MutexGuard` across `.await` (clippy `await_holding_lock`).
static MARKER_TEST_BUSY: AtomicBool = AtomicBool::new(false);

struct MarkerTestGuard;

impl MarkerTestGuard {
    fn acquire() -> Self {
        while MARKER_TEST_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            std::thread::yield_now();
        }
        Self
    }
}

impl Drop for MarkerTestGuard {
    fn drop(&mut self) {
        MARKER_TEST_BUSY.store(false, Ordering::Release);
    }
}

fn marker_test_guard() -> MarkerTestGuard {
    MarkerTestGuard::acquire()
}

/// Firmware that emits marker traces on init and after tick 5 so tests can
/// prove registered factories were attached and re-instantiated on Run.
struct MarkerFirmware {
    instance_id: u32,
    runtime_marker_emitted: bool,
}

impl Firmware for MarkerFirmware {
    fn init(&mut self, machine: &mut Machine) {
        machine.schedule_at(5, 0, "runtime_marker", Box::new(|_| {}));
        machine.record_trace(sim_core::TraceEvent::UserU32 {
            at: 0,
            label: "factory_marker",
            value: self.instance_id,
        });
    }

    fn step(&mut self, now: Tick, machine: &mut Machine) {
        if !self.runtime_marker_emitted && now >= 5 {
            self.runtime_marker_emitted = true;
            machine.record_trace(sim_core::TraceEvent::UserU32 {
                at: now,
                label: "runtime_marker",
                value: self.instance_id,
            });
        }
    }
}

fn marker_firmware_factory() -> Box<dyn Firmware> {
    FACTORY_CALLS.fetch_add(1, Ordering::SeqCst);
    let instance_id = NEXT_INSTANCE_ID.fetch_add(1, Ordering::SeqCst) as u32;
    Box::new(MarkerFirmware {
        instance_id,
        runtime_marker_emitted: false,
    })
}

const MARKER_SCENARIO: &str = r#"
name = "marker_fw"
[[machine]]
id = 0
name = "m0"
firmware = "marker_fw"
"#;

async fn start_server_with_marker_registry() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let mut registry = FirmwareRegistry::new();
    registry.register("marker_fw", Arc::new(marker_firmware_factory));
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

async fn collect_trace_lines(
    client: &mut SimulatorClient<tonic::transport::Channel>,
    session_id: u64,
    deadline_ticks: u64,
) -> Vec<String> {
    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: true,
                deadline_ticks,
            })),
        }])))
        .await
        .expect("run")
        .into_inner();

    let mut lines = Vec::new();
    while let Ok(Some(event)) = stream.message().await {
        if let Some(run_event::Payload::Trace(t)) = event.payload {
            lines.push(t.line);
        }
    }
    lines
}

#[tokio::test]
async fn run_sees_factories_attached_during_load() {
    let _guard = marker_test_guard();
    FACTORY_CALLS.store(0, Ordering::SeqCst);
    NEXT_INSTANCE_ID.store(1, Ordering::SeqCst);
    let (addr, _handle) = start_server_with_marker_registry().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MARKER_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    // Factories must be present immediately after LoadScenario returns Ready.
    // Prove via Run: ensure_firmware_loaded instantiates the factory and the
    // marker appears in the streamed human traces.
    let lines = collect_trace_lines(&mut client, sess.session_id, 0).await;
    assert!(
        lines.iter().any(|l| l.contains("factory_marker")),
        "registered firmware factory must be attached before Run"
    );
}

#[tokio::test]
async fn reset_preserves_registered_firmware() {
    let _guard = marker_test_guard();
    FACTORY_CALLS.store(0, Ordering::SeqCst);
    NEXT_INSTANCE_ID.store(1, Ordering::SeqCst);
    let (addr, _handle) = start_server_with_marker_registry().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MARKER_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let before_reset = FACTORY_CALLS.load(Ordering::SeqCst);
    let first_lines = collect_trace_lines(&mut client, sess.session_id, 10).await;
    assert!(
        first_lines.iter().any(|l| l.contains("factory_marker")),
        "first run must emit factory_marker"
    );
    assert_eq!(
        FACTORY_CALLS.load(Ordering::SeqCst),
        before_reset + 1,
        "first run must instantiate firmware"
    );

    client
        .reset_simulation(ResetSimulationRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("reset")
        .into_inner();

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.state, "ready");

    let second_lines = collect_trace_lines(&mut client, sess.session_id, 10).await;
    assert!(
        second_lines.iter().any(|l| l.contains("factory_marker")),
        "reset session must still run registered firmware"
    );
    assert!(
        FACTORY_CALLS.load(Ordering::SeqCst) > before_reset + 1,
        "reset must re-instantiate firmware via factory on next Run"
    );
}

#[tokio::test]
async fn clone_preserves_registered_firmware() {
    let _guard = marker_test_guard();
    FACTORY_CALLS.store(0, Ordering::SeqCst);
    NEXT_INSTANCE_ID.store(1, Ordering::SeqCst);
    let (addr, _handle) = start_server_with_marker_registry().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let source = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create source")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: source.session_id,
            scenario_toml: MARKER_SCENARIO.to_string(),
        })
        .await
        .expect("load source");

    let clone = client
        .clone_session(CloneSessionRequest {
            session_id: source.session_id,
        })
        .await
        .expect("clone")
        .into_inner();

    let source_lines = collect_trace_lines(&mut client, source.session_id, 10).await;
    let clone_lines = collect_trace_lines(&mut client, clone.new_session_id, 10).await;
    assert!(
        source_lines.iter().any(|l| l.contains("factory_marker")),
        "source session must emit factory_marker"
    );
    assert!(
        clone_lines.iter().any(|l| l.contains("factory_marker")),
        "cloned session must emit factory_marker"
    );

    let source_id = extract_marker_instance_id(&source_lines, "factory_marker");
    let clone_id = extract_marker_instance_id(&clone_lines, "factory_marker");
    assert_ne!(
        source_id, clone_id,
        "clone must instantiate independent firmware"
    );

    client
        .destroy_session(DestroySessionRequest {
            session_id: source.session_id,
        })
        .await
        .expect("destroy source");

    client
        .reset_simulation(ResetSimulationRequest {
            session_id: clone.new_session_id,
        })
        .await
        .expect("reset clone")
        .into_inner();

    let clone_after = collect_trace_lines(&mut client, clone.new_session_id, 10).await;
    assert!(
        clone_after.iter().any(|l| l.contains("factory_marker")),
        "destroying source must not break cloned session firmware"
    );
}

#[tokio::test]
async fn keyframe_restore_preserves_registered_firmware() {
    let _guard = marker_test_guard();
    FACTORY_CALLS.store(0, Ordering::SeqCst);
    NEXT_INSTANCE_ID.store(1, Ordering::SeqCst);
    let (addr, _handle) = start_server_with_marker_registry().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: MARKER_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let before_first_run = FACTORY_CALLS.load(Ordering::SeqCst);
    let _ = collect_trace_lines(&mut client, sess.session_id, 3).await;
    assert_eq!(
        FACTORY_CALLS.load(Ordering::SeqCst),
        before_first_run + 1,
        "initial run must instantiate firmware"
    );

    let kf = client
        .save_keyframe(SaveKeyframeRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("save keyframe")
        .into_inner();
    assert_eq!(kf.now_ticks, 3);

    let _ = collect_trace_lines(&mut client, sess.session_id, 10).await;

    let before_restore = FACTORY_CALLS.load(Ordering::SeqCst);
    client
        .load_keyframe(LoadKeyframeRequest {
            session_id: sess.session_id,
            keyframe_id: kf.keyframe_id,
        })
        .await
        .expect("load keyframe")
        .into_inner();

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.state, "paused");
    assert_eq!(status.now_ticks, 3);

    let restored_lines = collect_trace_lines(&mut client, sess.session_id, 10).await;
    assert!(
        FACTORY_CALLS.load(Ordering::SeqCst) > before_restore,
        "keyframe restore must create a fresh firmware instance on next Run"
    );
    assert!(
        restored_lines.iter().any(|l| l.contains("runtime_marker")),
        "restored session must resume past the keyframe tick with working firmware"
    );
}

fn extract_marker_instance_id(lines: &[String], label: &str) -> u32 {
    let marker = lines
        .iter()
        .find(|l| l.contains(label))
        .unwrap_or_else(|| panic!("missing {label} trace"));
    marker
        .rsplit('=')
        .next()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or_else(|| panic!("could not parse instance id from: {marker}"))
}

// ── R4: failed session returns World; sibling still runs ─────────────

/// Firmware that schedules work then panics on the first step.
struct PanickingFirmware;

impl Firmware for PanickingFirmware {
    fn init(&mut self, machine: &mut Machine) {
        // Ensure the run worker has an event to process (otherwise it
        // short-circuits to Done before calling drive_world/step).
        machine.schedule_at(0, 0, "panic_tick", Box::new(|_| {}));
    }

    fn step(&mut self, _now: Tick, _machine: &mut Machine) {
        panic!("deliberate test firmware panic");
    }
}

const PANIC_SCENARIO: &str = r#"
name = "panic_fw"
[[machine]]
id = 0
name = "m0"
firmware = "panic_fw"
"#;

async fn start_server_with_panic_firmware() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));

    let mut registry = FirmwareRegistry::new();
    registry.register(
        "panic_fw",
        Arc::new(|| Box::new(PanickingFirmware) as Box<dyn Firmware>),
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

#[tokio::test]
async fn failed_session_returns_world_and_sibling_runs() {
    let (addr, _handle) = start_server_with_panic_firmware().await;
    let mut client = SimulatorClient::connect(addr.clone())
        .await
        .expect("connect");

    // Session that will panic during run.
    let fail = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create fail")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: fail.session_id,
            scenario_toml: PANIC_SCENARIO.to_string(),
        })
        .await
        .expect("load panic scenario");

    // Sibling session with inert firmware-free scenario.
    let sib = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create sibling")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sib.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load sibling scenario");

    // Run the failing session — expect an Error event (or stream end with Error state).
    let mut fail_stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: fail.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run fail")
        .into_inner();

    let mut saw_error = false;
    while let Ok(Some(event)) = fail_stream.message().await {
        if let Some(run_event::Payload::Error(err)) = event.payload {
            assert!(
                err.message.contains("deliberate test firmware panic")
                    || err.message.contains("panic"),
                "unexpected error message: {}",
                err.message
            );
            saw_error = true;
        }
    }
    assert!(saw_error, "failed session run must emit SimulationError");

    // Failed session is Error and still inspectable (World returned).
    let fail_status = client
        .get_status(GetStatusRequest {
            session_id: fail.session_id,
        })
        .await
        .expect("fail status")
        .into_inner();
    assert_eq!(fail_status.state, "error");
    assert!(
        !fail_status.error_message.is_empty(),
        "error_message should be retained"
    );

    // Sibling completes independently and remains inspectable.
    let mut sib_client = SimulatorClient::connect(addr).await.expect("sib connect");
    let mut sib_stream = sib_client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sib.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run sibling")
        .into_inner();

    let mut sib_end = false;
    while let Ok(Some(event)) = sib_stream.message().await {
        match event.payload {
            Some(run_event::Payload::End(_)) => sib_end = true,
            Some(run_event::Payload::Error(err)) => {
                panic!("sibling must not error: {}", err.message);
            }
            _ => {}
        }
    }
    assert!(sib_end, "sibling should complete with SimulationEnd");

    let sib_status = client
        .get_status(GetStatusRequest {
            session_id: sib.session_id,
        })
        .await
        .expect("sib status")
        .into_inner();
    assert_eq!(sib_status.state, "done");

    // Failed session is still queryable after the sibling finished.
    let fail_again = client
        .get_status(GetStatusRequest {
            session_id: fail.session_id,
        })
        .await
        .expect("fail status again")
        .into_inner();
    assert_eq!(fail_again.state, "error");
}

// ── R4 WS4: factory panic during deferred firmware load ──────────────

const FACTORY_PANIC_SCENARIO: &str = r#"
name = "factory_panic_fw"
[[machine]]
id = 0
name = "m0"
firmware = "factory_panic_fw"
"#;

const TWO_MACHINE_FACTORY_PANIC_SCENARIO: &str = r#"
name = "two_machine_factory_panic"
[[machine]]
id = 0
name = "m0"
firmware = "marker_fw"
[[machine]]
id = 1
name = "m1"
firmware = "factory_panic_fw"
"#;

async fn start_server_with_factory_panic_registry() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));

    let mut registry = FirmwareRegistry::new();
    registry.register(
        "factory_panic_fw",
        Arc::new(|| panic!("deliberate factory panic")),
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

async fn start_server_with_marker_and_factory_panic() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));

    let mut registry = FirmwareRegistry::new();
    registry.register("marker_fw", Arc::new(marker_firmware_factory));
    registry.register(
        "factory_panic_fw",
        Arc::new(|| panic!("deliberate factory panic on machine 1")),
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

#[tokio::test]
async fn factory_panic_returns_world_as_error() {
    let (addr, _handle) = start_server_with_factory_panic_registry().await;
    let mut client = SimulatorClient::connect(addr.clone())
        .await
        .expect("connect");

    let fail = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create fail")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: fail.session_id,
            scenario_toml: FACTORY_PANIC_SCENARIO.to_string(),
        })
        .await
        .expect("load factory panic scenario");

    let sib = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create sibling")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sib.session_id,
            scenario_toml: MINIMAL_SCENARIO.to_string(),
        })
        .await
        .expect("load sibling scenario");

    let mut fail_stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: fail.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run fail")
        .into_inner();

    let mut saw_error = false;
    let mut error_message = String::new();
    while let Ok(Some(event)) = fail_stream.message().await {
        if let Some(run_event::Payload::Error(err)) = event.payload {
            error_message = err.message.clone();
            saw_error = true;
        }
    }
    assert!(saw_error, "factory panic run must emit SimulationError");
    assert!(
        error_message.contains("firmware factory panicked for machine 0")
            && error_message.contains("deliberate factory panic"),
        "unexpected error message: {error_message}"
    );

    let fail_status = client
        .get_status(GetStatusRequest {
            session_id: fail.session_id,
        })
        .await
        .expect("fail status")
        .into_inner();
    assert_eq!(fail_status.state, "error");
    assert!(
        fail_status
            .error_message
            .contains("firmware factory panicked for machine 0"),
        "status must retain factory panic message: {}",
        fail_status.error_message
    );

    let inspect = client
        .inspect_devices(InspectDevicesRequest {
            session_id: fail.session_id,
            machine_id: Some(0),
            device_type: String::new(),
            device_id: 0,
        })
        .await
        .expect("inspect after factory panic")
        .into_inner();
    assert!(
        inspect.devices.is_empty(),
        "inspect must succeed on Error session (empty device list is fine)"
    );

    client
        .reset_simulation(ResetSimulationRequest {
            session_id: fail.session_id,
        })
        .await
        .expect("reset after factory panic")
        .into_inner();

    let after_reset = client
        .get_status(GetStatusRequest {
            session_id: fail.session_id,
        })
        .await
        .expect("status after reset")
        .into_inner();
    assert_eq!(after_reset.state, "ready");

    let mut sib_client = SimulatorClient::connect(addr).await.expect("sib connect");
    let mut sib_stream = sib_client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sib.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run sibling")
        .into_inner();

    let mut sib_end = false;
    while let Ok(Some(event)) = sib_stream.message().await {
        match event.payload {
            Some(run_event::Payload::End(_)) => sib_end = true,
            Some(run_event::Payload::Error(err)) => {
                panic!("sibling must not error: {}", err.message);
            }
            _ => {}
        }
    }
    assert!(sib_end, "sibling should complete with SimulationEnd");

    let destroyed = client
        .destroy_session(DestroySessionRequest {
            session_id: fail.session_id,
        })
        .await
        .expect("destroy after error")
        .into_inner();
    assert!(destroyed.destroyed, "Error session must be destroyable");
}

#[tokio::test]
async fn second_machine_factory_panic() {
    let _guard = marker_test_guard();
    FACTORY_CALLS.store(0, Ordering::SeqCst);
    NEXT_INSTANCE_ID.store(1, Ordering::SeqCst);
    let (addr, _handle) = start_server_with_marker_and_factory_panic().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");

    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: TWO_MACHINE_FACTORY_PANIC_SCENARIO.to_string(),
        })
        .await
        .expect("load two-machine scenario");

    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 10,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 0,
            })),
        }])))
        .await
        .expect("run")
        .into_inner();

    let mut saw_error = false;
    let mut error_message = String::new();
    while let Ok(Some(event)) = stream.message().await {
        if let Some(run_event::Payload::Error(err)) = event.payload {
            error_message = err.message.clone();
            saw_error = true;
        }
    }
    assert!(
        saw_error,
        "second machine factory panic must emit SimulationError"
    );
    assert!(
        error_message.contains("firmware factory panicked for machine 1"),
        "error must name the panicking machine: {error_message}"
    );
    assert!(
        error_message.contains("deliberate factory panic on machine 1"),
        "error must include factory panic payload: {error_message}"
    );

    let status = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(
        status.state, "error",
        "session must not remain stuck in Running"
    );
    assert!(
        status.error_message.contains("machine 1"),
        "status error_message must name machine 1: {}",
        status.error_message
    );

    assert_eq!(
        FACTORY_CALLS.load(Ordering::SeqCst),
        1,
        "machine 0 marker factory must run before machine 1 factory panics"
    );

    client
        .reset_simulation(ResetSimulationRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("reset after partial factory load")
        .into_inner();

    let after_reset = client
        .get_status(GetStatusRequest {
            session_id: sess.session_id,
        })
        .await
        .expect("status after reset")
        .into_inner();
    assert_eq!(after_reset.state, "ready");
}

// ── Final bounded batch flush ────────────────────────────────────────

struct DeadlineTraceFirmware {
    marker_emitted: bool,
}

static DEADLINE_TRACE_MARKER_SENT: AtomicBool = AtomicBool::new(false);

impl DeadlineTraceFirmware {
    fn reset_test_state() {
        DEADLINE_TRACE_MARKER_SENT.store(false, Ordering::SeqCst);
    }
}

impl Firmware for DeadlineTraceFirmware {
    fn init(&mut self, machine: &mut Machine) {
        machine.schedule_at(100, 0, "deadline_marker", Box::new(|_| {}));
        machine.schedule_at(200, 0, "keep_alive", Box::new(|_| {}));
    }

    fn step(&mut self, now: Tick, machine: &mut Machine) {
        if now >= 100
            && !self.marker_emitted
            && !DEADLINE_TRACE_MARKER_SENT.swap(true, Ordering::SeqCst)
        {
            self.marker_emitted = true;
            machine.record_trace(sim_core::TraceEvent::UserU32 {
                at: now,
                label: "deadline_marker",
                value: 1,
            });
        }
    }
}

const DEADLINE_TRACE_SCENARIO: &str = r#"
name = "deadline_trace"
[[machine]]
id = 0
name = "m0"
firmware = "deadline_trace_fw"
"#;

async fn start_server_with_deadline_trace_firmware() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let mut registry = FirmwareRegistry::new();
    registry.register(
        "deadline_trace_fw",
        Arc::new(|| {
            Box::new(DeadlineTraceFirmware {
                marker_emitted: false,
            }) as Box<dyn Firmware>
        }),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedRunEvent {
    Trace,
    Tick(u64),
    Paused(u64),
    Display,
}

async fn collect_run_events_with_traces(
    client: &mut SimulatorClient<tonic::transport::Channel>,
    session_id: u64,
    tick_batch_size: u64,
    deadline_ticks: u64,
    stream_trace: bool,
    stream_display: bool,
) -> (Vec<ObservedRunEvent>, Vec<String>) {
    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(vec![RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id,
                tick_batch_size,
                stream_display,
                stream_trace,
                deadline_ticks,
            })),
        }])))
        .await
        .expect("run")
        .into_inner();

    let mut events = Vec::new();
    let mut trace_lines = Vec::new();
    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::Trace(t)) => {
                events.push(ObservedRunEvent::Trace);
                trace_lines.push(t.line);
            }
            Some(run_event::Payload::Tick(tick)) => events.push(ObservedRunEvent::Tick(tick.ts)),
            Some(run_event::Payload::Paused(paused)) => {
                events.push(ObservedRunEvent::Paused(paused.ts))
            }
            Some(run_event::Payload::Display(_)) => events.push(ObservedRunEvent::Display),
            _ => {}
        }
    }
    (events, trace_lines)
}

async fn collect_run_events(
    client: &mut SimulatorClient<tonic::transport::Channel>,
    session_id: u64,
    tick_batch_size: u64,
    deadline_ticks: u64,
    stream_trace: bool,
    stream_display: bool,
) -> Vec<ObservedRunEvent> {
    let (events, _) = collect_run_events_with_traces(
        client,
        session_id,
        tick_batch_size,
        deadline_ticks,
        stream_trace,
        stream_display,
    )
    .await;
    events
}

#[tokio::test]
async fn final_deadline_trace_precedes_pause() {
    DeadlineTraceFirmware::reset_test_state();
    let (addr, _handle) = start_server_with_deadline_trace_firmware().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: DEADLINE_TRACE_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let (events, trace_lines) =
        collect_run_events_with_traces(&mut client, sess.session_id, 25, 100, true, false).await;
    let marker_lines: Vec<_> = trace_lines
        .iter()
        .filter(|l| l.contains("user-u32") && l.contains("deadline_marker"))
        .collect();
    assert_eq!(
        marker_lines.len(),
        1,
        "deadline marker must appear exactly once"
    );
    let trace_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Trace))
        .expect("trace");
    let pause_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Paused(100)))
        .expect("paused at 100");
    assert!(
        trace_idx < pause_idx,
        "trace must precede Paused(100): {events:?}"
    );
}

#[tokio::test]
async fn final_deadline_tick_precedes_pause() {
    let (addr, _handle) = start_server_with_deadline_trace_firmware().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: DEADLINE_TRACE_SCENARIO.to_string(),
        })
        .await
        .expect("load");

    let events = collect_run_events(&mut client, sess.session_id, 25, 100, false, false).await;
    let tick_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Tick(100)))
        .expect("tick at 100");
    let pause_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Paused(100)))
        .expect("paused at 100");
    assert!(
        tick_idx < pause_idx,
        "Tick(100) must precede Paused(100): {events:?}"
    );
}

#[tokio::test]
async fn final_deadline_timer_fires_before_pause() {
    let (addr, _handle) = start_server_with_hold_firmware().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: TIMER_SCENARIO.to_string(),
        })
        .await
        .expect("load");
    client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
            machine_id: None,
            peripherals: vec![PeripheralDef {
                device: "timer".into(),
                id: 0,
                timer_irq: 16,
                ..Default::default()
            }],
        })
        .await
        .expect("configure timer");

    let messages = vec![
        RunRequest {
            payload: Some(run_request::Payload::Config(RunConfig {
                session_id: sess.session_id,
                tick_batch_size: 25,
                stream_display: false,
                stream_trace: false,
                deadline_ticks: 100,
            })),
        },
        RunRequest {
            payload: Some(run_request::Payload::TimerArm(TimerArm {
                machine_id: None,
                device_id: 0,
                delay_ticks: 100,
                period_ticks: 0,
            })),
        },
    ];
    let mut stream = client
        .run(tonic::Request::new(tokio_stream::iter(messages)))
        .await
        .expect("run")
        .into_inner();

    let mut events = Vec::new();
    while let Ok(Some(event)) = stream.message().await {
        match event.payload {
            Some(run_event::Payload::Tick(t)) => events.push(ObservedRunEvent::Tick(t.ts)),
            Some(run_event::Payload::Paused(p)) => events.push(ObservedRunEvent::Paused(p.ts)),
            _ => {}
        }
    }
    let tick_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Tick(100)))
        .expect("tick at deadline");
    let pause_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Paused(100)))
        .expect("paused at deadline");
    assert!(
        tick_idx < pause_idx,
        "timer tick must precede pause: {events:?}"
    );

    let resp = client
        .inspect_devices(InspectDevicesRequest {
            session_id: sess.session_id,
            device_type: "timer".into(),
            device_id: 0,
            ..Default::default()
        })
        .await
        .expect("inspect timer")
        .into_inner();
    assert_eq!(resp.devices[0].timer_fire_count, 1);
    assert_eq!(resp.devices[0].timer_last_fire_tick, 100);
}

struct DeadlineDisplayFirmware {
    filled: bool,
}

impl Firmware for DeadlineDisplayFirmware {
    fn init(&mut self, machine: &mut Machine) {
        machine.schedule_at(100, 0, "display_fill", Box::new(|_| {}));
        machine.schedule_at(200, 0, "keep_alive", Box::new(|_| {}));
    }

    fn step(&mut self, now: Tick, machine: &mut Machine) {
        if !self.filled && now >= 100 {
            self.filled = true;
            machine.with_device_context(|| {
                sim_devices::with_display_mut(0, |d| {
                    d.fill_rect(0, 0, 16, 16, 0xFF00_00FF);
                });
            });
        }
    }
}

const DEADLINE_DISPLAY_SCENARIO: &str = r#"
name = "deadline_display"
[[machine]]
id = 0
name = "m0"
firmware = "deadline_display_fw"
"#;

async fn start_server_with_deadline_display_firmware() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let mut registry = FirmwareRegistry::new();
    registry.register(
        "deadline_display_fw",
        Arc::new(|| Box::new(DeadlineDisplayFirmware { filled: false }) as Box<dyn Firmware>),
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

#[tokio::test]
async fn final_deadline_display_precedes_pause() {
    let (addr, _handle) = start_server_with_deadline_display_firmware().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: DEADLINE_DISPLAY_SCENARIO.to_string(),
        })
        .await
        .expect("load");
    client
        .configure_board(ConfigureBoardRequest {
            session_id: sess.session_id,
            machine_id: None,
            peripherals: vec![PeripheralDef {
                device: "display".into(),
                id: 0,
                display_width: 320,
                display_height: 240,
                color_mode: "rgb565".into(),
                ..Default::default()
            }],
        })
        .await
        .expect("configure display");

    let events = collect_run_events(&mut client, sess.session_id, 25, 100, false, true).await;
    let display_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Display))
        .expect("display frame");
    let pause_idx = events
        .iter()
        .position(|e| matches!(e, ObservedRunEvent::Paused(100)))
        .expect("paused at 100");
    assert!(
        display_idx < pause_idx,
        "DisplayFrame must precede Paused(100): {events:?}"
    );
}

#[tokio::test]
async fn bounded_event_after_deadline_does_not_fire() {
    struct AfterDeadlineFirmware {
        marker_emitted: bool,
    }
    impl Firmware for AfterDeadlineFirmware {
        fn init(&mut self, machine: &mut Machine) {
            machine.schedule_at(101, 0, "after_deadline", Box::new(|_| {}));
            machine.schedule_at(200, 0, "keep_alive", Box::new(|_| {}));
        }

        fn step(&mut self, now: Tick, machine: &mut Machine) {
            if !self.marker_emitted && now >= 101 {
                self.marker_emitted = true;
                machine.record_trace(sim_core::TraceEvent::UserU32 {
                    at: now,
                    label: "deadline_marker",
                    value: 1,
                });
            }
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));
    let mut registry = FirmwareRegistry::new();
    registry.register(
        "after_deadline_fw",
        Arc::new(|| {
            Box::new(AfterDeadlineFirmware {
                marker_emitted: false,
            }) as Box<dyn Firmware>
        }),
    );
    let service = SimulatorServiceImpl::new().with_firmware_registry(registry);
    let _handle = tokio::spawn(async move {
        Server::builder()
            .add_service(SimulatorServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .expect("server");
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id: sess.session_id,
            scenario_toml: r#"
name = "after_deadline"
[[machine]]
id = 0
name = "m0"
firmware = "after_deadline_fw"
"#
            .to_string(),
        })
        .await
        .expect("load");

    let first =
        collect_run_events_with_traces(&mut client, sess.session_id, 25, 100, true, false).await;
    let marker_lines: Vec<_> = first
        .1
        .iter()
        .filter(|l| l.contains("deadline_marker"))
        .collect();
    assert!(
        marker_lines.is_empty(),
        "event at 101 must not run before deadline 100"
    );
    assert!(
        first
            .0
            .iter()
            .any(|e| matches!(e, ObservedRunEvent::Paused(100))),
        "must pause at 100"
    );

    let second =
        collect_run_events_with_traces(&mut client, sess.session_id, 25, 250, true, false).await;
    let marker_lines: Vec<_> = second
        .1
        .iter()
        .filter(|l| l.contains("deadline_marker"))
        .collect();
    assert_eq!(
        marker_lines.len(),
        1,
        "resume must emit the deferred marker once"
    );
}

// ── Session revision guards ─────────────────────────────────────────

const SCENARIO_A: &str = r#"
name = "scenario_a"
[[machine]]
id = 0
name = "a0"
"#;

const SCENARIO_B: &str = r#"
name = "scenario_b"
[[machine]]
id = 0
name = "b0"
[[machine]]
id = 1
name = "b1"
"#;

#[tokio::test]
async fn reset_rejects_stale_publication() {
    let (addr, _handle, sessions) = start_server_with_shared_sessions().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    let session_id = sess.session_id;
    client
        .load_scenario(LoadScenarioRequest {
            session_id,
            scenario_toml: SCENARIO_A.to_string(),
        })
        .await
        .expect("load a");

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let sessions_reset = Arc::clone(&sessions);
    let reset_handle = tokio::task::spawn_blocking(move || {
        sessions_reset.reset_with(session_id, |_, _| {
            ready_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            Ok(())
        })
    });

    ready_rx.recv().unwrap();

    client
        .load_scenario(LoadScenarioRequest {
            session_id,
            scenario_toml: SCENARIO_B.to_string(),
        })
        .await
        .expect("load b while reset blocked");
    go_tx.send(()).unwrap();

    let reset_result = reset_handle.await.expect("reset join");
    assert!(
        reset_result
            .unwrap_err()
            .contains("session changed while operation was preparing"),
        "reset must reject stale publication"
    );

    let status = client
        .get_status(GetStatusRequest { session_id })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.state, "ready");
    assert_eq!(
        status.n_machines, 2,
        "session metadata must remain scenario B"
    );
}

#[tokio::test]
async fn keyframe_restore_rejects_stale_publication() {
    let (addr, _handle, sessions) = start_server_with_shared_sessions().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    let session_id = sess.session_id;
    client
        .load_scenario(LoadScenarioRequest {
            session_id,
            scenario_toml: SCENARIO_A.to_string(),
        })
        .await
        .expect("load a");
    let kf = client
        .save_keyframe(SaveKeyframeRequest { session_id })
        .await
        .expect("save keyframe")
        .into_inner();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let sessions_restore = Arc::clone(&sessions);
    let kf_id = kf.keyframe_id;
    let restore_handle = tokio::task::spawn_blocking(move || {
        sessions_restore.load_keyframe_with(session_id, kf_id, |_, _| {
            ready_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            Ok(())
        })
    });

    ready_rx.recv().unwrap();
    client
        .load_scenario(LoadScenarioRequest {
            session_id,
            scenario_toml: SCENARIO_B.to_string(),
        })
        .await
        .expect("load b while restore blocked");
    go_tx.send(()).unwrap();

    let restore_result = restore_handle.await.expect("restore join");
    let restore_err = restore_result.expect_err("restore must fail");
    assert!(
        restore_err.contains("session changed while operation was preparing")
            || restore_err.contains("keyframe belongs to an older scenario revision"),
        "restore must reject stale publication: {restore_err}"
    );

    let status = client
        .get_status(GetStatusRequest { session_id })
        .await
        .expect("status")
        .into_inner();
    assert_eq!(status.n_machines, 2, "session must remain coherently B");
}

#[tokio::test]
async fn old_keyframe_invalid_after_scenario_reload() {
    let (addr, _handle, _sessions) = start_server_with_shared_sessions().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    let session_id = sess.session_id;
    client
        .load_scenario(LoadScenarioRequest {
            session_id,
            scenario_toml: SCENARIO_A.to_string(),
        })
        .await
        .expect("load a");
    let kf = client
        .save_keyframe(SaveKeyframeRequest { session_id })
        .await
        .expect("save keyframe")
        .into_inner();
    client
        .load_scenario(LoadScenarioRequest {
            session_id,
            scenario_toml: SCENARIO_B.to_string(),
        })
        .await
        .expect("load b");

    let err = client
        .load_keyframe(LoadKeyframeRequest {
            session_id,
            keyframe_id: kf.keyframe_id,
        })
        .await
        .expect_err("stale keyframe must fail");
    assert!(
        err.code() == tonic::Code::NotFound || err.code() == tonic::Code::FailedPrecondition,
        "unexpected status: {}",
        err
    );
}

#[tokio::test]
async fn successful_keyframe_restore_publishes_coherent_metadata() {
    let _guard = marker_test_guard();
    FACTORY_CALLS.store(0, Ordering::SeqCst);
    NEXT_INSTANCE_ID.store(1, Ordering::SeqCst);
    let (addr, _handle) = start_server_with_marker_registry().await;
    let mut client = SimulatorClient::connect(addr).await.expect("connect");
    let sess = client
        .create_session(CreateSessionRequest {})
        .await
        .expect("create")
        .into_inner();
    let session_id = sess.session_id;
    client
        .load_scenario(LoadScenarioRequest {
            session_id,
            scenario_toml: MARKER_SCENARIO.to_string(),
        })
        .await
        .expect("load marker scenario");
    let kf = client
        .save_keyframe(SaveKeyframeRequest { session_id })
        .await
        .expect("save keyframe")
        .into_inner();
    assert_eq!(kf.now_ticks, 0);

    let _ = collect_trace_lines(&mut client, session_id, 10).await;

    client
        .load_keyframe(LoadKeyframeRequest {
            session_id,
            keyframe_id: kf.keyframe_id,
        })
        .await
        .expect("restore keyframe")
        .into_inner();

    let status = client
        .get_status(GetStatusRequest { session_id })
        .await
        .expect("status after restore")
        .into_inner();
    assert_eq!(status.now_ticks, 0);
    assert_eq!(status.n_machines, 1);

    client
        .reset_simulation(ResetSimulationRequest { session_id })
        .await
        .expect("reset after restore")
        .into_inner();

    let lines = collect_trace_lines(&mut client, session_id, 10).await;
    assert!(
        lines.iter().any(|l| l.contains("factory_marker")),
        "reset after keyframe restore must rebuild scenario A firmware"
    );
}
