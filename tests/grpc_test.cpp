#include <doctest/doctest.h>

#include <atomic>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <format>
#include <sstream>
#include <string>
#include <thread>

#include <grpcpp/grpcpp.h>

#include "costar/world.hpp"
#include "costar_grpc/server.hpp"

namespace v1 = costar::v1;

namespace {

std::string read_file(const std::filesystem::path& file) {
  std::ifstream in(file);
  std::stringstream text;
  text << in.rdbuf();
  return text.str();
}

// A server on a free local port, and a client stub for it.
struct Fixture {
  std::unique_ptr<v1::Costar::Service> service;
  std::unique_ptr<grpc::Server> server;
  std::unique_ptr<v1::Costar::Stub> client;

  explicit Fixture(costar::PlantFactories plants = {}) : service(costar::grpc_server::make_service(std::move(plants))) {
    int port = 0;
    grpc::ServerBuilder builder;
    builder.AddListeningPort("127.0.0.1:0", grpc::InsecureServerCredentials(), &port);
    builder.RegisterService(service.get());
    server = builder.BuildAndStart();
    client = v1::Costar::NewStub(
        grpc::CreateChannel("127.0.0.1:" + std::to_string(port), grpc::InsecureChannelCredentials()));
  }
  ~Fixture() { server->Shutdown(); }

  v1::SessionStatus create(const std::string& scenario_file) {
    grpc::ClientContext context;
    v1::CreateSessionRequest request;
    request.set_scenario_toml(read_file(std::string(SCENARIO_DIR) + "/" + scenario_file));
    request.set_scenario_path(std::string(SCENARIO_DIR) + "/" + scenario_file);
    request.add_image_dirs(std::filesystem::path(LINK_NODE).parent_path().string());
    v1::SessionStatus reply;
    REQUIRE(client->CreateSession(&context, request, &reply).ok());
    return reply;
  }

