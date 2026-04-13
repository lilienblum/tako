package tako

import (
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"tako.sh/internal"
)

func TestGetSecretFallsBackToEnv(t *testing.T) {
	// When the secret store is empty, GetSecret falls back to os.Getenv.
	secrets.Inject(map[string]string{})
	key := "TAKO_TEST_SECRET_FALLBACK"
	origVal := os.Getenv(key)
	os.Setenv(key, "from-env")
	defer setOrUnset(key, origVal)

	if got := GetSecret(key); got != "from-env" {
		t.Errorf("GetSecret(%q) = %q, want %q (env fallback)", key, got, "from-env")
	}

	// Store value takes priority over env.
	secrets.Inject(map[string]string{key: "from-store"})
	if got := GetSecret(key); got != "from-store" {
		t.Errorf("GetSecret(%q) = %q, want %q (store priority)", key, got, "from-store")
	}
}

func TestGetSecret(t *testing.T) {
	secrets.Inject(map[string]string{"KEY": "value", "OTHER": "data"})

	if got := GetSecret("KEY"); got != "value" {
		t.Errorf("GetSecret(KEY) = %q, want %q", got, "value")
	}
	if got := GetSecret("OTHER"); got != "data" {
		t.Errorf("GetSecret(OTHER) = %q, want %q", got, "data")
	}
	// MISSING is not in the store and should not be in env either.
	os.Unsetenv("MISSING")
	if got := GetSecret("MISSING"); got != "" {
		t.Errorf("GetSecret(MISSING) = %q, want empty", got)
	}
}

func TestMetadata(t *testing.T) {
	configOnce = syncOnce()
	origArgs := os.Args
	os.Args = []string{"test", "--instance", "meta1234", "--version", "v5.0"}
	defer func() { os.Args = origArgs }()

	if got := InstanceID(); got != "meta1234" {
		t.Errorf("InstanceID() = %q, want %q", got, "meta1234")
	}
	if got := Version(); got != "v5.0" {
		t.Errorf("Version() = %q, want %q", got, "v5.0")
	}
	if got := Uptime(); got <= 0 {
		t.Errorf("Uptime() = %v, want > 0", got)
	}
}

func TestMetadataEmptyInDevMode(t *testing.T) {
	configOnce = syncOnce()
	origArgs := os.Args
	os.Args = []string{"test"}
	defer func() { os.Args = origArgs }()

	if got := InstanceID(); got != "" {
		t.Errorf("InstanceID() = %q, want empty in dev mode", got)
	}
	if got := Version(); got != "" {
		t.Errorf("Version() = %q, want empty in dev mode", got)
	}
}

func TestListenerTCP(t *testing.T) {
	configOnce = syncOnce()
	origArgs := os.Args
	os.Args = []string{"test"}
	origPort := os.Getenv("PORT")
	os.Setenv("PORT", "19876")
	defer func() {
		os.Args = origArgs
		setOrUnset("PORT", origPort)
	}()

	ln, err := Listener()
	if err != nil {
		t.Fatalf("Listener() error: %v", err)
	}
	defer ln.Close()

	if ln.Addr().Network() != "tcp" {
		t.Errorf("network = %q, want tcp", ln.Addr().Network())
	}
}

func TestFullProtocol(t *testing.T) {
	configOnce = syncOnce()
	origArgs := os.Args
	os.Args = []string{"test", "--instance", "full1234", "--version", "v3"}
	origPort := os.Getenv("PORT")
	origHost := os.Getenv("HOST")
	origToken := os.Getenv("TAKO_INTERNAL_TOKEN")
	os.Setenv("HOST", "127.0.0.1")
	os.Setenv("PORT", "0")
	os.Setenv("TAKO_INTERNAL_TOKEN", "test-token")
	defer func() {
		os.Args = origArgs
		setOrUnset("PORT", origPort)
		setOrUnset("HOST", origHost)
		setOrUnset("TAKO_INTERNAL_TOKEN", origToken)
	}()

	userHandler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		w.Write([]byte("<!doctype html><html><body>hello</body></html>"))
	})

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()

	cfg := config()
	wrapped := internal.NewEndpointHandler(cfg.InstanceID, cfg.Version, cfg.InternalToken, userHandler)
	go http.Serve(ln, wrapped)
	time.Sleep(10 * time.Millisecond)

	addr := ln.Addr().String()
	client := &http.Client{}

	// Health check (with token)
	req, _ := http.NewRequest("GET", "http://"+addr+"/status", nil)
	req.Host = "tako.internal"
	req.Header.Set("x-tako-internal-token", "test-token")
	resp, err := client.Do(req)
	if err != nil {
		t.Fatalf("health check failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != 200 {
		t.Fatalf("health check status = %d, want 200", resp.StatusCode)
	}

	var status internal.StatusResponse
	json.NewDecoder(resp.Body).Decode(&status)
	if status.InstanceID != "full1234" {
		t.Errorf("instance_id = %q, want %q", status.InstanceID, "full1234")
	}

	// User passthrough
	req3, _ := http.NewRequest("GET", "http://"+addr+"/", nil)
	resp3, err := client.Do(req3)
	if err != nil {
		t.Fatal(err)
	}
	defer resp3.Body.Close()

	body, _ := io.ReadAll(resp3.Body)
	if !strings.Contains(string(body), "hello") {
		t.Errorf("body = %q, want to contain %q", string(body), "hello")
	}
}

func TestListenerEmitsReadySignal(t *testing.T) {
	configOnce = syncOnce()
	origArgs := os.Args
	os.Args = []string{"test"}
	origPort := os.Getenv("PORT")
	os.Setenv("PORT", "0")
	defer func() {
		os.Args = origArgs
		setOrUnset("PORT", origPort)
	}()

	// Capture stdout to verify the TAKO:READY signal.
	origStdout := os.Stdout
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	os.Stdout = w

	ln, err := Listener()
	if err != nil {
		os.Stdout = origStdout
		t.Fatalf("Listener() error: %v", err)
	}
	defer ln.Close()

	w.Close()
	os.Stdout = origStdout

	out, _ := io.ReadAll(r)
	line := strings.TrimSpace(string(out))

	if !strings.HasPrefix(line, "TAKO:READY:") {
		t.Fatalf("expected TAKO:READY:<port>, got %q", line)
	}

	portStr := strings.TrimPrefix(line, "TAKO:READY:")
	tcpAddr := ln.Addr().(*net.TCPAddr)
	if portStr != fmt.Sprintf("%d", tcpAddr.Port) {
		t.Errorf("ready signal port = %s, listener port = %d", portStr, tcpAddr.Port)
	}
}

func setOrUnset(key, value string) {
	if value != "" {
		os.Setenv(key, value)
	} else {
		os.Unsetenv(key)
	}
}

func syncOnce() sync.Once {
	return sync.Once{}
}
