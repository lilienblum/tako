// Package tako provides the runtime SDK for Tako-deployed Go applications.
//
// All durable-task state is owned by tako-server. The SDK is a thin RPC
// client over the per-app unix socket (path in TAKO_ENQUEUE_SOCKET). The
// SDK itself has no SQLite dependency.
package tako

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"time"
)

// EnqueueSocketEnv is the env var tako-server sets to point SDK clients at
// the per-app enqueue unix socket.
const EnqueueSocketEnv = "TAKO_ENQUEUE_SOCKET"

// EnqueueOpts controls per-enqueue behavior. Nil fields fall back to
// server-side defaults (runAt = now, maxAttempts = 3, no dedup).
type EnqueueOpts struct {
	RunAt       *time.Time
	MaxAttempts *uint32
	// UniqueKey, if set, collapses this enqueue onto any existing
	// non-terminal task with the same key. Used for cron idempotency.
	UniqueKey *string
}

// EnqueueResult is the server's response.
type EnqueueResult struct {
	ID           string
	Deduplicated bool
}

// Enqueue dispatches a task to the named workflow.
func Enqueue(ctx context.Context, name string, payload any, opts EnqueueOpts) (*EnqueueResult, error) {
	client, err := ClientFromEnv()
	if err != nil {
		return nil, err
	}
	return client.Enqueue(ctx, name, payload, opts)
}

// Client is a thin RPC wrapper over the per-app enqueue socket.
type Client struct {
	socketPath string
}

// NewClient constructs a client rooted at the given socket path.
func NewClient(socketPath string) *Client {
	return &Client{socketPath: socketPath}
}

// ClientFromEnv reads TAKO_ENQUEUE_SOCKET.
func ClientFromEnv() (*Client, error) {
	sock := os.Getenv(EnqueueSocketEnv)
	if sock == "" {
		return nil, errors.New("tako: " + EnqueueSocketEnv + " is not set")
	}
	return NewClient(sock), nil
}

func (c *Client) Enqueue(ctx context.Context, name string, payload any, opts EnqueueOpts) (*EnqueueResult, error) {
	if payload == nil {
		payload = struct{}{}
	}
	cmd := map[string]any{
		"command": "enqueue_run",
		"app":     "",
		"name":    name,
		"payload": payload,
		"opts":    optsToWire(opts),
	}
	data, err := c.call(ctx, cmd)
	if err != nil {
		return nil, err
	}
	var ok struct {
		ID           string `json:"id"`
		Deduplicated bool   `json:"deduplicated"`
	}
	if err := json.Unmarshal(data, &ok); err != nil {
		return nil, fmt.Errorf("tako: parse enqueue response: %w", err)
	}
	return &EnqueueResult{ID: ok.ID, Deduplicated: ok.Deduplicated}, nil
}

// RegisterSchedules sends the list of cron schedules to the server.
func (c *Client) RegisterSchedules(ctx context.Context, schedules []ScheduleSpec) error {
	_, err := c.call(ctx, map[string]any{
		"command":   "register_schedules",
		"app":       "",
		"schedules": schedules,
	})
	return err
}

// ScheduleSpec is one workflow+cron pair sent on worker startup.
type ScheduleSpec struct {
	Name string `json:"name"`
	Cron string `json:"cron"`
}

// Task is the server-returned task payload from ClaimTask.
type Run struct {
	ID          string          `json:"id"`
	Name        string          `json:"name"`
	Payload     json.RawMessage `json:"payload"`
	Status      string          `json:"status"`
	Attempts    uint32          `json:"attempts"`
	MaxAttempts uint32          `json:"max_attempts"`
	RunAtMs     int64           `json:"run_at_ms"`
	StepState   map[string]any  `json:"step_state"`
}

// Claim atomically claims the oldest eligible task and bumps attempts.
// Returns nil when nothing is due.
func (c *Client) Claim(ctx context.Context, workerID string, names []string, leaseMs uint64) (*Run, error) {
	data, err := c.call(ctx, map[string]any{
		"command":   "claim_run",
		"worker_id": workerID,
		"names":     names,
		"lease_ms":  leaseMs,
	})
	if err != nil {
		return nil, err
	}
	if len(data) == 0 || string(data) == "null" {
		return nil, nil
	}
	var t Run
	if err := json.Unmarshal(data, &t); err != nil {
		return nil, fmt.Errorf("tako: parse task: %w", err)
	}
	if t.StepState == nil {
		t.StepState = map[string]any{}
	}
	return &t, nil
}

// Heartbeat extends the lease on a running task.
func (c *Client) Heartbeat(ctx context.Context, id string, leaseMs uint64) error {
	_, err := c.call(ctx, map[string]any{
		"command":  "heartbeat_run",
		"id":       id,
		"lease_ms": leaseMs,
	})
	return err
}

// SaveStep persists a single completed step result. First-write-wins on
// (run_id, step_name) — duplicate saves are ignored server-side.
func (c *Client) SaveStep(ctx context.Context, id, stepName string, result any) error {
	_, err := c.call(ctx, map[string]any{
		"command":   "save_step",
		"id":        id,
		"step_name": stepName,
		"result":    result,
	})
	return err
}

