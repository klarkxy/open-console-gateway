package main

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"time"

	sdkhandlers "github.com/router-for-me/CLIProxyAPI/v8/sdk/api/handlers"
	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

var (
	errPrivateBridge   = errors.New("invalid_private_bridge")
	errRequestDeadline = errors.New("invalid_request_deadline")
)

// privateBridge is the management validated tuple. Omitted kind is accepted.
// Accepted requests that carry a pin or protocol are rejected. Validated
// requests require the applied auth id, one approved protocol, and the
// handler path for that protocol.
func privateBridge(kind, pin, protocol, path string) (string, string, string, error) {
	kind = strings.ToLower(strings.TrimSpace(kind))
	pin = strings.TrimSpace(pin)
	protocol = strings.ToLower(strings.TrimSpace(protocol))
	switch kind {
	case "":
		kind = "accepted"
	case "accepted", "validated":
	default:
		return "", "", "", errPrivateBridge
	}
	if kind == "accepted" {
		if pin != "" || protocol != "" {
			return "", "", "", errPrivateBridge
		}
		return kind, "", "", nil
	}
	if pin == "" || !approvedProtocol(protocol) {
		return "", "", "", errPrivateBridge
	}
	got, ok := pathProtocol(path)
	if !ok || got != protocol {
		return "", "", "", errPrivateBridge
	}
	return kind, pin, protocol, nil
}

func approvedProtocol(protocol string) bool {
	switch protocol {
	case "chat_completions", "responses", "messages":
		return true
	default:
		return false
	}
}

func ingressCallableProtocol(path string) string {
	path = strings.TrimSuffix(strings.TrimSpace(path), "/")
	if protocol, ok := pathProtocol(path); ok {
		return protocol
	}
	switch path {
	case "/v1/messages/count_tokens", "/openai/v1/messages/count_tokens":
		return "messages"
	}
	if strings.HasPrefix(path, "/backend-api/codex/") {
		return "responses"
	}
	if strings.HasPrefix(path, "/v1beta/models/") && (strings.Contains(path, ":generateContent") || strings.Contains(path, ":streamGenerateContent") || strings.Contains(path, ":countTokens")) {
		return "chat_completions"
	}
	return ""
}

func pathProtocol(path string) (string, bool) {
	path = strings.TrimSuffix(strings.TrimSpace(path), "/")
	switch path {
	case "/v1/chat/completions", "/openai/v1/chat/completions":
		return "chat_completions", true
	case "/v1/responses", "/openai/v1/responses", "/v1/responses/compact", "/openai/v1/responses/compact":
		return "responses", true
	case "/v1/messages", "/openai/v1/messages":
		return "messages", true
	default:
		return "", false
	}
}

// parseRequestDeadline accepts UTC RFC3339, including a fractional second.
// A zero offset is UTC. Ingress expiry uses the wall clock; capture retention
// later uses the captured instant plus 60s.
func parseRequestDeadline(raw string, now time.Time) (time.Time, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return time.Time{}, errRequestDeadline
	}
	parsed, err := time.Parse(time.RFC3339Nano, raw)
	if err != nil {
		return time.Time{}, errRequestDeadline
	}
	_, offset := parsed.Zone()
	if offset != 0 || !now.Before(parsed) {
		return time.Time{}, errRequestDeadline
	}
	return parsed, nil
}

func stripPrivateOCG(header http.Header) {
	if header == nil {
		return
	}
	for key := range header {
		if strings.HasPrefix(strings.ToLower(key), "x-ocg-") {
			header.Del(key)
		}
	}
}

func (h *Host) hasAppliedAuth(id string) bool {
	if h == nil || h.service == nil || strings.TrimSpace(id) == "" {
		return false
	}
	manager := h.service.CoreAuthManager()
	if manager == nil {
		return false
	}
	for _, auth := range manager.List() {
		if auth != nil && auth.ID == id {
			return true
		}
	}
	return false
}

func bridgeContext(ctx context.Context, requestID, kind, pin, protocol string, deadline time.Time) (context.Context, context.CancelFunc) {
	ctx = withRequestID(ctx, requestID)
	ctx = coreauth.WithOCGRequestKind(ctx, kind)
	if kind == "validated" {
		ctx = sdkhandlers.WithPinnedAuthID(ctx, pin)
		ctx = coreauth.WithOCGValidatedProtocol(ctx, protocol)
	}
	return context.WithDeadline(ctx, deadline)
}
