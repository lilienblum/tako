package internal

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"sync"
	"syscall"
)

// SecretsFromFd3 reads secrets JSON from file descriptor 3 (Tako runtime ABI).
//
// Returns nil if fd 3 does not exist (EBADF — not running under Tako).
// Exits hard on invalid JSON (broken Tako launch path).
func SecretsFromFd3() map[string]string {
	return secretsFromFd(3)
}

// secretsFromFd reads secrets JSON from the given file descriptor.
// Extracted from SecretsFromFd3 for testability (tests can use arbitrary fds
// without clobbering fd 3 which the Go test harness uses).
func secretsFromFd(fd int) map[string]string {
	f := os.NewFile(uintptr(fd), "tako-secrets")
	if f == nil {
		return nil
	}
	defer f.Close()

	data, err := io.ReadAll(f)
	if err != nil {
		if errors.Is(err, syscall.EBADF) {
			return nil
		}
		fmt.Fprintf(os.Stderr, "tako: failed to read secrets from fd %d: %v\n", fd, err)
		os.Exit(1)
	}

	var secrets map[string]string
	if err := json.Unmarshal(data, &secrets); err != nil {
		fmt.Fprintf(os.Stderr, "tako: invalid secrets JSON on fd %d: %v\n", fd, err)
		os.Exit(1)
	}
	return secrets
}

// SecretStore is a thread-safe store for Tako-managed secrets.
type SecretStore struct {
	mu      sync.RWMutex
	secrets map[string]string
}

// NewSecretStore creates an empty secret store.
func NewSecretStore() *SecretStore {
	return &SecretStore{
		secrets: make(map[string]string),
	}
}

// Get returns a single secret value by name.
// Returns empty string if the secret doesn't exist.
func (s *SecretStore) Get(name string) string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.secrets[name]
}

// All returns a copy of all secrets.
func (s *SecretStore) All() map[string]string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	out := make(map[string]string, len(s.secrets))
	for k, v := range s.secrets {
		out[k] = v
	}
	return out
}

// Inject replaces all secrets with the given map.
func (s *SecretStore) Inject(secrets map[string]string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.secrets = secrets
}

// String returns "[REDACTED]" to prevent accidental logging.
func (s *SecretStore) String() string {
	return "[REDACTED]"
}
