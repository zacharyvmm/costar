# costar Requirements

Documenting design decisions and feature requirements for the costar simulation
engine and its integrations (multi-machine World, bus topology, plant models,
scenario DSL).

## Phase 4: Plant Model Integration (Completed)

### EnvironmentModel trait

Defined in `crates/sim-world/src/plant.rs`:

- `step(&mut self, now: Tick, world: &mut World)` — advance the environment model
  by one tick. Receives mutable access to the World for reading actuator commands
  from buses and publishing sensor readings.
- `queue_driver_input(&mut self, at: Tick, throttle_percent: u8, brake_pressed: bool)`
  — queue a timed driver input for the plant to apply.

### Plant support in World

`crates/sim-world/src/world.rs`:

- `set_plant(plant, tick_interval_ms)` — attach a boxed EnvironmentModel.  The
  `tick_interval_ms` is converted to µs ticks (×1000) matching the bus timing
  convention.
- `queue_plant_input(at, throttle, brake)` — delegate to the plant's
  `queue_driver_input`.
- `step_plant(now)` — called each event-loop iteration; steps the plant once per
  elapsed tick interval.  Handles time-jumps past multiple intervals (steps the
  plant for each one).
- `next_global_event_time()` — includes `next_plant_tick` in the set of
  deadlines considered for lockstep advancement.

Stop conditions (`run` / `run_until`):

- `run()`: runs until all machines are idle, links/buses empty, AND no plant
  is attached (`all_idle() && plant.is_none()`).  A plant-only world must
  use `run_until()` with a deadline.
- `run_until(deadline)`: runs until the deadline or until all machines idle
  with no plant.  If a plant is attached, plant ticks keep the simulation
  alive until the deadline.

### Scenario DSL extensions

`crates/sim-world/src/scenario.rs`:

- `[plant]` section — `type` (e.g. "microcar") and optional `tick_ms` (default
  10ms).
- `[[input]]` table — `at_ms`, `type` ("driver_input"), `throttle_percent`, and
  `brake_pressed`.
- `attach_plant_to(world, plant)` — queues all `[[input]]` entries as timed
  driver inputs, then attaches the plant with the configured tick interval.
- `duration_ms` field — used by `run_scenario` to call `world.run_until()`
  instead of `world.run()`, allowing plant-tick-driven simulations to run for a
  fixed duration.

### Microcar plant (`microcar-plant` crate)

`/home/zmm/projects/microcar/plant/src/model.rs`:

- `MicrocarPlant` implements `EnvironmentModel`.
- Contains `VehiclePlant` (speed/drag/torque), `BatteryModel` (SOC/temperature/
  voltage/current), and `SensorReadings`.
- Reads driver inputs from the queued scenario-injected inputs (maps throttle %
  1:1 to motor torque % for MVP).
- Publishes:
  - `CAN_ID_WHEEL_SPEED` (0x200) — u16 BE, speed in 0.1 km/h
  - `CAN_ID_BMS_STATUS` (0x300) — 7 bytes: SOC%, voltage (u16), temperature
    (i16), current (i16)
- `CAN_ID_MOTOR_COMMAND` (0x100) defined but MVP reads motor torque from
  driver input queue (firmware ECUs not running yet).

### Integration with scenario runner

`crates/sim-runner/src/main.rs`:

- `run_scenario()` and `run_scenario_test()` both:
  1. Call `scenario.build_world()` to create machines, links, and buses.
  2. If `[plant]` section present with `type = "microcar"`, create
     `microcar_plant::MicrocarPlant` and call `scenario.attach_plant_to()`.
  3. Use `world.run_until(duration_ms * 1000)` when `duration_ms` is set,
     otherwise `world.run()`.
  4. Compare trace output against `[expect].trace` golden file.

### Testing

- `test_world_plant_tick_scheduling` — verifies plant steps the correct number
  of times when time jumps past multiple intervals.
- `test_world_plant_next_event_includes_plant_tick` — verifies plant tick
  appears in `next_global_event_time()` and `run_until()` works for plant-only
  worlds.
- `test_world_plant_with_idle_machines` — verifies plant steps alongside idle
  machines.
- 7 `MicrocarPlant` tests in `model.rs` — speed change, BMS publishing, brake
  override, multiple input scheduling.
- `normal_drive_cycle.toml` scenario test — 3000ms, 4 machines on vcan0, plant
  publishes 2392 trace events (299 plant ticks × 4 machines × 2 frames).
