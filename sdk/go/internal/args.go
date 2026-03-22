package internal

import (
	"os"
)

// Config holds runtime configuration parsed from CLI args and env vars.
type Config struct {
	// InstanceID is the 8-char instance identifier assigned by tako-server.
	InstanceID string
	// Version is the deploy version string.
	Version string
	// Host is the address to bind to. Defaults to "0.0.0.0".
	Host string
	// Port is the TCP port to listen on. Defaults to "3000".
	Port string
	// InternalToken authenticates Host:tako requests from tako-server.
	// Set via TAKO_INTERNAL_TOKEN env var. Empty in dev mode (no auth required).
	InternalToken string
}

// ParseConfig reads configuration from os.Args and environment variables.
func ParseConfig() Config {
	return ParseConfigFrom(os.Args[1:], os.Getenv)
}

// ParseConfigFrom reads configuration from the given args and env lookup function.
func ParseConfigFrom(args []string, getenv func(string) string) Config {
	cfg := Config{
		Host: "0.0.0.0",
		Port: "3000",
	}

	// Parse CLI args: --instance <id> --version <ver>
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--instance":
			if i+1 < len(args) {
				i++
				cfg.InstanceID = args[i]
			}
		case "--version":
			if i+1 < len(args) {
				i++
				cfg.Version = args[i]
			}
		}
	}

	if host := getenv("HOST"); host != "" {
		cfg.Host = host
	}
	if port := getenv("PORT"); port != "" {
		cfg.Port = port
	}

	cfg.InternalToken = getenv("TAKO_INTERNAL_TOKEN")

	return cfg
}
