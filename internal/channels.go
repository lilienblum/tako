package internal

import (
	"net/http"
	"sort"
	"strings"
	"sync"
)

const (
	DefaultChannelRetentionMs             int64 = 24 * 60 * 60 * 1000
	DefaultChannelInactivityTtlMs         int64 = 0
	DefaultChannelKeepaliveIntervalMs     int64 = 25 * 1000
	DefaultChannelMaxConnectionLifetimeMs int64 = 2 * 60 * 60 * 1000
)

type ChannelTransport string

const (
	ChannelTransportWS ChannelTransport = "ws"
)

type ChannelOperation string

const (
	ChannelOperationSubscribe ChannelOperation = "subscribe"
	ChannelOperationPublish   ChannelOperation = "publish"
	ChannelOperationConnect   ChannelOperation = "connect"
)

type ChannelLifecycleConfig struct {
	ReplayWindowMs          int64 `json:"replayWindowMs,omitempty"`
	InactivityTtlMs         int64 `json:"inactivityTtlMs"`
	KeepaliveIntervalMs     int64 `json:"keepaliveIntervalMs,omitempty"`
	MaxConnectionLifetimeMs int64 `json:"maxConnectionLifetimeMs,omitempty"`
}

func (c ChannelLifecycleConfig) withDefaults() ChannelLifecycleConfig {
	if c.ReplayWindowMs == 0 {
		c.ReplayWindowMs = DefaultChannelRetentionMs
	}
	if c.KeepaliveIntervalMs == 0 {
		c.KeepaliveIntervalMs = DefaultChannelKeepaliveIntervalMs
	}
	if c.MaxConnectionLifetimeMs == 0 {
		c.MaxConnectionLifetimeMs = DefaultChannelMaxConnectionLifetimeMs
	}
	return c
}

type ChannelAuthContext struct {
	Channel   string           `json:"channel"`
	Operation ChannelOperation `json:"operation"`
	Pattern   string           `json:"pattern"`
}

type ChannelGrant struct {
	Subject string `json:"subject,omitempty"`
	ChannelLifecycleConfig
}

type ChannelAuthDecision struct {
	OK bool `json:"ok"`
	ChannelGrant
}

func AllowChannel(grant ChannelGrant) ChannelAuthDecision {
	grant.ChannelLifecycleConfig = grant.ChannelLifecycleConfig.withDefaults()
	return ChannelAuthDecision{
		OK:           true,
		ChannelGrant: grant,
	}
}

func RejectChannel() ChannelAuthDecision {
	return ChannelAuthDecision{OK: false}
}

type ChannelDefinition struct {
	Auth func(*http.Request, ChannelAuthContext) ChannelAuthDecision
	ChannelLifecycleConfig
	Transport ChannelTransport
}

type ChannelAuthRequest struct {
	URL     string            `json:"url"`
	Method  string            `json:"method,omitempty"`
	Headers map[string]string `json:"headers,omitempty"`
}

type ChannelAuthorizeInput struct {
	Channel   string             `json:"channel"`
	Operation ChannelOperation   `json:"operation"`
	Request   ChannelAuthRequest `json:"request"`
}

type ChannelAuthorizeResponse struct {
	OK        bool             `json:"ok"`
	Transport ChannelTransport `json:"transport,omitempty"`
	ChannelGrant
}

type Channel struct {
	name      string
	transport ChannelTransport
}

func (c *Channel) Name() string {
	return c.name
}

func (c *Channel) Transport() ChannelTransport {
	return c.transport
}

type channelDefinitionEntry struct {
	definition ChannelDefinition
	index      int
	pattern    string
}

type ChannelRegistry struct {
	mu          sync.RWMutex
	definitions []channelDefinitionEntry
	nextIndex   int
}

func NewChannelRegistry() *ChannelRegistry {
	return &ChannelRegistry{}
}

var Channels = NewChannelRegistry()

func (r *ChannelRegistry) Create(name string, definition ...ChannelDefinition) *Channel {
	if len(definition) > 0 {
		r.Define(name, definition[0])
		return &Channel{name: name, transport: definition[0].Transport}
	}
	return &Channel{name: name}
}

