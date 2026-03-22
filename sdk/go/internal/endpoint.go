package internal

import (
	"encoding/json"
	"io"
	"net/http"
	"os"
	"time"
)

const internalTokenHeader = "x-tako-internal-token"

// StatusResponse is the JSON shape returned by GET /status on Host: tako.
type StatusResponse struct {
	Status        string `json:"status"`
	InstanceID    string `json:"instance_id"`
	Version       string `json:"version"`
	PID           int    `json:"pid"`
	UptimeSeconds int64  `json:"uptime_seconds"`
}

// EndpointHandler intercepts Host: tako requests for internal endpoints.
type EndpointHandler struct {
	startTime     time.Time
	instanceID    string
	version       string
	internalToken string
	secrets       *SecretStore
	userApp       http.Handler
}

// NewEndpointHandler creates a handler that intercepts tako internal requests.
func NewEndpointHandler(instanceID, version, internalToken string, secrets *SecretStore, userApp http.Handler) *EndpointHandler {
	return &EndpointHandler{
		startTime:     time.Now(),
		instanceID:    instanceID,
		version:       version,
		internalToken: internalToken,
		secrets:       secrets,
		userApp:       userApp,
	}
}

// ServeHTTP dispatches to internal endpoints or the user's app.
func (h *EndpointHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Host == "tako" {
		h.handleInternal(w, r)
		return
	}
	h.userApp.ServeHTTP(w, r)
}

func (h *EndpointHandler) handleInternal(w http.ResponseWriter, r *http.Request) {
	// Verify internal token when set (production mode).
	// In dev mode (no token), all Host:tako requests are allowed.
	if h.internalToken != "" {
		if r.Header.Get(internalTokenHeader) != h.internalToken {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
	}

	switch {
	case r.Method == http.MethodGet && r.URL.Path == "/status":
		h.handleStatus(w)
	case r.Method == http.MethodPost && r.URL.Path == "/secrets":
		h.handleSecrets(w, r)
	default:
		http.NotFound(w, r)
	}
}

func (h *EndpointHandler) handleStatus(w http.ResponseWriter) {
	resp := StatusResponse{
		Status:        "healthy",
		InstanceID:    h.instanceID,
		Version:       h.version,
		PID:           os.Getpid(),
		UptimeSeconds: int64(time.Since(h.startTime).Seconds()),
	}
	w.Header().Set("Content-Type", "application/json")
	if h.internalToken != "" {
		w.Header().Set(internalTokenHeader, h.internalToken)
	}
	json.NewEncoder(w).Encode(resp)
}

func (h *EndpointHandler) handleSecrets(w http.ResponseWriter, r *http.Request) {
	// Limit body to 1MB to prevent memory exhaustion.
	body, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
	if err != nil {
		http.Error(w, "failed to read body", http.StatusBadRequest)
		return
	}

	var secrets map[string]string
	if err := json.Unmarshal(body, &secrets); err != nil {
		http.Error(w, "invalid JSON", http.StatusBadRequest)
		return
	}

	h.secrets.Inject(secrets)

	w.Header().Set("Content-Type", "application/json")
	w.Write([]byte(`{"status":"ok"}`))
}
