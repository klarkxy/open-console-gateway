package auth

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"net/http"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

type dispatchBoundary struct {
	pins []EndpointPin
	n    atomic.Int32
}

func (d *dispatchBoundary) BeforeSend(context.Context, *Auth, string, string, string) (GenerationDecision, error) {
	id := "attempt-1"
	if d.n.Add(1) > 1 {
		id = "attempt-2"
	}
	return GenerationDecision{Action: "allow", Reason: "eligible", AttemptID: id, EndpointPins: append([]EndpointPin(nil), d.pins...)}, nil
}

func (d *dispatchBoundary) AfterResult(context.Context, *Auth, string, string, string, *Result) error {
	return nil
}

func admitDispatch(t *testing.T, oauth bool, kind string, pins []EndpointPin) (context.Context, *Auth) {
	t.Helper()
	manager := &Manager{}
	manager.SetGenerationBoundary(&dispatchBoundary{pins: pins})
	auth := &Auth{ID: "auth-1", Provider: "codex", Metadata: map[string]any{"type": "codex", "ocg_oauth": oauth}}
	ctx := WithOCGCallableProtocol(context.Background(), "responses")
	ctx, err := manager.beforeGenerationSend(ctx, auth, kind, "gpt-5.5", "gpt-5.5")
	if err != nil {
		t.Fatalf("admit: %v", err)
	}
	return ctx, auth
}

func dispatchRequest(t *testing.T, raw string) *http.Request {
	t.Helper()
	req, err := http.NewRequest(http.MethodPost, raw, nil)
	if err != nil {
		t.Fatal(err)
	}
	return req
}

func dispatchPin(t *testing.T, raw string) EndpointPin {
	t.Helper()
	_, origin, fingerprint, err := CanonicalDispatchURL(raw)
	if err != nil {
		t.Fatal(err)
	}
	return EndpointPin{Protocol: "responses", EndpointID: "lb-1", Origin: origin, EndpointFingerprint: fingerprint, HTTPMethod: "POST"}
}