func (r *ChannelRegistry) Define(pattern string, definition ChannelDefinition) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.definitions = append(r.definitions, channelDefinitionEntry{
		definition: definition,
		index:      r.nextIndex,
		pattern:    pattern,
	})
	r.nextIndex++
}

func (r *ChannelRegistry) Clear() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.definitions = nil
	r.nextIndex = 0
}

func (r *ChannelRegistry) ResolveDefinition(channel string) *ChannelDefinition {
	r.mu.RLock()
	defer r.mu.RUnlock()

	matches := make([]channelDefinitionEntry, 0)
	for _, entry := range r.definitions {
		if patternMatches(entry.pattern, channel) {
			matches = append(matches, entry)
		}
	}
	if len(matches) == 0 {
		return nil
	}

	sort.SliceStable(matches, func(i, j int) bool {
		left, right := matches[i], matches[j]
		if isExactPattern(left.pattern) != isExactPattern(right.pattern) {
			return isExactPattern(left.pattern)
		}
		leftSpecificity := patternSpecificity(left.pattern)
		rightSpecificity := patternSpecificity(right.pattern)
		if leftSpecificity != rightSpecificity {
			return leftSpecificity > rightSpecificity
		}
		return left.index < right.index
	})

	definition := matches[0].definition
	return &definition
}

func (r *ChannelRegistry) Authorize(input ChannelAuthorizeInput) (ChannelAuthorizeResponse, bool, bool) {
	entry := r.resolveEntry(input.Channel)
	if entry == nil {
		return ChannelAuthorizeResponse{}, false, false
	}

	method := input.Request.Method
	if method == "" {
		method = http.MethodGet
	}
	req, err := http.NewRequest(method, input.Request.URL, nil)
	if err != nil {
		return ChannelAuthorizeResponse{}, true, false
	}
	for key, value := range input.Request.Headers {
		req.Header.Set(key, value)
	}

	decision := entry.definition.Auth(req, ChannelAuthContext{
		Channel:   input.Channel,
		Operation: input.Operation,
		Pattern:   entry.pattern,
	})
	if !decision.OK {
		return ChannelAuthorizeResponse{}, true, false
	}

	grant := decision.ChannelGrant
	if grant.ChannelLifecycleConfig == (ChannelLifecycleConfig{}) {
		grant.ChannelLifecycleConfig = entry.definition.ChannelLifecycleConfig
	}
	grant.ChannelLifecycleConfig = grant.ChannelLifecycleConfig.withDefaults()

	return ChannelAuthorizeResponse{
		OK:           true,
		Transport:    entry.definition.Transport,
		ChannelGrant: grant,
	}, true, true
}

func (r *ChannelRegistry) resolveEntry(channel string) *channelDefinitionEntry {
	r.mu.RLock()
	defer r.mu.RUnlock()

	matches := make([]channelDefinitionEntry, 0)
	for _, entry := range r.definitions {
		if patternMatches(entry.pattern, channel) {
			matches = append(matches, entry)
		}
	}
	if len(matches) == 0 {
		return nil
	}

	sort.SliceStable(matches, func(i, j int) bool {
		left, right := matches[i], matches[j]
		if isExactPattern(left.pattern) != isExactPattern(right.pattern) {
			return isExactPattern(left.pattern)
		}
		leftSpecificity := patternSpecificity(left.pattern)
		rightSpecificity := patternSpecificity(right.pattern)
		if leftSpecificity != rightSpecificity {
			return leftSpecificity > rightSpecificity
		}
		return left.index < right.index
	})

	entry := matches[0]
	return &entry
}

func isExactPattern(pattern string) bool {
	return !strings.Contains(pattern, "*")
}

func patternMatches(pattern, channel string) bool {
	if pattern == "*" {
		return true
	}
	if isExactPattern(pattern) {
		return pattern == channel
	}
	if strings.HasSuffix(pattern, "*") {
		return strings.HasPrefix(channel, strings.TrimSuffix(pattern, "*"))
	}
	return false
}

func patternSpecificity(pattern string) int {
	return len(strings.ReplaceAll(pattern, "*", ""))
}
