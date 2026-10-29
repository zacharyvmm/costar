#include "costar_grpc/server.hpp"

#include <chrono>
#include <cstdint>
#include <iostream>
#include <map>
#include <mutex>
#include <stdexcept>
#include <vector>

#include "costar/session.hpp"

namespace costar::grpc_server {

namespace {

using grpc::ServerContext;
using grpc::StatusCode;

// ── Messages ──────────────────────────────────────────────────────────

void to_proto(uint64_t id, const Session& session, const SessionStatus& status, v1::SessionStatus* out) {
  out->set_session(id);
  out->set_name(session.name());
  out->set_now_ns(status.now);
  out->set_running(status.running);
  out->set_run_until_ns(status.run_until);
  out->set_trace_length(status.trace_length);
  out->set_error(status.error);
  out->set_realtime(status.realtime);
}

void to_proto(const Snapshot& snapshot, v1::Snapshot* out) {
  out->set_now_ns(snapshot.now);
  for (const MachineSnapshot& machine : snapshot.machines) {
    v1::MachineState* m = out->add_machines();
    m->set_id(machine.id);
    m->set_name(machine.name);
    m->set_cpu_state(machine.cpu_state);
    for (const DeviceSnapshot& device : machine.devices) {
      v1::DeviceState* d = m->add_devices();
      d->set_name(device.name);
      d->set_kind(static_cast<v1::DeviceState::Kind>(device.kind));
      d->set_irq(device.irq);
      d->set_queued_frames(static_cast<uint32_t>(device.queued_frames));
      for (const auto& [reg, value] : device.registers) (*d->mutable_registers())[reg] = value;
      d->set_memory(std::string(device.memory.begin(), device.memory.end()));
    }
  }
}

void to_proto(uint64_t index, const TraceRecord& record, v1::TraceRecord* out) {
  out->set_index(index);
  out->set_at_ns(record.at);
  out->set_machine(record.machine);
  out->set_label(record.label);
  out->set_value(record.value);
  if (record.text) out->set_text(*record.text);
}

Frame bytes(const std::string& data) { return Frame(data.begin(), data.end()); }

Input from_proto(const v1::InputRequest& request, const Scenario& scenario) {
  const std::string& target = request.target();
  if (request.action_case() == v1::InputRequest::kPower) {
    if (target.find('.') != std::string::npos) throw std::invalid_argument("a power input's target is a machine");
    Input input{.kind = Input::Kind::Power, .at = request.at_ns(), .target = {scenario.machine_id(target), ""}};
    switch (request.power()) {
      case v1::POWER_RESET: input.power = Input::Power::Reset; break;
      case v1::POWER_OFF: input.power = Input::Power::Off; break;
      case v1::POWER_ON: input.power = Input::Power::On; break;
      default: throw std::invalid_argument("unknown power action " + std::to_string(request.power()));
    }
    return input;
  }
  const size_t dot = target.find('.');
  if (dot == std::string::npos) throw std::invalid_argument("target should be machine.device");
  Input input{.kind = Input::Kind::Deliver,
              .at = request.at_ns(),
              .target = {scenario.machine_id(target.substr(0, dot)), target.substr(dot + 1)}};
  switch (request.action_case()) {
    case v1::InputRequest::kDeliver: input.data = bytes(request.deliver()); break;
    case v1::InputRequest::kSend:
      input.kind = Input::Kind::Send;
      input.data = bytes(request.send());
      break;
    case v1::InputRequest::kSetRegister:
      input.kind = Input::Kind::SetRegister;
      input.reg = request.set_register().reg();
      input.value = request.set_register().value();
      break;
    case v1::InputRequest::kWriteMemory:
      input.kind = Input::Kind::WriteMemory;
      input.offset = request.write_memory().offset();
      input.data = bytes(request.write_memory().data());
      break;
    case v1::InputRequest::kPower: break;  // handled above
    case v1::InputRequest::ACTION_NOT_SET: throw std::invalid_argument("an input needs an action");
  }
  return input;
}

// ── Service ───────────────────────────────────────────────────────────

class Service final : public v1::Costar::Service {
 public:
  explicit Service(PlantFactories plants) : plants_(std::move(plants)) {}

