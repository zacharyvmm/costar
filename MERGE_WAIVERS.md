# Merge Waivers — costar#5

This PR is recommended for merge with the following explicit waivers.

## 1. `dogfood/b3_gateway_reboot_downtime.toml` harness failure (microcar side)

The companion microcar scenario `dogfood/b3_gateway_reboot_downtime.toml` fails the
harness with a `sim-fiber` / FreeRTOS coroutine leak:

```
Fiber::drop id=1 state=Created | Fiber::drop leaking coroutine id=1
microcar: error [check]: trace mismatch
```

- Reproduced at the pre-cleanup commit `949c0fe` in the `microcar` repo — the failure
  is not caused by the final comment/fmt cleanup.
- Root cause is in `sim-fiber` / FreeRTOS port integration, not in the TOML scenario.
- This is waived for Stage A foundation merge.

## 2. Full workspace clippy (`-D warnings`) not clean

```
cargo clippy --workspace --all-targets -- -D warnings
```

Fails with pre-existing issues in untouched crates:
- `sim-net`: `manual_c_str_literals` clippy lint (Rust 1.97)
- `sim-grpc`: requires `PROTOC` env var for build

The crates changed in this PR (`sim-world`, `sim-runner`) pass clippy cleanly.

## 3. `sim.stop` semantics

`handle_sim_stop` sets the session to `Ready` while the underlying world is
`WorldRunState::Stopped`. A TODO documents the three resolution options
(terminal stop, rebuild-from-scenario, separate `sim.restart` method).
This is deferred to a post-Stage-A follow-up.

## 4. Scoped-out for follow-up

- NetworkBank / Ethernet isolation: Stage B3 (NetworkBank infrastructure exists
  in `sim-net/src/bank.rs` but is not yet wired into `SimulatorExecutionContext`;
  the TODO in `sim-ffi/src/simulator.rs` documents this explicitly)
- Telematics: current `dogfood/src/telematics.rs` is a trace-based smoke test
  only, not the full Stage H host-TCP-bridge lane from the dogfood plan
- gRPC-specific `failed_session_returns_world_and_sibling_runs`: follow-up
- gRPC registry firmware reboot test: follow-up
- JSON-RPC device-ID-0 isolation test: follow-up (TODO in `jsonrpc_two_sessions_run_independently`)
