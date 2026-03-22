package internal

import (
	"sync"
)

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
// Called when tako-server pushes secrets via POST /secrets.
func (s *SecretStore) Inject(secrets map[string]string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.secrets = secrets
}

// String returns "[REDACTED]" to prevent accidental logging.
func (s *SecretStore) String() string {
	return "[REDACTED]"
}
