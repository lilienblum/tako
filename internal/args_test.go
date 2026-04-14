package internal

import (
	"testing"
)

func TestParseConfigFromArgsAndEnv(t *testing.T) {
	args := []string{"--instance", "abcd1234"}
	cfg := ParseConfigFrom(args, func(key string) string {
		switch key {
		case "TAKO_BUILD":
			return "v1.0"
		default:
			return ""
		}
	})

	if cfg.InstanceID != "abcd1234" {
		t.Errorf("InstanceID = %q, want %q", cfg.InstanceID, "abcd1234")
	}
	if cfg.Version != "v1.0" {
		t.Errorf("Version = %q, want %q", cfg.Version, "v1.0")
	}
}

func TestParseConfigPortAndHostFromEnv(t *testing.T) {
	cfg := ParseConfigFrom(nil, func(key string) string {
		switch key {
		case "PORT":
			return "8080"
		case "HOST":
			return "127.0.0.1"
		default:
			return ""
		}
	})

	if cfg.Port != "8080" {
		t.Errorf("Port = %q, want %q", cfg.Port, "8080")
	}
	if cfg.Host != "127.0.0.1" {
		t.Errorf("Host = %q, want %q", cfg.Host, "127.0.0.1")
	}
}

func TestParseConfigDefaults(t *testing.T) {
	cfg := ParseConfigFrom(nil, func(string) string { return "" })

	if cfg.Port != "3000" {
		t.Errorf("Port = %q, want %q", cfg.Port, "3000")
	}
	if cfg.Host != "0.0.0.0" {
		t.Errorf("Host = %q, want %q", cfg.Host, "0.0.0.0")
	}
}

func TestParseConfigDevMode(t *testing.T) {
	cfg := ParseConfigFrom(nil, func(string) string { return "" })

	if cfg.InstanceID != "" {
		t.Errorf("InstanceID should be empty in dev mode, got %q", cfg.InstanceID)
	}
}
