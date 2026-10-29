#include "costar/session.hpp"

#include <algorithm>
#include <exception>
#include <memory>
#include <stdexcept>

#include "costar/world.hpp"

namespace costar {

namespace {

// How far a running session simulates before it looks at its commands.
constexpr Time kSlice = milliseconds(1);

const char* cpu_state_name(Cpu::State state) {
  switch (state) {
    case Cpu::State::Awake: return "awake";
    case Cpu::State::Halted: return "halted";
    case Cpu::State::Busy: return "busy";
    case Cpu::State::Exited: return "off";
  }
  return "?";
}

// Fail early, on the caller's request, rather than inside the simulation.
void check_input(World& world, const Input& input) {
  if (input.target.machine >= world.machine_count()) throw std::invalid_argument("no such machine");
  Devices& devices = world.machine(input.target.machine).devices();
  const std::string& device = input.target.device;
  switch (input.kind) {
    case Input::Kind::Deliver:
    case Input::Kind::Send: devices.get<Channel>(device); break;
    case Input::Kind::SetRegister: devices.get<Registers>(device); break;
    case Input::Kind::WriteMemory:
      if (input.offset + input.data.size() > devices.get<Memory>(device).bytes.size()) {
        throw std::invalid_argument("write past the end of memory device " + device);
      }
      break;
  }
}

}  // namespace

// Everything below is owned by the session's thread.
// An input, and the point in the run it was submitted at: the number of
// world steps taken by then.  Time alone is not enough: steps at one instant
// can come before and after a submission.
struct RecordedInput {
  Input input;
  uint64_t submitted_after_steps;
};

struct Session::State {
  std::unique_ptr<World> world;
  std::vector<RecordedInput> inputs;  // in submission order
  bool running = false;
  Time target = 0;
  std::string error;  // set if the simulation failed
};

Session::Session(Scenario scenario, PlantFactories plants)
    : scenario_(std::move(scenario)), plants_(std::move(plants)) {
  std::promise<void> built;
  std::future<void> ready = built.get_future();
  thread_ = std::thread(&Session::loop, this, std::move(built));
  try {
    ready.get();
  } catch (...) {
    thread_.join();
    throw;
  }
}

Session::~Session() { close(); }

void Session::close() {
  {
    std::lock_guard lock(mutex_);
    closing_ = true;
  }
  commands_ready_.notify_all();
  trace_grew_.notify_all();
  std::lock_guard join_lock(join_mutex_);
  if (thread_.joinable()) thread_.join();
}

// ── Commands ──────────────────────────────────────────────────────────

template <class F>
auto Session::call(F f) -> decltype(f(std::declval<State&>())) {
  using Result = decltype(f(std::declval<State&>()));
  std::packaged_task<Result(State&)> task(std::move(f));
  std::future<Result> result = task.get_future();
  {
    std::lock_guard lock(mutex_);
    if (closing_) throw std::runtime_error("session is closing");
    commands_.push_back([&task](State& state) { task(state); });
  }
  commands_ready_.notify_one();
  return result.get();
}

SessionStatus Session::run(Time until) {
  return call([&](State& s) {
    s.running = until > s.world->now() && s.error.empty();
    s.target = until;
    return status_of(s);
  });
}

SessionStatus Session::pause() {
  return call([&](State& s) {
    s.running = false;
    return status_of(s);
  });
}

SessionStatus Session::input(Input input) {
  return call([&](State& s) {
    check_input(*s.world, input);
    input.at = std::max(input.at, s.world->now());
    s.inputs.push_back({input, s.world->steps()});
    schedule_input(*s.world, input);
    return status_of(s);
  });
}

SessionStatus Session::seek(Time time) {
  return call([&](State& s) {
    // Build the replacement fully before touching the session's state, so a
    // failed seek leaves the world and its history as they were.
    std::vector<RecordedInput> inputs = s.inputs;
    std::erase_if(inputs, [time](const RecordedInput& r) { return r.input.at > time; });
    auto world = std::make_unique<World>();
    build_world(scenario_, *world, plants_);
    for (const RecordedInput& r : inputs) {
      world->run_steps(r.submitted_after_steps);
      schedule_input(*world, r.input);
    }
    world->run_until(time);
    s.world = std::move(world);
    s.inputs = std::move(inputs);
    s.running = false;
    s.error.clear();
    {
      std::lock_guard lock(mutex_);
      published_.clear();
      ++rewinds_;
    }
    return status_of(s);
  });
}

SessionStatus Session::status() {
  return call([&](State& s) { return status_of(s); });
}

Snapshot Session::inspect() {
  return call([&](State& s) {
    Snapshot snapshot{.now = s.world->now(), .machines = {}};
    for (uint32_t id = 0; id < s.world->machine_count(); ++id) {
      Machine& machine = s.world->machine(id);
      MachineSnapshot m{.id = id,
                        .name = scenario_.machines.at(id).name,
                        .cpu_state = cpu_state_name(machine.cpu().state()),
                        .devices = {}};
      Devices& devices = machine.devices();
      for (int handle = 0; handle < static_cast<int>(devices.size()); ++handle) {
        const Device& device = *devices.at(handle);
        DeviceSnapshot d{.name = device.name,
                         .kind = std::holds_alternative<Channel>(device.state)     ? DeviceKind::Channel
                                 : std::holds_alternative<Registers>(device.state) ? DeviceKind::Registers
                                                                                   : DeviceKind::Memory,
                         .irq = device.irq == kNoIrq ? -1 : static_cast<int>(device.irq)};
        if (const auto* channel = std::get_if<Channel>(&device.state)) d.queued_frames = channel->received.size();
        if (const auto* registers = std::get_if<Registers>(&device.state)) d.registers = registers->values;
        if (const auto* memory = std::get_if<Memory>(&device.state)) d.memory = memory->bytes;
        m.devices.push_back(std::move(d));
      }
      snapshot.machines.push_back(std::move(m));
    }
    return snapshot;
  });
}

std::optional<std::vector<TraceRecord>> Session::wait_trace(TraceCursor& cursor, std::chrono::milliseconds timeout) {
  std::unique_lock lock(mutex_);
  if (!cursor.rewinds) cursor.rewinds = rewinds_;
  trace_grew_.wait_for(lock, timeout, [&] {
    return closing_ || rewinds_ != *cursor.rewinds || published_.size() > cursor.next;
  });
  if (closing_ || rewinds_ != *cursor.rewinds) return std::nullopt;
  std::vector<TraceRecord> records(published_.begin() + std::min(cursor.next, published_.size()), published_.end());
  cursor.next = std::max(cursor.next, published_.size());
  return records;
}

// ── The session's thread ──────────────────────────────────────────────

void Session::loop(std::promise<void> built) {
  State state;
  try {
    state.world = std::make_unique<World>();
    build_world(scenario_, *state.world, plants_);
  } catch (...) {
    built.set_exception(std::current_exception());
    return;
  }
  built.set_value();

  for (;;) {
    std::vector<Command> commands;
    bool closing;
    {
      std::unique_lock lock(mutex_);
      commands_ready_.wait(lock, [&] { return closing_ || !commands_.empty() || state.running; });
      commands.swap(commands_);
      closing = closing_;
    }
    for (Command& command : commands) command(state);  // answers callers even when closing
    if (closing) return;

    if (state.running) {
      try {
        World& world = *state.world;
        if (world.quiescent()) {
          // Nothing will ever happen again: jump to the target, or stop.
          if (state.target != kNever) world.run_until(state.target);
          state.running = false;
        } else if (state.target == kNever) {
          // Unbounded: leave time at the last event, as an uninterrupted
          // World::run() would, instead of at a slice boundary.
          world.run_events_until(std::max(*world.next_event_time(), time_add(world.now(), kSlice)));
        } else {
          world.run_until(std::min(state.target, world.now() + kSlice));
          state.running = world.now() < state.target;
        }
      } catch (const std::exception& error) {
        state.error = error.what();
        state.running = false;
      }
    }
    publish(state);
  }
}

void Session::publish(const State& state) {
  const std::vector<TraceRecord>& records = state.world->trace().records();
  {
    std::lock_guard lock(mutex_);
    if (records.size() == published_.size()) return;
    published_.insert(published_.end(), records.begin() + published_.size(), records.end());
  }
  trace_grew_.notify_all();
}

SessionStatus Session::status_of(const State& state) const {
  return {.now = state.world->now(),
          .running = state.running,
          .run_until = state.target,
          .trace_length = state.world->trace().records().size(),
          .error = state.error};
}

}  // namespace costar