func TestOCGNativeDispatchGuard(t *testing.T) {
	const raw = "http://127.0.0.1:9/v1/responses"
	pin := dispatchPin(t, raw)

	t.Run("context copy consumes once", func(t *testing.T) {
		ctx, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{pin})
		req := dispatchRequest(t, raw)
		if err := BeforeOCGHTTPDispatch(ctx, req); err != nil {
			t.Fatal(err)
		}
		copied := context.WithValue(ctx, struct{ name string }{}, "copy")
		if err := BeforeOCGHTTPDispatch(copied, req); err == nil || !errors.Is(err, ErrAttemptStop) {
			t.Fatalf("second dispatch = %v", err)
		}
		facts := generationFactsFrom(ctx)
		if facts == nil || !facts.dispatchConsumed || facts.matched == nil || facts.matched.EndpointID != pin.EndpointID {
			t.Fatalf("facts = %+v", facts)
		}
	})

	t.Run("parent and internal keep separate permits", func(t *testing.T) {
		parent, auth := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{pin})
		child, err := AdmitInternalGeneration(parent, auth, "gpt-5.5")
		if err != nil {
			t.Fatal(err)
		}
		if CurrentAttemptID(parent) == "" || CurrentAttemptID(parent) == CurrentAttemptID(child) {
			t.Fatalf("parent %s child %s", CurrentAttemptID(parent), CurrentAttemptID(child))
		}
		req := dispatchRequest(t, raw)
		if err := BeforeOCGHTTPDispatch(child, req); err != nil {
			t.Fatal(err)
		}
		if err := BeforeOCGHTTPDispatch(parent, req); err != nil {
			t.Fatalf("parent permit consumed by child: %v", err)
		}
	})

	t.Run("mismatch and host mutation do not consume", func(t *testing.T) {
		ctx, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{pin})
		wrong := dispatchRequest(t, "http://127.0.0.1:9/v1/other")
		if err := BeforeOCGHTTPDispatch(ctx, wrong); err == nil || !errors.Is(err, ErrAttemptStop) {
			t.Fatalf("mismatch = %v", err)
		}
		facts := generationFactsFrom(ctx)
		if facts == nil || facts.dispatchConsumed || facts.sent || facts.transport != "" {
			t.Fatal("mismatch consumed the permit or performed IO")
		}
		if err := BeforeOCGHTTPDispatch(ctx, dispatchRequest(t, raw)); err == nil || !errors.Is(err, ErrAttemptStop) {
			t.Fatalf("halted context resumed: %v", err)
		}
		if facts.dispatchConsumed {
			t.Fatal("resumed context consumed the permit")
		}
		hostCtx, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{pin})
		mutated := dispatchRequest(t, raw)
		mutated.Host = "evil.example"
		if err := BeforeOCGHTTPDispatch(hostCtx, mutated); err == nil || !errors.Is(err, ErrAttemptStop) {
			t.Fatalf("host mutation = %v", err)
		}
		hostFacts := generationFactsFrom(hostCtx)
		if hostFacts == nil || hostFacts.dispatchConsumed || hostFacts.sent || hostFacts.transport != "" {
			t.Fatal("host mutation consumed the permit or performed IO")
		}
		if err := BeforeOCGHTTPDispatch(hostCtx, dispatchRequest(t, raw)); err == nil || !errors.Is(err, ErrAttemptStop) {
			t.Fatalf("host denial resumed: %v", err)
		}
		fresh, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{pin})
		if err := BeforeOCGHTTPDispatch(fresh, dispatchRequest(t, raw)); err != nil {
			t.Fatalf("fresh request: %v", err)
		}
		freshFacts := generationFactsFrom(fresh)
		if freshFacts == nil || !freshFacts.dispatchConsumed || freshFacts.matched == nil || freshFacts.matched.EndpointID != pin.EndpointID {
			t.Fatal("fresh permit was not usable")
		}
		if facts.dispatchConsumed || hostFacts.dispatchConsumed {
			t.Fatal("halted permits were consumed")
		}
	})

	t.Run("installed boundary refuses missing or unadmitted facts", func(t *testing.T) {
		manager := &Manager{}
		manager.SetGenerationBoundary(&dispatchBoundary{pins: []EndpointPin{pin}})
		auth := &Auth{ID: "auth-1", Provider: "codex", Metadata: map[string]any{"type": "codex", "ocg_oauth": true}}
		missing := context.WithValue(context.Background(), internalGateKey{}, &internalGate{manager: manager, auth: auth})
		req := dispatchRequest(t, raw)
		if err := BeforeOCGHTTPDispatch(missing, req); err == nil || !errors.Is(err, ErrAttemptStop) {
			t.Fatalf("missing facts = %v", err)
		}
		admittedCtx := WithOCGCallableProtocol(context.Background(), "responses")
		stopped, err := manager.beforeGenerationSend(admittedCtx, auth, GenerationKindExecute, "gpt-5.5", "gpt-5.5")
		if err != nil {
			t.Fatal(err)
		}
		facts := generationFactsFrom(stopped)
		facts.mu.Lock()
		facts.admitted = false
		facts.mu.Unlock()
		if err := BeforeOCGHTTPDispatch(stopped, req); err == nil || !errors.Is(err, ErrAttemptStop) {
			t.Fatalf("unadmitted facts = %v", err)
		}
		facts.mu.Lock()
		consumed := facts.dispatchConsumed
		sent := facts.sent
		facts.mu.Unlock()
		if consumed || sent {
			t.Fatal("unadmitted refusal consumed a permit or performed IO")
		}
		ordinary := &Manager{}
		plain, err := ordinary.beforeGenerationSend(WithOCGCallableProtocol(context.Background(), "responses"), auth, GenerationKindExecute, "gpt-5.5", "gpt-5.5")
		if err != nil {
			t.Fatal(err)
		}
		if err := BeforeOCGHTTPDispatch(plain, req); err != nil {
			t.Fatalf("ordinary dispatch = %v", err)
		}
	})

	t.Run("native null and local count do not consume", func(t *testing.T) {
		ctx, _ := admitDispatch(t, true, GenerationKindExecute, nil)
		req := dispatchRequest(t, raw)
		if err := BeforeOCGHTTPDispatch(ctx, req); err == nil {
			t.Fatal("unpinned native send")
		}
		if generationFactsFrom(ctx).dispatchConsumed {
			t.Fatal("native denial consumed the permit")
		}
		counted, _ := admitDispatch(t, true, GenerationKindCount, nil)
		if err := BeforeOCGHTTPDispatch(counted, req); err == nil {
			t.Fatal("local count sent")
		}
		if generationFactsFrom(counted).dispatchConsumed || !generationFactsFrom(counted).localCount {
			t.Fatalf("count facts = %+v", generationFactsFrom(counted))
		}
	})

	t.Run("api null consumes once", func(t *testing.T) {
		ctx, _ := admitDispatch(t, false, GenerationKindExecute, nil)
		req := dispatchRequest(t, raw)
		if err := BeforeOCGHTTPDispatch(ctx, req); err != nil {
			t.Fatal(err)
		}
		if err := BeforeOCGHTTPDispatch(ctx, req); err == nil {
			t.Fatal("api null pin was reused")
		}
		if !generationFactsFrom(ctx).dispatchConsumed || generationFactsFrom(ctx).matched != nil {
			t.Fatalf("api facts = %+v", generationFactsFrom(ctx))
		}
	})

	t.Run("cancel and deadline do not consume", func(t *testing.T) {
		parent, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{pin})
		cancelled, cancel := context.WithCancel(parent)
		cancel()
		if err := BeforeOCGHTTPDispatch(cancelled, dispatchRequest(t, raw)); err == nil {
			t.Fatal("cancelled dispatch")
		}
		if generationFactsFrom(parent).dispatchConsumed {
			t.Fatal("cancel consumed the permit")
		}
		expired, stop := context.WithDeadline(parent, time.Now().Add(-time.Second))
		defer stop()
		if err := BeforeOCGHTTPDispatch(expired, dispatchRequest(t, raw)); err == nil {
			t.Fatal("expired dispatch")
		}
		if generationFactsFrom(parent).dispatchConsumed {
			t.Fatal("deadline consumed the permit")
		}
	})
}

