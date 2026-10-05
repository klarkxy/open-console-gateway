package main

import (
	"context"
	"encoding/json"
	"io"
	"net/http"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

func withTestProtocol(ctx context.Context) context.Context {
	return coreauth.WithOCGCallableProtocol(ctx, "chat_completions")
}

func readPolicy(r *http.Request) (policyRequest, []byte, error) {
	raw, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
	if err != nil {
		return policyRequest{}, nil, err
	}
	if err := validatePolicyDocument(raw); err != nil {
		return policyRequest{}, raw, err
	}
	var req policyRequest
	if err := json.Unmarshal(raw, &req); err != nil {
		return policyRequest{}, raw, err
	}
	return req, raw, nil
}

func writeReady(w http.ResponseWriter, req policyRequest) {
	_ = json.NewEncoder(w).Encode(map[string]any{
		"protocolVersion":    1,
		"policyReady":        true,
		"processGeneration":  req.ProcessGeneration,
		"projectionRevision": req.ProjectionRevision,
		"projectionDigest":   req.ProjectionDigest,
		"reason":             "ready",
		"unavailable":        false,
		"endpointPins":       nil,
	})
}

func writeDecision(w http.ResponseWriter, action, reason string) {
	writeDecisionPins(w, action, reason, nil)
}

func writeDecisionPins(w http.ResponseWriter, action, reason string, pins []endpointPin) {
	var encoded any
	if len(pins) == 0 {
		encoded = nil
	} else {
		encoded = pins
	}
	_ = json.NewEncoder(w).Encode(map[string]any{
		"protocolVersion":     1,
		"action":              action,
		"reason":              reason,
		"restrictionDeadline": nil,
		"unavailable":         false,
		"endpointPins":        encoded,
	})
}
