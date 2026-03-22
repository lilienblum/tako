package internal

import (
	"sync"
	"testing"
)

func TestSecretStoreGetEmpty(t *testing.T) {
	s := NewSecretStore()
	if got := s.Get("missing"); got != "" {
		t.Errorf("Get(missing) = %q, want empty", got)
	}
}

func TestSecretStoreInjectAndGet(t *testing.T) {
	s := NewSecretStore()
	s.Inject(map[string]string{"DB_URL": "postgres://...", "API_KEY": "secret123"})

	if got := s.Get("DB_URL"); got != "postgres://..." {
		t.Errorf("Get(DB_URL) = %q, want %q", got, "postgres://...")
	}
	if got := s.Get("API_KEY"); got != "secret123" {
		t.Errorf("Get(API_KEY) = %q, want %q", got, "secret123")
	}
}

func TestSecretStoreAll(t *testing.T) {
	s := NewSecretStore()
	s.Inject(map[string]string{"A": "1", "B": "2"})

	all := s.All()
	if len(all) != 2 {
		t.Fatalf("All() returned %d entries, want 2", len(all))
	}

	// Verify it's a copy — modifying the returned map shouldn't affect the store
	all["A"] = "modified"
	if got := s.Get("A"); got != "1" {
		t.Errorf("store was modified through All() return value")
	}
}

func TestSecretStoreString(t *testing.T) {
	s := NewSecretStore()
	s.Inject(map[string]string{"KEY": "value"})

	if got := s.String(); got != "[REDACTED]" {
		t.Errorf("String() = %q, want %q", got, "[REDACTED]")
	}
}

func TestSecretStoreConcurrent(t *testing.T) {
	s := NewSecretStore()
	s.Inject(map[string]string{"KEY": "initial"})

	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(2)
		go func() {
			defer wg.Done()
			_ = s.Get("KEY")
		}()
		go func() {
			defer wg.Done()
			s.Inject(map[string]string{"KEY": "updated"})
		}()
	}
	wg.Wait()
}
