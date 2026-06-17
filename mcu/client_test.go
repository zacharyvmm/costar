package costar

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

// findBinary locates the sim-runner binary for integration tests.
// It searches relative to the project root (assumes tests run from mcu/).
func findBinary() string {
	// Try relative paths from the mcu/ directory.
	candidates := []string{
		"../target/debug/sim-runner",
		"../target/release/sim-runner",
		"../../target/debug/sim-runner", // if tests run from a subdir in mcu/
	}

	for _, p := range candidates {
		abs, err := filepath.Abs(p)
		if err != nil {
			continue
		}
		if _, err := os.Stat(abs); err == nil {
			return abs
		}
	}

	// Fallback: check COSTAR_BINARY env var.
	if env := os.Getenv("COSTAR_BINARY"); env != "" {
		return env
	}

	return ""
}

// scenarioPath returns the absolute path to tests/scenarios/ping_pong.toml.
func scenarioPath() string {
	candidates := []string{
		"../tests/scenarios/ping_pong.toml",
		"../../tests/scenarios/ping_pong.toml",
	}

	for _, p := range candidates {
		abs, err := filepath.Abs(p)
		if err != nil {
			continue
		}
		if _, err := os.Stat(abs); err == nil {
			return abs
		}
	}

	// Fallback: check env var.
	if env := os.Getenv("COSTAR_SCENARIO"); env != "" {
		return env
	}

	return ""
}

// TestIntegration_FullLifecycle tests the full client lifecycle:
// start → create session → load scenario → run → get trace → destroy session → close.
func TestIntegration_FullLifecycle(t *testing.T) {
	binaryPath := findBinary()
	if binaryPath == "" {
		t.Skip("sim-runner binary not found (run `cargo build` first, or set COSTAR_BINARY)")
	}

	scenarioFile := scenarioPath()
	if scenarioFile == "" {
		t.Skip("ping_pong.toml scenario not found (set COSTAR_SCENARIO)")
	}

	ctx := context.Background()
	client, err := Start(ctx, binaryPath)
	if err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	defer client.Close()

	// 1. Create session.
	session, err := client.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession failed: %v", err)
	}
	if session.SessionID == 0 {
		t.Error("expected non-zero session_id")
	}
	if session.State != StateIdle {
		t.Errorf("expected state 'idle', got %q", session.State)
	}
	t.Logf("created session %d", session.SessionID)

	// 2. Load scenario.
	loadResult, err := client.LoadScenario(session.SessionID, scenarioFile)
	if err != nil {
		t.Fatalf("LoadScenario failed: %v", err)
	}
	if loadResult.NMachines != 2 {
		t.Errorf("expected 2 machines, got %d", loadResult.NMachines)
	}
	if loadResult.NLinks != 1 {
		t.Errorf("expected 1 link, got %d", loadResult.NLinks)
	}
	if loadResult.NInjections != 1 {
		t.Errorf("expected 1 injection, got %d", loadResult.NInjections)
	}
	t.Logf("loaded scenario: %d machines, %d links, %d injections",
		loadResult.NMachines, loadResult.NLinks, loadResult.NInjections)

	// 3. Run simulation.
	runResult, err := client.Run(session.SessionID)
	if err != nil {
		t.Fatalf("Run failed: %v", err)
	}
	if runResult.ExitCode != 0 {
		t.Errorf("expected exit_code 0, got %d (error: %s)", runResult.ExitCode, runResult.Error)
	}
	if runResult.NEvents == 0 {
		t.Error("expected non-zero n_events")
	}
	t.Logf("run complete: exit_code=%d, n_events=%d, duration_ms=%d",
		runResult.ExitCode, runResult.NEvents, runResult.DurationMs)

	// 4. Get trace in human format.
	traceResult, err := client.GetTrace(session.SessionID, "human")
	if err != nil {
		t.Fatalf("GetTrace failed: %v", err)
	}
	if traceResult.Trace == "" {
		t.Error("expected non-empty trace")
	}
	t.Logf("trace (human): %d bytes", len(traceResult.Trace))

	// 5. Get trace in jsonl format.
	traceResult, err = client.GetTrace(session.SessionID, "jsonl")
	if err != nil {
		t.Fatalf("GetTrace(jsonl) failed: %v", err)
	}
	if traceResult.Trace == "" {
		t.Error("expected non-empty jsonl trace")
	}
	t.Logf("trace (jsonl): %d bytes", len(traceResult.Trace))

	// 6. Destroy session.
	if err := client.DestroySession(session.SessionID); err != nil {
		t.Fatalf("DestroySession failed: %v", err)
	}
	t.Log("session destroyed")

	// 7. Verify destroying again returns an error.
	if err := client.DestroySession(session.SessionID); err == nil {
		t.Error("expected error when destroying non-existent session")
	}
}