  v1::SessionStatus wait_until_stopped(uint64_t session) {
    v1::SessionStatus status;
    for (int i = 0; i < 10'000; ++i) {
      grpc::ClientContext context;
      v1::SessionRef ref;
      ref.set_session(session);
      REQUIRE(client->Status(&context, ref, &status).ok());
      if (!status.running()) break;
      std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    return status;
  }
};

}  // namespace

TEST_CASE("a remote client runs a scenario and watches the same trace as costar test") {
  Fixture f;
  const v1::SessionStatus created = f.create("link_nodes.toml");
  CHECK(created.name() == "link-nodes");

  {
    grpc::ClientContext context;
    v1::RunRequest run;
    run.set_session(created.session());
    run.set_until_ns(30'000'000);
    v1::SessionStatus status;
    REQUIRE(f.client->Run(&context, run, &status).ok());
  }
  CHECK(f.wait_until_stopped(created.session()).now_ns() == 30'000'000);

  // Watch from the start: the stream replays everything so far, then waits.
  grpc::ClientContext watch_context;
  v1::WatchRequest watch;
  watch.set_session(created.session());
  auto stream = f.client->Watch(&watch_context, watch);
  std::string trace;
  v1::TraceRecord record;
  for (uint64_t expected_index = 0; expected_index < 19 && stream->Read(&record); ++expected_index) {
    CHECK(record.index() == expected_index);
    trace += record.text().empty()
                 ? std::format("{} m{} {} {}\n", record.at_ns(), record.machine(), record.label(), record.value())
                 : std::format("{} m{} {} {}\n", record.at_ns(), record.machine(), record.label(), record.text());
  }
  watch_context.TryCancel();
  stream->Finish();
  CHECK(trace == read_file(std::string(SCENARIO_DIR) + "/link_nodes.trace"));
}

TEST_CASE("inputs, inspection and errors over gRPC") {
  Fixture f;
  const uint64_t session = f.create("ping_pong.toml").session();

  grpc::ClientContext input_context;
  v1::InputRequest input;
  input.set_session(session);
  input.set_target("receiver.link");
  input.set_deliver("hi");
  v1::SessionStatus status;
  REQUIRE(f.client->Input(&input_context, input, &status).ok());
  f.wait_until_stopped(session);

  {
    grpc::ClientContext context;
    v1::RunRequest run;
    run.set_session(session);
    run.set_until_ns(1'000);
    REQUIRE(f.client->Run(&context, run, &status).ok());
    f.wait_until_stopped(session);
  }

  grpc::ClientContext inspect_context;
  v1::SessionRef ref;
  ref.set_session(session);
  v1::Snapshot snapshot;
  REQUIRE(f.client->Inspect(&inspect_context, ref, &snapshot).ok());
  REQUIRE(snapshot.machines_size() == 2);
  CHECK(snapshot.machines(1).name() == "receiver");
  CHECK(snapshot.machines(1).devices(0).queued_frames() == 1);

  grpc::ClientContext bad_context;
  input.set_target("nobody.link");
  CHECK(f.client->Input(&bad_context, input, &status).error_code() == grpc::StatusCode::INVALID_ARGUMENT);

  grpc::ClientContext missing_context;
  ref.set_session(999);
  CHECK(f.client->Inspect(&missing_context, ref, &snapshot).error_code() == grpc::StatusCode::NOT_FOUND);

  grpc::ClientContext destroy_context;
  ref.set_session(session);
  v1::Empty empty;
  CHECK(f.client->DestroySession(&destroy_context, ref, &empty).ok());
  grpc::ClientContext list_context;
  v1::SessionList list;
  REQUIRE(f.client->ListSessions(&list_context, empty, &list).ok());
  CHECK(list.sessions_size() == 0);
}

TEST_CASE("a bad scenario is rejected with its error") {
  Fixture f;
  grpc::ClientContext context;
  v1::CreateSessionRequest request;
  request.set_scenario_toml("[[wire]]\nendpoints = [\"x.a\", \"y.a\"]");
  v1::SessionStatus reply;
  const grpc::Status status = f.client->CreateSession(&context, request, &reply);
  CHECK(status.error_code() == grpc::StatusCode::FAILED_PRECONDITION);
  CHECK(status.error_message().find("no machine named 'x'") != std::string::npos);
}

namespace {
// Counts its steps, and records each in the trace (via a register), so a
// watcher that never reads soon blocks the server's writes.
struct Counter : costar::Plant {
  std::atomic<int>* steps;
  explicit Counter(std::atomic<int>* s) : steps(s) {}
  void step(costar::Time, costar::World& world) override {
    world.machine(0).set_register("r", 0, static_cast<uint32_t>(++*steps));
  }
};
}  // namespace

TEST_CASE("destroying a session stops it, even while a Watch client is not reading") {
  std::atomic<int> steps = 0;
  Fixture f({{"counter", [&steps](const costar::PlantParams&, const costar::Scenario&) {
                return std::make_unique<Counter>(&steps);
              }}});
  grpc::ClientContext create_context;
  v1::CreateSessionRequest create;
  create.set_scenario_toml(
      "[[machine]]\nname = \"m\"\ndevices = [{ name = \"r\", kind = \"registers\" }]\n"
      "[[plant]]\ntype = \"counter\"\nperiod_us = 1\n");
  v1::SessionStatus created;
  REQUIRE(f.client->CreateSession(&create_context, create, &created).ok());

  grpc::ClientContext run_context;
  v1::RunRequest run;
  run.set_session(created.session());
  v1::SessionStatus status;
  REQUIRE(f.client->Run(&run_context, run, &status).ok());

  // A watcher that never reads: the server's writes back up.
  grpc::ClientContext watch_context;
  watch_context.set_deadline(std::chrono::system_clock::now() + std::chrono::seconds(20));
  v1::WatchRequest watch;
  watch.set_session(created.session());
  auto stream = f.client->Watch(&watch_context, watch);
  std::this_thread::sleep_for(std::chrono::milliseconds(300));  // the server's writes back up

  grpc::ClientContext destroy_context;
  v1::SessionRef ref;
  ref.set_session(created.session());
  v1::Empty empty;
  REQUIRE(f.client->DestroySession(&destroy_context, ref, &empty).ok());
  const int after_destroy = steps;
  std::this_thread::sleep_for(std::chrono::milliseconds(200));
  CHECK(steps == after_destroy);  // the simulation stopped with the session

  // The server ends the call itself: without client cancellation, the
  // stream runs dry and finishes well before its deadline.
  const auto drain_start = std::chrono::steady_clock::now();
  v1::TraceRecord record;
  while (stream->Read(&record)) {
  }
  const grpc::Status finished = stream->Finish();
  CHECK(finished.error_code() != grpc::StatusCode::DEADLINE_EXCEEDED);
  CHECK(std::chrono::steady_clock::now() - drain_start < std::chrono::seconds(10));
}

TEST_CASE("a remote client powers a machine off and on") {
  Fixture f;
  const uint64_t session = f.create("link_nodes.toml").session();
  const auto power = [&](const std::string& target, v1::Power action) {
    grpc::ClientContext context;
    v1::InputRequest input;
    input.set_session(session);
    input.set_target(target);
    input.set_power(action);
    v1::SessionStatus status;
    return f.client->Input(&context, input, &status);
  };
  REQUIRE(power("responder", v1::POWER_OFF).ok());
  CHECK(power("responder.link", v1::POWER_OFF).error_code() == grpc::StatusCode::INVALID_ARGUMENT);
  CHECK(power("nobody", v1::POWER_OFF).error_code() == grpc::StatusCode::INVALID_ARGUMENT);

  // Inputs take effect when the session runs.
  const auto cpu_state_after_running_to = [&](uint64_t until_ns) {
    {
      grpc::ClientContext context;
      v1::RunRequest run;
      run.set_session(session);
      run.set_until_ns(until_ns);
      v1::SessionStatus status;
      REQUIRE(f.client->Run(&context, run, &status).ok());
      f.wait_until_stopped(session);
    }
    grpc::ClientContext context;
    v1::SessionRef ref;
    ref.set_session(session);
    v1::Snapshot snapshot;
    REQUIRE(f.client->Inspect(&context, ref, &snapshot).ok());
    return snapshot.machines(1).cpu_state();
  };
  CHECK(cpu_state_after_running_to(1'000'000) == "off");
  REQUIRE(power("responder", v1::POWER_ON).ok());
  CHECK(cpu_state_after_running_to(2'000'000) == "halted");
}