  grpc::Status CreateSession(ServerContext*, const v1::CreateSessionRequest* request, v1::SessionStatus* reply) override {
    return guarded([&] {
      std::vector<std::filesystem::path> image_dirs(request->image_dirs().begin(), request->image_dirs().end());
      const std::string path = request->scenario_path().empty() ? "scenario.toml" : request->scenario_path();
      auto session = std::make_shared<Session>(parse_scenario(request->scenario_toml(), path, image_dirs), plants_);
      uint64_t id;
      {
        std::lock_guard lock(mutex_);
        id = next_id_++;
        sessions_[id] = session;
      }
      to_proto(id, *session, session->status(), reply);
    });
  }

  // Closes the session at once, even while Watch calls still hold it, and
  // cancels those calls (a Write blocked on a slow client included).
  grpc::Status DestroySession(ServerContext*, const v1::SessionRef* request, v1::Empty*) override {
    std::shared_ptr<Session> session;
    {
      std::lock_guard lock(mutex_);
      const auto it = sessions_.find(request->session());
      if (it == sessions_.end()) return not_found(request->session());
      session = std::move(it->second);
      sessions_.erase(it);
      const auto [first, last] = watchers_.equal_range(request->session());
      for (auto watcher = first; watcher != last; ++watcher) watcher->second->TryCancel();
    }
    session->close();
    return grpc::Status::OK;
  }

  grpc::Status ListSessions(ServerContext*, const v1::Empty*, v1::SessionList* reply) override {
    std::map<uint64_t, std::shared_ptr<Session>> sessions;
    {
      std::lock_guard lock(mutex_);
      sessions = sessions_;
    }
    return guarded([&] {
      for (const auto& [id, session] : sessions) to_proto(id, *session, session->status(), reply->add_sessions());
    });
  }

  grpc::Status Run(ServerContext*, const v1::RunRequest* request, v1::SessionStatus* reply) override {
    return on_session(request->session(), reply, [&](Session& s) {
      return s.run(request->has_until_ns() ? request->until_ns() : kNever, request->realtime());
    });
  }

  grpc::Status Pause(ServerContext*, const v1::SessionRef* request, v1::SessionStatus* reply) override {
    return on_session(request->session(), reply, [](Session& s) { return s.pause(); });
  }

  grpc::Status Status(ServerContext*, const v1::SessionRef* request, v1::SessionStatus* reply) override {
    return on_session(request->session(), reply, [](Session& s) { return s.status(); });
  }

  grpc::Status Input(ServerContext*, const v1::InputRequest* request, v1::SessionStatus* reply) override {
    return on_session(request->session(), reply, [&](Session& s) { return s.input(from_proto(*request, s.scenario())); });
  }

  grpc::Status Seek(ServerContext*, const v1::SeekRequest* request, v1::SessionStatus* reply) override {
    return on_session(request->session(), reply, [&](Session& s) { return s.seek(request->time_ns()); });
  }

  grpc::Status Inspect(ServerContext*, const v1::SessionRef* request, v1::Snapshot* reply) override {
    const std::shared_ptr<Session> session = find(request->session());
    if (!session) return not_found(request->session());
    return guarded([&] { to_proto(session->inspect(), reply); });
  }

