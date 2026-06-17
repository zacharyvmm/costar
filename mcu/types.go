// Package costar provides a JSON-RPC 2.0 client for the costar serve protocol.
package costar

import (
	"encoding/json"
	"fmt"
)

// ── JSON-RPC 2.0 envelope ──────────────────────────────────────────────────

// RPCRequest is a JSON-RPC 2.0 request.
type RPCRequest struct {
	JSONRPC string      `json:"jsonrpc"`
	ID      int64       `json:"id"`
	Method  string      `json:"method"`
	Params  interface{} `json:"params,omitempty"`
}

// RPCResponse is a JSON-RPC 2.0 response.
type RPCResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      int64           `json:"id"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *RPCError       `json:"error,omitempty"`
}

// RPCError is a JSON-RPC 2.0 error object.
type RPCError struct {
	Code    int64           `json:"code"`
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data,omitempty"`
}

func (e *RPCError) Error() string {
	return fmt.Sprintf("JSON-RPC error %d: %s", e.Code, e.Message)
}

// ── Session types ──────────────────────────────────────────────────────────

// SessionState represents the state of a simulation session.
type SessionState string

const (
	StateIdle    SessionState = "idle"
	StateReady   SessionState = "ready"
	StateRunning SessionState = "running"
	StateDone    SessionState = "done"
	StateError   SessionState = "error"
)

// SessionInfo is the response from session.create.
type SessionInfo struct {
	SessionID uint64       `json:"session_id"`
	State     SessionState `json:"state"`
}

// SessionSummary is an entry in session.list.
type SessionSummary struct {
	SessionID  uint64       `json:"session_id"`
	State      SessionState `json:"state"`
	NMachines  int          `json:"n_machines"`
	UptimeTicks uint64      `json:"uptime_ticks"`
}

// ── Scenario types ─────────────────────────────────────────────────────────

// ScenarioLoadResult is the response from scenario.load / scenario.load_inline.
type ScenarioLoadResult struct {
	NMachines   int `json:"n_machines"`
	NLinks      int `json:"n_links"`
	NInjections int `json:"n_injections"`
}

// ── Simulation types ───────────────────────────────────────────────────────

// RunResult is the response from sim.run.
type RunResult struct {
	ExitCode    int      `json:"exit_code"`
	NEvents     int      `json:"n_events"`
	TraceJSONL  []string `json:"trace_jsonl"`
	DurationMs  uint64   `json:"duration_ms"`
	Error       string   `json:"error,omitempty"`
}

// SimStatus is the response from sim.status.
type SimStatus struct {
	State      SessionState `json:"state"`
	NowTicks   uint64       `json:"now_ticks"`
	NMachines  int          `json:"n_machines"`
}

// ── Trace types ────────────────────────────────────────────────────────────

// TraceResult is the response from trace.get.
type TraceResult struct {
	Trace string `json:"trace"`
}

// TraceEvent is a single trace event that can be parsed from JSONL.
//
// NOTE: The current costar server returns trace events as human-format
// display strings (e.g. "[machine.0] EventDispatched at=0..."), not as
// JSON objects.  ParseTraceEvents is provided for forward compatibility
// when the server supports JSONL trace output.
//
// Uses the serde(tag="event") pattern from the Rust TraceEvent enum.
type TraceEvent struct {
	Event       string `json:"event"`
	At          uint64 `json:"at"`

	// EventScheduled
	ID          uint64 `json:"id,omitempty"`
	Priority    uint16 `json:"priority,omitempty"`
	Label       string `json:"label,omitempty"`
	TargetAt    uint64 `json:"target_at,omitempty"`

	// TaskResume / TaskYield
	Task   uint64 `json:"task,omitempty"`
	Reason string `json:"reason,omitempty"`

	// InterruptRaised / InterruptDelivered
	IRQ uint32 `json:"irq,omitempty"`

	// PacketRx / PacketTx
	Len int `json:"len,omitempty"`

	// Fatal
	Code int `json:"code,omitempty"`

	// UserU32
	Value uint32 `json:"value,omitempty"`

	// MachineEvent (multi-machine)
	MachineID   uint64 `json:"machine_id,omitempty"`
	MachineName string `json:"machine_name,omitempty"`
}

// ParseTraceEvents parses JSONL trace strings into TraceEvent structs.
func ParseTraceEvents(jsonl []string) ([]TraceEvent, error) {
	events := make([]TraceEvent, 0, len(jsonl))
	for _, line := range jsonl {
		var ev TraceEvent
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			return nil, fmt.Errorf("failed to parse trace event: %w", err)
		}
		events = append(events, ev)
	}
	return events, nil
}
