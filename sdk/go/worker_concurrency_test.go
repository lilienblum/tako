package tako

import (
	"context"
	"encoding/json"
	"testing"
	"time"
)

func TestWorkerHonorsConcurrencyAndDrains(t *testing.T) {
	resetRegistry()
	s := startMockServer(t)
	t.Setenv("TAKO_INTERNAL_SOCKET", s.path)
	t.Setenv("TAKO_APP_NAME", "test-app")
	t.Setenv("TAKO_WORKER_CONCURRENCY", "2")
	started := make(chan struct{}, 3)
	release := make(chan struct{})
	defer close(release)
	RegisterWorkflow("work", func(_ *WorkflowContext, _ json.RawMessage) error {
		started <- struct{}{}
		<-release
		return nil
	})
	for range 3 {
		s.seed("work", 3)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() { done <- RunWorker(ctx) }()
	for range 2 {
		select {
		case <-started:
		case <-time.After(3 * time.Second):
			t.Fatal("configured concurrent handlers did not start")
		}
	}
	select {
	case <-started:
		t.Fatal("worker exceeded configured concurrency")
	case <-time.After(50 * time.Millisecond):
	}
	cancel()
	select {
	case <-done:
		t.Fatal("worker returned before active handlers drained")
	case <-time.After(50 * time.Millisecond):
	}
	release <- struct{}{}
	release <- struct{}{}
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("worker did not finish draining")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	completed, pending := 0, 0
	for _, task := range s.tasks {
		if task.Status == "succeeded" {
			completed++
		}
		if task.Status == "pending" {
			pending++
		}
	}
	if completed != 2 || pending != 1 {
		t.Fatalf("drain: completed=%d pending=%d", completed, pending)
	}
}