  grpc::Status Watch(ServerContext* context, const v1::WatchRequest* request,
                     grpc::ServerWriter<v1::TraceRecord>* writer) override {
    // Look the session up and register in one step, so DestroySession either
    // sees this watcher or this watcher doesn't see the session.
    const WatcherRegistration registration(*this, request->session(), context);
    const std::shared_ptr<Session>& session = registration.session();
    if (!session) return not_found(request->session());

    Session::TraceCursor cursor{.next = request->from_index()};
    while (!context->IsCancelled()) {
      const size_t first = cursor.next;
      const auto records = session->wait_trace(cursor, std::chrono::milliseconds(100));
      if (!records) {
        if (!find(request->session())) return grpc::Status::OK;  // the session was destroyed
        return {StatusCode::ABORTED, "a seek rewound the trace; watch again from 0"};
      }
      for (size_t i = 0; i < records->size(); ++i) {
        v1::TraceRecord record;
        to_proto(first + i, (*records)[i], &record);
        if (!writer->Write(record)) return grpc::Status::OK;  // the client went away
      }
      if (!find(request->session())) return grpc::Status::OK;
    }
    return {StatusCode::CANCELLED, "watch cancelled"};
  }

 private:
  // A Watch call, known to DestroySession while it runs.
  class WatcherRegistration {
   public:
    WatcherRegistration(Service& service, uint64_t id, ServerContext* context) : service_(service) {
      std::lock_guard lock(service_.mutex_);
      const auto it = service_.sessions_.find(id);
      if (it == service_.sessions_.end()) return;
      session_ = it->second;
      entry_ = service_.watchers_.emplace(id, context);
    }
    ~WatcherRegistration() {
      std::lock_guard lock(service_.mutex_);
      if (session_) service_.watchers_.erase(entry_);
    }
    const std::shared_ptr<Session>& session() const { return session_; }
    WatcherRegistration(const WatcherRegistration&) = delete;
    WatcherRegistration& operator=(const WatcherRegistration&) = delete;

   private:
    Service& service_;
    std::shared_ptr<Session> session_;  // null: no such session
    std::multimap<uint64_t, ServerContext*>::iterator entry_;
  };

  std::shared_ptr<Session> find(uint64_t id) {
    std::lock_guard lock(mutex_);
    const auto it = sessions_.find(id);
    return it == sessions_.end() ? nullptr : it->second;
  }

  static grpc::Status not_found(uint64_t id) {
    return {StatusCode::NOT_FOUND, "no session " + std::to_string(id)};
  }

  // Run `f`, turning exceptions into gRPC errors.
  template <class F>
  static grpc::Status guarded(F f) {
    try {
      f();
      return grpc::Status::OK;
    } catch (const std::invalid_argument& error) {
      return {StatusCode::INVALID_ARGUMENT, error.what()};
    } catch (const std::out_of_range& error) {
      return {StatusCode::INVALID_ARGUMENT, error.what()};
    } catch (const std::exception& error) {
      return {StatusCode::FAILED_PRECONDITION, error.what()};
    }
  }

  template <class F>
  grpc::Status on_session(uint64_t id, v1::SessionStatus* reply, F f) {
    const std::shared_ptr<Session> session = find(id);
    if (!session) return not_found(id);
    return guarded([&] { to_proto(id, *session, f(*session), reply); });
  }

  const PlantFactories plants_;
  std::mutex mutex_;  // guards the registry; sessions are actors
  std::map<uint64_t, std::shared_ptr<Session>> sessions_;
  std::multimap<uint64_t, ServerContext*> watchers_;
  uint64_t next_id_ = 1;
};

}  // namespace

std::unique_ptr<v1::Costar::Service> make_service(PlantFactories plants) {
  return std::make_unique<Service>(std::move(plants));
}

int serve(const std::string& address, PlantFactories plants) {
  const std::unique_ptr<v1::Costar::Service> service = make_service(std::move(plants));
  grpc::ServerBuilder builder;
  builder.AddListeningPort(address, grpc::InsecureServerCredentials());
  builder.RegisterService(service.get());
  const std::unique_ptr<grpc::Server> server = builder.BuildAndStart();
  if (!server) {
    std::cerr << "costar-grpc: cannot listen on " << address << '\n';
    return 1;
  }
  std::cerr << "costar-grpc: listening on " << address << '\n';
  server->Wait();
  return 0;
}

}  // namespace costar::grpc_server
