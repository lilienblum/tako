package internal

import (
	"encoding/json"
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
	userApp       http.Handler
}

// NewEndpointHandler creates a handler that intercepts tako internal requests.
func NewEndpointHandler(instanceID, version, internalToken string, userApp http.Handler) *EndpointHandler {
	return &EndpointHandler{
		startTime:     time.Now(),
		instanceID:    instanceID,
		version:       version,
		internalToken: internalToken,
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
