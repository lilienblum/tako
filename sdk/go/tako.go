// Package tako is the Tako SDK for Go.
//
// It handles the Tako protocol (TCP serving, health checks, secrets)
// so your Go app can be deployed and managed by Tako.
//
// # Quick Start
//
// Most Go frameworks implement [http.Handler] and work directly with
// [ListenAndServe]:
//
//	mux := http.NewServeMux()
//	mux.HandleFunc("/", handler)
//	tako.ListenAndServe(mux)
//
// This also works with Gin, Echo, Chi, gorilla/mux, and any other framework
// that implements [http.Handler].
//
// # Secrets
//
// Secrets are accessed via a generated Secrets struct in tako_secrets.go.
// Run `tako typegen` to generate it from your project's secret definitions:
//
//	db := Secrets.DatabaseUrl()
//	key := Secrets.ApiKey()
//
// # Fiber
//
// Fiber uses fasthttp (not net/http), so use [Listener] directly:
//
//	ln, _ := tako.Listener()
//	app := fiber.New()
//	app.Listener(ln)
package tako

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/signal"
	"sync"
	"syscall"
	"time"

	"tako.sh/internal"
)

var (
	secrets    = internal.NewSecretStore()
	configOnce sync.Once
	configVal  internal.Config
	startTime  = time.Now()
)

func config() internal.Config {
	configOnce.Do(func() {
		configVal = internal.ParseConfig()
	})
	return configVal
}

// ListenAndServe wraps the given handler with Tako protocol support and starts
// serving. It blocks until the server shuts down.
//
// Handles SIGTERM and SIGINT for graceful shutdown — in-flight requests are
// given 10 seconds to complete before the server force-closes. This is important
// for rolling deploys where tako-server sends SIGTERM to old instances.
//
// The app listens on HOST:PORT (from environment variables, defaulting to
// 0.0.0.0:3000). In production, tako-server sets these to the assigned
// address for the instance.
//
// Works with any [http.Handler]:
//
//	// net/http
//	mux := http.NewServeMux()
//	tako.ListenAndServe(mux)
//
//	// Gin
//	r := gin.Default()
//	tako.ListenAndServe(r)
//
//	// Echo
//	e := echo.New()
//	tako.ListenAndServe(e)
//
//	// Chi
//	r := chi.NewRouter()
//	tako.ListenAndServe(r)
func ListenAndServe(handler http.Handler) error {
	ln, err := Listener()
	if err != nil {
		return err
	}

	cfg := config()
	wrapped := internal.NewEndpointHandler(cfg.InstanceID, cfg.Version, cfg.InternalToken, secrets, handler)

	srv := &http.Server{Handler: wrapped}

	// Graceful shutdown on SIGTERM/SIGINT
	shutdownCh := make(chan os.Signal, 1)
	signal.Notify(shutdownCh, syscall.SIGTERM, syscall.SIGINT)
	defer signal.Stop(shutdownCh)

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	select {
	case err := <-errCh:
		if err == http.ErrServerClosed {
			return nil
		}
		return err
	case <-shutdownCh:
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		return srv.Shutdown(ctx)
	}
}

// Listener returns a [net.Listener] configured for the Tako environment.
//
// Listens on HOST:PORT from environment variables (defaults to 0.0.0.0:3000).
// In production, tako-server sets these to the instance's assigned address.
//
// Use this for frameworks that manage their own server lifecycle, like Fiber:
//
//	ln, err := tako.Listener()
//	if err != nil {
//	    log.Fatal(err)
//	}
//	app := fiber.New()
//	app.Listener(ln)
func Listener() (net.Listener, error) {
	cfg := config()
	addr := net.JoinHostPort(cfg.Host, cfg.Port)
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("tako: failed to listen on %s: %w", addr, err)
	}
	return ln, nil
}

// InstanceID returns the Tako instance identifier assigned by tako-server.
// Returns an empty string in development mode.
//
// Useful for structured logging and distributed tracing:
//
//	slog.Info("request handled",
//	    "instance", tako.InstanceID(),
//	    "path", r.URL.Path,
//	)
func InstanceID() string {
	return config().InstanceID
}

// Version returns the deploy version string.
// Returns an empty string in development mode.
//
// Useful for logging, health endpoints, and error reporting:
//
//	slog.Info("server started", "version", tako.Version())
func Version() string {
	return config().Version
}

// Uptime returns how long since the process started.
//
//	slog.Info("status", "uptime", tako.Uptime())
func Uptime() time.Duration {
	return time.Since(startTime)
}

// GetSecret returns a secret value by name. This is called by generated code
// in tako_secrets.go — use the typed Secrets struct instead of calling this
// directly.
//
// Run `tako typegen` to generate the Secrets struct:
//
//	// Generated in tako_secrets.go — use this:
//	db := Secrets.DatabaseUrl()
//
//	// Instead of this:
//	db := tako.GetSecret("DATABASE_URL")
func GetSecret(name string) string {
	return secrets.Get(name)
}
