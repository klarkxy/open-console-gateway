package main

import (
	"fmt"
	"strings"
	"sync"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

type nativeGrant struct {
	kind string
	base string
}

var (
	nativeGrantMu sync.Mutex
	nativeGrants  []nativeGrant
)

func noteNativeGrant(kind, base string) {
	kind = strings.TrimSpace(kind)
	base = strings.TrimRight(strings.TrimSpace(base), "/")
	if kind == "" || base == "" {
		return
	}
	nativeGrantMu.Lock()
	nativeGrants = append(nativeGrants, nativeGrant{kind: kind, base: base})
	nativeGrantMu.Unlock()
}

func resetNativeGrants() {
	nativeGrantMu.Lock()
	nativeGrants = nil
	nativeGrantMu.Unlock()
}

func allowPins(log *policyLog, req policyRequest) ([]endpointPin, error) {
	if log != nil && log.overridePins {
		if len(log.pins) == 0 {
			return nil, nil
		}
		return append([]endpointPin(nil), log.pins...), nil
	}
	if req.ProviderID != "canonical-oauth" {
		return nil, nil
	}
	nativeGrantMu.Lock()
	grants := append([]nativeGrant(nil), nativeGrants...)
	nativeGrantMu.Unlock()
	localOnly := len(grants) > 0
	for _, grant := range grants {
		if grant.kind != "codex" && grant.kind != "xai" {
			localOnly = false
			break
		}
	}
	if req.GenerationKind == "count-tokens" && localOnly {
		return nil, nil
	}
	pins := make([]endpointPin, 0, 4)
	seenID := map[string]struct{}{}
	seenFingerprint := map[string]struct{}{}
	for index, grant := range grants {
		for _, target := range dispatchTargets(grant, req) {
			id := fmt.Sprintf("lb-%s-%d-%s", grant.kind, index, target.suffix)
			pin, err := pinForTarget(req.CallableProtocol, id, target.url)
			if err != nil {
				return nil, err
			}
			if _, ok := seenFingerprint[pin.EndpointFingerprint]; ok {
				continue
			}
			if _, ok := seenID[pin.EndpointID]; ok {
				return nil, fmt.Errorf("duplicate endpoint id %s", pin.EndpointID)
			}
			seenID[pin.EndpointID] = struct{}{}
			seenFingerprint[pin.EndpointFingerprint] = struct{}{}
			pins = append(pins, pin)
		}
	}
	if len(pins) > 32 {
		return nil, fmt.Errorf("endpoint pin vector %d exceeds 32", len(pins))
	}
	if len(pins) == 0 {
		return nil, nil
	}
	return pins, nil
}

type dispatchTarget struct {
	url    string
	suffix string
}

func dispatchTargets(grant nativeGrant, req policyRequest) []dispatchTarget {
	base := strings.TrimRight(grant.base, "/")
	kind := req.GenerationKind
	stream := kind == "stream" || kind == "stream-refresh" || kind == "stream-bootstrap"
	count := kind == "count-tokens"
	switch grant.kind {
	case "codex", "xai":
		if count {
			return nil
		}
		if stream {
			return []dispatchTarget{{url: base + "/responses", suffix: "responses"}}
		}
		return []dispatchTarget{
			{url: base + "/responses", suffix: "responses"},
			{url: base + "/responses/compact", suffix: "compact"},
		}
	case "claude":
		if count {
			return []dispatchTarget{{url: base + "/v1/messages/count_tokens?beta=true", suffix: "count"}}
		}
		return []dispatchTarget{{url: base + "/v1/messages?beta=true", suffix: "messages"}}
	case "kimi", "kimi-ai":
		if count {
			return []dispatchTarget{{url: base + "/v1/messages/count_tokens?beta=true", suffix: "count"}}
		}
		switch req.CallableProtocol {
		case "responses":
			return []dispatchTarget{{url: base + "/v1/responses", suffix: "responses"}}
		case "messages":
			return []dispatchTarget{{url: base + "/v1/messages?beta=true", suffix: "messages"}}
		default:
			return []dispatchTarget{{url: base + "/v1/chat/completions", suffix: "chat"}}
		}
	case "antigravity":
		if count {
			return []dispatchTarget{{url: base + "/v1internal:countTokens", suffix: "count"}}
		}
		if stream {
			return []dispatchTarget{{url: base + "/v1internal:streamGenerateContent?alt=sse", suffix: "stream"}}
		}
		return []dispatchTarget{
			{url: base + "/v1internal:generateContent", suffix: "generate"},
			{url: base + "/v1internal:streamGenerateContent?alt=sse", suffix: "stream"},
		}
	default:
		return nil
	}
}

func pinForTarget(protocol, id, raw string) (endpointPin, error) {
	_, origin, fingerprint, err := coreauth.CanonicalDispatchURL(raw)
	if err != nil {
		return endpointPin{}, err
	}
	return endpointPin{
		Protocol:            protocol,
		EndpointID:          id,
		Origin:              origin,
		EndpointFingerprint: fingerprint,
		HTTPMethod:          "POST",
	}, nil
}
