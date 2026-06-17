package costar

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"
)

// Client is a JSON-RPC 2.0 client connected to a costar serve process.
type Client struct {
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	stdout *bufio.Reader
	stderr io.ReadCloser

	mu      sync.Mutex
	nextID  int64
	closed  atomic.Bool
}

// Start spawns a costar serve --stdio process and returns a connected Client.
//
// binaryPath is the path to the costar binary (e.g. "target/debug/sim-runner").
// The process working directory is derived from the binary path (project root).
// The returned Client must be closed with Close() to clean up the child process.
func Start(ctx context.Context, binaryPath string) (*Client, error) {
	cmd := exec.CommandContext(ctx, binaryPath, "serve", "--stdio")

	// Compute the project root from the binary path.
	// Binary is at <root>/target/{debug,release}/sim-runner.
	// Walk up to find a directory containing Cargo.toml or tests/.
	if abs, err := filepath.Abs(binaryPath); err == nil {
		dir := filepath.Dir(abs) // <root>/target/{debug,release}
		for {
			if _, err := os.Stat(filepath.Join(dir, "Cargo.toml")); err == nil {
				cmd.Dir = dir
				break
			}
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			dir = parent
		}
	}

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("failed to create stdin pipe: %w", err)
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("failed to create stdout pipe: %w", err)
	}

	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("failed to create stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("failed to start %s: %w", binaryPath, err)
	}

	c := &Client{
		cmd:    cmd,
		stdin:  stdin,
		stdout: bufio.NewReader(stdout),
		stderr: stderr,
		nextID: 1,
	}

	// Drain stderr in the background so the child process doesn't block.
	go c.drainStderr()

	return c, nil
}

// drainStderr reads and discards stderr output.
func (c *Client) drainStderr() {
	// Read in 4KB chunks until EOF or error.
	buf := make([]byte, 4096)
	for {
		_, err := c.stderr.Read(buf)
		if err != nil {
			return
		}
	}
}

// call sends a JSON-RPC request and returns the parsed response.
func (c *Client) call(method string, params interface{}) (*RPCResponse, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	if c.closed.Load() {
		return nil, fmt.Errorf("client is closed")
	}

	id := c.nextID
	c.nextID++

	req := RPCRequest{
		JSONRPC: "2.0",
		ID:      id,
		Method:  method,
		Params:  params,
	}

	reqBytes, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request: %w", err)
	}

	// Write the request line.
	if _, err := c.stdin.Write(append(reqBytes, '\n')); err != nil {
		return nil, fmt.Errorf("failed to write request: %w", err)
	}

	// Read the response line with a timeout.
	type readResult struct {
		line string
		err  error
	}
	ch := make(chan readResult, 1)
	go func() {
		line, err := c.stdout.ReadString('\n')
		ch <- readResult{line, err}
	}()

	var line string
	select {
	case res := <-ch:
		if res.err != nil {
			c.closed.Store(true)
			return nil, fmt.Errorf("failed to read response: %w", res.err)
		}
		line = res.line
	case <-time.After(30 * time.Second):
		c.closed.Store(true)
		return nil, fmt.Errorf("timeout waiting for response to %s", method)
	}

	var resp RPCResponse
	if err := json.Unmarshal([]byte(line), &resp); err != nil {
		return nil, fmt.Errorf("failed to parse response: %w (raw: %s)", err, line)
	}

	if resp.ID != id {
		return nil, fmt.Errorf("response ID mismatch: expected %d, got %d", id, resp.ID)
	}

	if resp.Error != nil {
		return nil, resp.Error
	}

	return &resp, nil
}

// CreateSession creates a new simulation session.
func (c *Client) CreateSession() (*SessionInfo, error) {
	resp, err := c.call("session.create", struct{}{})
	if err != nil {
		return nil, fmt.Errorf("session.create: %w", err)
	}

	var info SessionInfo
	if err := json.Unmarshal(resp.Result, &info); err != nil {
		return nil, fmt.Errorf("session.create: failed to parse result: %w", err)
	}
	return &info, nil
}

// DestroySession destroys a simulation session.
func (c *Client) DestroySession(sessionID uint64) error {
	params := map[string]uint64{"session_id": sessionID}
	_, err := c.call("session.destroy", params)
	if err != nil {
		return fmt.Errorf("session.destroy: %w", err)
	}
	return nil
}

// LoadScenario loads a scenario TOML file into a session.
func (c *Client) LoadScenario(sessionID uint64, path string) (*ScenarioLoadResult, error) {
	params := map[string]interface{}{
		"session_id": sessionID,
		"path":       path,
	}
	resp, err := c.call("scenario.load", params)
	if err != nil {
		return nil, fmt.Errorf("scenario.load: %w", err)
	}

	var result ScenarioLoadResult
	if err := json.Unmarshal(resp.Result, &result); err != nil {
		return nil, fmt.Errorf("scenario.load: failed to parse result: %w", err)
	}
	return &result, nil
}

// Run executes a simulation in the given session and returns the results.
func (c *Client) Run(sessionID uint64) (*RunResult, error) {
	params := map[string]uint64{"session_id": sessionID}
	resp, err := c.call("sim.run", params)
	if err != nil {
		return nil, fmt.Errorf("sim.run: %w", err)
	}

	var result RunResult
	if err := json.Unmarshal(resp.Result, &result); err != nil {
		return nil, fmt.Errorf("sim.run: failed to parse result: %w", err)
	}
	return &result, nil
}

// GetTrace retrieves the trace for a session.
//
// format can be "human" or "jsonl" (defaults to "jsonl").
func (c *Client) GetTrace(sessionID uint64, format string) (*TraceResult, error) {
	if format == "" {
		format = "jsonl"
	}
	params := map[string]interface{}{
		"session_id": sessionID,
		"format":     format,
	}
	resp, err := c.call("trace.get", params)
	if err != nil {
		return nil, fmt.Errorf("trace.get: %w", err)
	}

	var result TraceResult
	if err := json.Unmarshal(resp.Result, &result); err != nil {
		return nil, fmt.Errorf("trace.get: failed to parse result: %w", err)
	}
	return &result, nil
}

// Close shuts down the server and waits for the process to exit.
func (c *Client) Close() error {
	if c.closed.Swap(true) {
		return nil // Already closed.
	}

	// Try to send server.shutdown gracefully.
	// Ignore errors — the process may have already exited.
	_ = c.sendShutdown()

	// Close stdin so the server's read loop can exit.
	_ = c.stdin.Close()

	// Wait for the process to exit with a timeout.
	done := make(chan error, 1)
	go func() {
		done <- c.cmd.Wait()
	}()

	select {
	case err := <-done:
		return err
	case <-time.After(5 * time.Second):
		// Force kill if it doesn't exit gracefully.
		_ = c.cmd.Process.Kill()
		<-done
		return fmt.Errorf("costar server did not exit gracefully, killed")
	}
}

// sendShutdown sends the server.shutdown request without blocking on response.
func (c *Client) sendShutdown() error {
	req := RPCRequest{
		JSONRPC: "2.0",
		ID:      c.nextID,
		Method:  "server.shutdown",
		Params:  struct{}{},
	}
	c.nextID++

	reqBytes, err := json.Marshal(req)
	if err != nil {
		return err
	}
	_, err = c.stdin.Write(append(reqBytes, '\n'))
	return err
}