// Complete marks the run succeeded.
func (c *Client) Complete(ctx context.Context, id string) error {
	_, err := c.call(ctx, map[string]any{"command": "complete_run", "id": id})
	return err
}

// Cancel ends the run cleanly as `cancelled` (no retries).
func (c *Client) Cancel(ctx context.Context, id string, reason *string) error {
	body := map[string]any{"command": "cancel_run", "id": id, "reason": nil}
	if reason != nil {
		body["reason"] = *reason
	}
	_, err := c.call(ctx, body)
	return err
}

// Defer parks the run for later. nil wakeAt = parked indefinitely.
// Does not consume retry budget.
func (c *Client) Defer(ctx context.Context, id string, wakeAt *time.Time) error {
	body := map[string]any{"command": "defer_run", "id": id, "wake_at_ms": nil}
	if wakeAt != nil {
		body["wake_at_ms"] = wakeAt.UnixMilli()
	}
	_, err := c.call(ctx, body)
	return err
}

// WaitForEvent parks the run on a named event. Resumes when a matching
// Signal arrives or timeoutAt elapses.
func (c *Client) WaitForEvent(
	ctx context.Context, id, stepName, eventName string, timeoutAt *time.Time,
) error {
	body := map[string]any{
		"command":       "wait_for_event",
		"id":            id,
		"step_name":     stepName,
		"event_name":    eventName,
		"timeout_at_ms": nil,
	}
	if timeoutAt != nil {
		body["timeout_at_ms"] = timeoutAt.UnixMilli()
	}
	_, err := c.call(ctx, body)
	return err
}

// Signal delivers an event payload, waking every parked WaitForEvent with
// matching name. Returns the number of runs woken.
func (c *Client) Signal(ctx context.Context, eventName string, payload any) (uint64, error) {
	data, err := c.call(ctx, map[string]any{
		"command":    "signal",
		"app":        "",
		"event_name": eventName,
		"payload":    payload,
	})
	if err != nil {
		return 0, err
	}
	var resp struct {
		Woken uint64 `json:"woken"`
	}
	if err := json.Unmarshal(data, &resp); err != nil {
		return 0, fmt.Errorf("tako: parse signal response: %w", err)
	}
	return resp.Woken, nil
}

// Signal is a top-level convenience that uses the client from env.
func Signal(ctx context.Context, eventName string, payload any) (uint64, error) {
	c, err := ClientFromEnv()
	if err != nil {
		return 0, err
	}
	return c.Signal(ctx, eventName, payload)
}

// Fail records a failure. When finalize is true the run becomes dead;
// otherwise it goes back to pending with nextRunAt as its new run_at.
func (c *Client) Fail(ctx context.Context, id, errMsg string, nextRunAt *time.Time, finalize bool) error {
	body := map[string]any{
		"command":  "fail_run",
		"id":       id,
		"error":    errMsg,
		"finalize": finalize,
	}
	if nextRunAt != nil {
		body["next_run_at_ms"] = nextRunAt.UnixMilli()
	} else {
		body["next_run_at_ms"] = nil
	}
	_, err := c.call(ctx, body)
	return err
}

func optsToWire(o EnqueueOpts) map[string]any {
	w := map[string]any{}
	if o.RunAt != nil {
		w["run_at_ms"] = o.RunAt.UnixMilli()
	}
	if o.MaxAttempts != nil {
		w["max_attempts"] = *o.MaxAttempts
	}
	if o.UniqueKey != nil {
		w["unique_key"] = *o.UniqueKey
	}
	return w
}

type wireResponse struct {
	Status  string          `json:"status"`
	Data    json.RawMessage `json:"data,omitempty"`
	Message string          `json:"message,omitempty"`
}

func (c *Client) call(ctx context.Context, cmd map[string]any) (json.RawMessage, error) {
	d := net.Dialer{Timeout: 5 * time.Second}
	conn, err := d.DialContext(ctx, "unix", c.socketPath)
	if err != nil {
		return nil, fmt.Errorf("tako: dial enqueue socket: %w", err)
	}
	defer conn.Close()
	if deadline, ok := ctx.Deadline(); ok {
		_ = conn.SetDeadline(deadline)
	} else {
		_ = conn.SetDeadline(time.Now().Add(30 * time.Second))
	}

	body, err := json.Marshal(cmd)
	if err != nil {
		return nil, fmt.Errorf("tako: marshal: %w", err)
	}
	body = append(body, '\n')
	if _, err := conn.Write(body); err != nil {
		return nil, fmt.Errorf("tako: write: %w", err)
	}

	r := bufio.NewReader(conn)
	line, err := r.ReadBytes('\n')
	if err != nil {
		return nil, fmt.Errorf("tako: read: %w", err)
	}
	var resp wireResponse
	if err := json.Unmarshal(line, &resp); err != nil {
		return nil, fmt.Errorf("tako: parse response: %w", err)
	}
	if resp.Status != "ok" {
		msg := resp.Message
		if msg == "" {
			msg = "rpc failed"
		}
		return nil, errors.New("tako: " + msg)
	}
	return resp.Data, nil
}
