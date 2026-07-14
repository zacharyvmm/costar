//! Integration tests for the costar gRPC server.
//!
//! Tests exercise the full gRPC stack: session management, scenario
//! loading, board configuration, device inspection, keyframes, and
//! the bidirectional Run stream.

use sim_grpc::proto::simulator_client::SimulatorClient;
use sim_grpc::proto::simulator_server::SimulatorServer;
use sim_grpc::proto::*;
use sim_grpc::server::SimulatorServiceImpl;
use tokio_stream::wrappers::TcpListenerStream;
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
