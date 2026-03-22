package tako

import (
	"encoding/json"
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

func TestGetSecret(t *testing.T) {
	secrets.Inject(map[string]string{"KEY": "value", "OTHER": "data"})

	if got := GetSecret("KEY"); got != "value" {
		t.Errorf("GetSecret(KEY) = %q, want %q", got, "value")
	}
	if got := GetSecret("OTHER"); got != "data" {
		t.Errorf("GetSecret(OTHER) = %q, want %q", got, "data")
	}
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
	wrapped := internal.NewEndpointHandler(cfg.InstanceID, cfg.Version, cfg.InternalToken, secrets, userHandler)
	go http.Serve(ln, wrapped)
	time.Sleep(10 * time.Millisecond)

	addr := ln.Addr().String()
	client := &http.Client{}

	// Health check (with token)
	req, _ := http.NewRequest("GET", "http://"+addr+"/status", nil)
	req.Host = "tako"
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

	// Secrets injection (with token)
	secretsBody := `{"SECRET_KEY":"secret_value"}`
	req2, _ := http.NewRequest("POST", "http://"+addr+"/secrets", strings.NewReader(secretsBody))
	req2.Host = "tako"
	req2.Header.Set("x-tako-internal-token", "test-token")
	resp2, err := client.Do(req2)
	if err != nil {
		t.Fatal(err)
	}
	resp2.Body.Close()

	if got := GetSecret("SECRET_KEY"); got != "secret_value" {
		t.Errorf("GetSecret(SECRET_KEY) = %q, want %q", got, "secret_value")
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
