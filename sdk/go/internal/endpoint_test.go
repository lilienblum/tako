package internal

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"testing"
)

func TestStatusEndpoint(t *testing.T) {
	handler := NewEndpointHandler("test1234", "v1.0", "", http.NotFoundHandler())

	req := httptest.NewRequest(http.MethodGet, "/status", nil)
	req.Host = "tako"
	w := httptest.NewRecorder()

	handler.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("status code = %d, want 200", w.Code)
	}

	var resp StatusResponse
	json.NewDecoder(w.Body).Decode(&resp)

	if resp.Status != "healthy" {
		t.Errorf("status = %q, want %q", resp.Status, "healthy")
	}
	if resp.InstanceID != "test1234" {
		t.Errorf("instance_id = %q, want %q", resp.InstanceID, "test1234")
	}
	if resp.PID != os.Getpid() {
		t.Errorf("pid = %d, want %d", resp.PID, os.Getpid())
	}
}

func TestTokenVerification(t *testing.T) {
	handler := NewEndpointHandler("test1234", "v1.0", "secret-token", http.NotFoundHandler())

	// Valid token → 200 + token echoed back
	req := httptest.NewRequest(http.MethodGet, "/status", nil)
	req.Host = "tako"
	req.Header.Set("x-tako-internal-token", "secret-token")
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("valid token: status = %d, want 200", w.Code)
	}
	if got := w.Header().Get("x-tako-internal-token"); got != "secret-token" {
		t.Errorf("response token = %q, want %q", got, "secret-token")
	}

	// Wrong token → 401
	req2 := httptest.NewRequest(http.MethodGet, "/status", nil)
	req2.Host = "tako"
	req2.Header.Set("x-tako-internal-token", "wrong")
	w2 := httptest.NewRecorder()
	handler.ServeHTTP(w2, req2)

	if w2.Code != http.StatusUnauthorized {
		t.Errorf("wrong token: status = %d, want 401", w2.Code)
	}

	// Missing token → 401
	req3 := httptest.NewRequest(http.MethodGet, "/status", nil)
	req3.Host = "tako"
	w3 := httptest.NewRecorder()
	handler.ServeHTTP(w3, req3)

	if w3.Code != http.StatusUnauthorized {
		t.Errorf("missing token: status = %d, want 401", w3.Code)
	}
}

func TestNoTokenInDevMode(t *testing.T) {
	// Empty token (dev mode) → no auth required
	handler := NewEndpointHandler("test1234", "v1.0", "", http.NotFoundHandler())

	req := httptest.NewRequest(http.MethodGet, "/status", nil)
	req.Host = "tako"
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Errorf("dev mode (no token): status = %d, want 200", w.Code)
	}
}

func TestNonTakoHostPassthrough(t *testing.T) {
	called := false
	userApp := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		called = true
		w.Write([]byte("user response"))
	})

	handler := NewEndpointHandler("test1234", "v1.0", "", userApp)

	req := httptest.NewRequest(http.MethodGet, "/", nil)
	req.Host = "example.com"
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, req)

	if !called {
		t.Error("user app should be called for non-tako host")
	}
}