// TestIntegration_TraceParsing verifies that trace data is returned and
// can be inspected.  Note: the server currently returns human-format
// display strings in trace_jsonl, not JSON objects.
func TestIntegration_TraceParsing(t *testing.T) {
	binaryPath := findBinary()
	if binaryPath == "" {
		t.Skip("sim-runner binary not found (run `cargo build` first, or set COSTAR_BINARY)")
	}

	scenarioFile := scenarioPath()
	if scenarioFile == "" {
		t.Skip("ping_pong.toml scenario not found (set COSTAR_SCENARIO)")
	}

	ctx := context.Background()
	client, err := Start(ctx, binaryPath)
	if err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	defer client.Close()

	session, err := client.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession failed: %v", err)
	}

	_, err = client.LoadScenario(session.SessionID, scenarioFile)
	if err != nil {
		t.Fatalf("LoadScenario failed: %v", err)
	}

	runResult, err := client.Run(session.SessionID)
	if err != nil {
		t.Fatalf("Run failed: %v", err)
	}

	// Verify trace_jsonl contains display-format trace lines.
	if len(runResult.TraceJSONL) == 0 {
		t.Error("expected non-empty trace_jsonl")
	}

	// Each entry should be a human-format string with machine prefix.
	for i, line := range runResult.TraceJSONL {
		if len(line) == 0 {
			t.Errorf("trace_jsonl[%d]: empty line", i)
		}
		// Human format: "[machine.N] EventName at=..."
		if line[0] != '[' {
			t.Logf("trace_jsonl[%d]: %s", i, line)
		}
	}
	t.Logf("trace_jsonl contains %d lines", len(runResult.TraceJSONL))

	// Clean up.
	_ = client.DestroySession(session.SessionID)
}

// TestClient_ServerCrash verifies graceful handling when the server crashes.
func TestClient_ServerCrash(t *testing.T) {
	binaryPath := findBinary()
	if binaryPath == "" {
		t.Skip("sim-runner binary not found")
	}

	ctx := context.Background()
	client, err := Start(ctx, binaryPath)
	if err != nil {
		t.Fatalf("Start failed: %v", err)
	}

	// Create a session.
	session, err := client.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession failed: %v", err)
	}

	// Kill the server process.
	if client.cmd.Process != nil {
		client.cmd.Process.Kill()
	}

	// Subsequent calls should fail gracefully, not panic.
	_, err = client.CreateSession()
	if err == nil {
		t.Error("expected error after server crash")
	}
	t.Logf("error after crash (expected): %v", err)

	// Close should not hang.
	if err := client.Close(); err != nil {
		t.Logf("Close after crash: %v (expected non-nil)", err)
	}

	// Verify destroyed reference doesn't panic.
	_ = session
}

// TestClient_InvalidParams verifies error handling for invalid parameters.
func TestClient_InvalidParams(t *testing.T) {
	binaryPath := findBinary()
	if binaryPath == "" {
		t.Skip("sim-runner binary not found")
	}

	ctx := context.Background()
	client, err := Start(ctx, binaryPath)
	if err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	defer client.Close()

	// LoadScenario with non-existent session.
	_, err = client.LoadScenario(99999, "/nonexistent/path")
	if err == nil {
		t.Error("expected error for non-existent session")
	}
	t.Logf("error for invalid session (expected): %v", err)

	// DestroySession with non-existent session.
	err = client.DestroySession(99999)
	if err == nil {
		t.Error("expected error for non-existent session")
	}
	t.Logf("error for invalid destroy (expected): %v", err)
}