func literalDispatchPin(canonical, origin string) EndpointPin {
	sum := sha256.Sum256([]byte(canonical))
	return EndpointPin{
		Protocol:            "responses",
		EndpointID:          "lb-literal",
		Origin:              origin,
		EndpointFingerprint: hex.EncodeToString(sum[:]),
		HTTPMethod:          "POST",
	}
}

func TestOCGNativeDispatchGuardCanonicalHosts(t *testing.T) {
	cases := []struct {
		name, raw, canonical, origin string
	}{
		{"ipv6 explicit 80", "http://[::1]:80/v1/responses", "http://[::1]/v1/responses", "http://[::1]"},
		{"ipv6 without port", "http://[::1]/v1/responses", "http://[::1]/v1/responses", "http://[::1]"},
		{"ipv6 explicit 443", "https://[::1]:443/v1/responses", "https://[::1]/v1/responses", "https://[::1]"},
		{"ipv6 https without port", "https://[::1]/v1/responses", "https://[::1]/v1/responses", "https://[::1]"},
		{"ipv6 nondefault port", "http://[::1]:9/v1/responses", "http://[::1]:9/v1/responses", "http://[::1]:9"},
		{"ipv4 explicit 80", "http://127.0.0.1:80/v1/responses", "http://127.0.0.1/v1/responses", "http://127.0.0.1"},
		{"ipv4 without port", "http://127.0.0.1/v1/responses", "http://127.0.0.1/v1/responses", "http://127.0.0.1"},
		{"dns explicit 443", "https://chatgpt.com:443/backend-api/codex/responses", "https://chatgpt.com/backend-api/codex/responses", "https://chatgpt.com"},
		{"dns query", "https://api.anthropic.com/v1/messages?beta=true", "https://api.anthropic.com/v1/messages?beta=true", "https://api.anthropic.com"},
	}
	for _, item := range cases {
		t.Run(item.name, func(t *testing.T) {
			canonical, origin, fingerprint, err := CanonicalDispatchURL(item.raw)
			if err != nil {
				t.Fatal(err)
			}
			if canonical != item.canonical || origin != item.origin || strings.Contains(canonical, "://::1") {
				t.Fatalf("canonical=%s origin=%s", canonical, origin)
			}
			sum := sha256.Sum256([]byte(item.canonical))
			if fingerprint != hex.EncodeToString(sum[:]) {
				t.Fatalf("fingerprint %s", fingerprint)
			}
			pin := literalDispatchPin(item.canonical, item.origin)
			ctx, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{pin})
			req, err := http.NewRequest(http.MethodPost, item.raw, nil)
			if err != nil {
				t.Fatal(err)
			}
			if err := BeforeOCGHTTPDispatch(ctx, req); err != nil {
				t.Fatal(err)
			}
			facts := generationFactsFrom(ctx)
			if facts == nil || !facts.dispatchConsumed || facts.matched == nil || facts.matched.EndpointFingerprint != pin.EndpointFingerprint || facts.matched.Origin != item.origin {
				t.Fatalf("facts=%+v", facts)
			}
			if req.Method != http.MethodPost {
				t.Fatalf("method %s", req.Method)
			}
		})
	}

	t.Run("raw path and query survive an ipv6 default port", func(t *testing.T) {
		req := &http.Request{
			Method: http.MethodPost,
			Host:   "[::1]:80",
			URL: &url.URL{
				Scheme:   "http",
				Host:     "[::1]:80",
				Path:     "/backend-api/codex/responses",
				RawPath:  "/backend-api/codex/responses",
				RawQuery: "beta=true",
			},
		}
		canonical, origin, wireHost, err := canonicalRequestTarget(req)
		if err != nil {
			t.Fatal(err)
		}
		if canonical != "http://[::1]/backend-api/codex/responses?beta=true" || origin != "http://[::1]" || wireHost != "[::1]" || !hostHeaderMatches(req.Host, req.URL.Scheme, wireHost) {
			t.Fatalf("canonical=%s origin=%s wire=%s", canonical, origin, wireHost)
		}
	})
}
