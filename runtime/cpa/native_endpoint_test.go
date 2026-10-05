package main

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/executor"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v8/sdk/translator"
)

func postPolicy(t *testing.T, host *Host, path, payload string) chatResult {
	t.Helper()
	got, err := postPolicyResult(host, path, payload)
	if err != nil {
		t.Fatal(err)
	}
	return got
}

func postPolicyResult(host *Host, path, payload string) (chatResult, error) {
	req, err := http.NewRequest(http.MethodPost, host.URL()+path, strings.NewReader(payload))
	if err != nil {
		return chatResult{}, err
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	req.Header.Set("Content-Type", "application/json")
	stampPolicyIdentity(req, newID(), "41", "7")
	resp, err := directClient().Do(req)
	if err != nil {
		return chatResult{}, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(resp.Body)
	return chatResult{status: resp.StatusCode, body: string(raw)}, err
}

func requireMatchedPin(t *testing.T, item policyRequest, rawURL, protocol string) {
	t.Helper()
	if item.EndpointPin == nil {
		t.Fatalf("missing matched pin: %+v", item)
	}
	_, _, fingerprint, err := coreauth.CanonicalDispatchURL(rawURL)
	if err != nil {
		t.Fatal(err)
	}
	pin := item.EndpointPin
	if pin.Protocol != protocol || pin.HTTPMethod != "POST" || pin.EndpointFingerprint != fingerprint || pin.EndpointID == "" || pin.Origin == "" {
		t.Fatalf("pin %+v want %s %s", pin, protocol, fingerprint)
	}
}

func soleResult(t *testing.T, log *policyLog) policyRequest {
	t.Helper()
	log.mu.Lock()
	defer log.mu.Unlock()
	if len(log.items) != 1 {
		t.Fatalf("results = %+v", log.items)
	}
	return log.items[0]
}

func TestNativeFamilySendsMatchFrozenPins(t *testing.T) {
	t.Run("codex", func(t *testing.T) {
		assertFamilyPost(t, "codex", "", "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`, "/responses", "", "responses", codexSSE, "text/event-stream")
	})
	t.Run("claude", func(t *testing.T) {
		body := `{"id":"msg","type":"message","role":"assistant","model":"claude-sonnet","stop_reason":"end_turn","content":[{"type":"text","text":"yes"}],"usage":{"input_tokens":1,"output_tokens":1}}`
		assertFamilyPost(t, "claude", "", "/v1/messages", `{"model":"claude-sonnet","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}`, "/v1/messages", "beta=true", "messages", body, "application/json")
	})
	t.Run("kimi", func(t *testing.T) {
		assertFamilyPost(t, "kimi", "", "/v1/chat/completions", `{"model":"`+nativeModel+`","messages":[{"role":"user","content":"hi"}]}`, "/v1/chat/completions", "", "chat_completions", okBody, "application/json")
	})
	t.Run("xai", func(t *testing.T) {
		assertFamilyPost(t, "xai", "false", "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`, "/responses", "", "responses", codexSSE, "text/event-stream")
	})
}

func assertFamilyPost(t *testing.T, kind, usingAPI, ingress, payload, wantPath, wantQuery, protocol, upstreamBody, contentType string) {
	t.Helper()
	var hits atomic.Int32
	var gotPath, gotQuery string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"models":{}}`))
			return
		}
		hits.Add(1)
		gotPath = r.URL.Path
		gotQuery = r.URL.RawQuery
		w.Header().Set("Content-Type", contentType)
		_, _ = w.Write([]byte(upstreamBody))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{
			file: kind + ".json", id: "oauth-" + kind, version: "4", priority: "1", provider: "canonical-oauth",
			models: []string{nativeModel, "claude-sonnet"}, access: "token-" + kind, baseURL: upstream.URL, kind: kind, usingAPI: usingAPI, project: "project-1",
		}},
	})
	got := postPolicy(t, host, ingress, payload)
	if hits.Load() != 1 || gotPath != wantPath || gotQuery != wantQuery || got.status >= 500 {
		t.Fatalf("%s hits=%d path=%s query=%s status=%d body=%s", kind, hits.Load(), gotPath, gotQuery, got.status, got.body)
	}
	target := upstream.URL + wantPath
	if wantQuery != "" {
		target += "?" + wantQuery
	}
	item := soleResult(t, results)
	if item.Outcome != "success" || item.Sent == nil || !*item.Sent {
		t.Fatalf("%s result %+v", kind, item)
	}
	requireMatchedPin(t, item, target, protocol)
	if host.unavailable.Load() {
		t.Fatal("successful native send marked the plane unavailable")
	}
}

func TestNativeAntigravityExecuteReportsChatPin(t *testing.T) {
	model := "gemini-3.7-flash-high"
	payload := []byte(`{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}`)
	opts := cliproxyexecutor.Options{SourceFormat: sdktranslator.FromString("gemini"), OriginalRequest: payload}
	var hits atomic.Int32
	var path string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !antigravityGeneration(r) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"models":{}}`))
			return
		}
		hits.Add(1)
		path = r.URL.Path
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"response":{"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"text":"yes"}]}}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4}}}`))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startAntigravity(t, policy.URL, upstream.URL, model)
	_, err := host.service.CoreAuthManager().Execute(withRequestID(context.Background(), newID()), []string{"antigravity"}, cliproxyexecutor.Request{Model: model, Payload: payload}, opts)
	if err != nil || hits.Load() != 1 || path != "/v1internal:generateContent" {
		t.Fatalf("execute err=%v hits=%d path=%s", err, hits.Load(), path)
	}
	item := soleResult(t, results)
	requireMatchedPin(t, item, upstream.URL+"/v1internal:generateContent", "chat_completions")
	results.mu.Lock()
	defer results.mu.Unlock()
	for _, admit := range results.admits {
		if admit.CallableProtocol != "chat_completions" || admit.GenerationKind != "execute" {
			t.Fatalf("admit protocol %+v", admit)
		}
	}
}

func TestNativeCodexCountStaysLocal(t *testing.T) {
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex.json", "oauth-codex", upstream.URL, nativeModel)},
	})
	payload := []byte(`{"model":"` + nativeModel + `","input":"hello"}`)
	_, err := host.service.CoreAuthManager().ExecuteCount(withRequestID(context.Background(), newID()), []string{"codex"}, cliproxyexecutor.Request{Model: nativeModel, Payload: payload}, cliproxyexecutor.Options{SourceFormat: sdktranslator.FromString("openai-response"), OriginalRequest: payload})
	if err != nil {
		t.Fatal(err)
	}
	item := soleResult(t, results)
	if hits.Load() != 0 || item.Outcome != "success" || item.Sent == nil || *item.Sent || item.EndpointPin != nil {
		t.Fatalf("local count hits=%d item=%+v", hits.Load(), item)
	}
}

func TestNativeMismatchDoesNotReplayOrStopThePlane(t *testing.T) {
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	_, origin, fingerprint, err := coreauth.CanonicalDispatchURL("http://127.0.0.1:9/responses")
	if err != nil {
		t.Fatal(err)
	}
	results.overridePins = true
	results.pins = []endpointPin{{Protocol: "responses", EndpointID: "wrong-target", Origin: origin, EndpointFingerprint: fingerprint, HTTPMethod: "POST"}}
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex.json", "oauth-codex", upstream.URL, nativeModel)},
	})
	denied := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	item := soleResult(t, results)
	if hits.Load() != 0 || item.Outcome != "local_failure" || item.EndpointPin != nil || item.Sent == nil || *item.Sent || host.unavailable.Load() {
		t.Fatalf("denial hits=%d unavailable=%v status=%d item=%+v", hits.Load(), host.unavailable.Load(), denied.status, item)
	}
	results.mu.Lock()
	results.overridePins = false
	results.pins = nil
	results.mu.Unlock()
	allowed := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	results.mu.Lock()
	published := len(results.items)
	first := policyRequest{}
	if published > 0 {
		first = results.items[0]
	}
	results.mu.Unlock()
	if hits.Load() != 1 || allowed.status >= 500 || host.unavailable.Load() || published != 2 || first.Outcome != "local_failure" || first.Sent == nil || *first.Sent || first.EndpointPin != nil {
		t.Fatalf("follow-up hits=%d status=%d body=%s unavailable=%v publications=%d first=%+v", hits.Load(), allowed.status, allowed.body, host.unavailable.Load(), published, first)
	}
}

func TestNativeNullAllowRefusesWithoutSending(t *testing.T) {
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		hits.Add(1)
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	results.overridePins = true
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex.json", "oauth-codex", upstream.URL, nativeModel)},
	})
	_ = postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	item := soleResult(t, results)
	if hits.Load() != 0 || item.Outcome != "local_failure" || item.EndpointPin != nil || host.unavailable.Load() {
		t.Fatalf("null native hits=%d unavailable=%v item=%+v", hits.Load(), host.unavailable.Load(), item)
	}
}

func TestAPINonemptyPinsStayMalformed(t *testing.T) {
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		hits.Add(1)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstream.Close()
	other := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		hits.Add(1)
	}))
	defer other.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	_, origin, fingerprint, err := coreauth.CanonicalDispatchURL(upstream.URL + "/v1/chat/completions")
	if err != nil {
		t.Fatal(err)
	}
	results.overridePins = true
	results.pins = []endpointPin{{Protocol: "chat_completions", EndpointID: "api-pin", Origin: origin, EndpointFingerprint: fingerprint, HTTPMethod: "POST"}}
	host, _ := startSample(t, policy.URL, "7", "41", upstream.URL, other.URL)
	got := chat(t, directClient(), host, "41", "7", `{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`)
	if hits.Load() != 0 || !host.unavailable.Load() || got.status < 400 {
		t.Fatalf("api pins hits=%d unavailable=%v status=%d body=%s", hits.Load(), host.unavailable.Load(), got.status, got.body)
	}
}

func TestNativeTransportLossKeepsMatchedPin(t *testing.T) {
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		w.Header().Set("Content-Length", "64")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("partial"))
		hj, ok := w.(http.Hijacker)
		if !ok {
			return
		}
		conn, _, err := hj.Hijack()
		if err == nil {
			_ = conn.Close()
		}
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex.json", "oauth-codex", upstream.URL, nativeModel)},
	})
	_ = postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	item := soleResult(t, results)
	if hits.Load() != 1 || item.Outcome != "uncertain" || item.EndpointPin == nil {
		t.Fatalf("loss hits=%d item=%+v", hits.Load(), item)
	}
	requireMatchedPin(t, item, upstream.URL+"/responses", "responses")
}

func TestNativeFrozenGrantIgnoresRegistryReset(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		once.Do(func() { close(started) })
		select {
		case <-release:
		case <-r.Context().Done():
		case <-time.After(5 * time.Second):
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex.json", "oauth-codex", upstream.URL, nativeModel)},
	})
	done := make(chan chatResult, 1)
	fail := make(chan error, 1)
	go func() {
		got, err := postPolicyResult(host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
		if err != nil {
			fail <- err
			return
		}
		done <- got
	}()
	select {
	case <-started:
	case err := <-fail:
		t.Fatal(err)
	case <-time.After(8 * time.Second):
		t.Fatal("pinned send did not reach upstream")
	}
	resetNativeGrants()
	close(release)
	select {
	case first := <-done:
		if first.status >= 500 {
			t.Fatalf("in-flight status %d body %s", first.status, first.body)
		}
	case err := <-fail:
		t.Fatal(err)
	case <-time.After(8 * time.Second):
		t.Fatal("in-flight send did not finish")
	}
	second := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	if hits.Load() != 1 || second.status < 400 {
		t.Fatalf("revoked follow-up hits=%d status=%d body=%s", hits.Load(), second.status, second.body)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 1 || results.items[0].Outcome != "success" || results.items[0].EndpointPin == nil {
		t.Fatalf("frozen in-flight result %+v", results.items)
	}
	requireMatchedPin(t, results.items[0], upstream.URL+"/responses", "responses")
}

func TestNativeQuotaSkipDoesNotReplayTheSameTarget(t *testing.T) {
	var limitedHits, nextHits atomic.Int32
	limited := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		limitedHits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"limited"}}`))
	}))
	defer limited.Close()
	next := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		nextHits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer next.Close()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusTooManyRequests {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{file: "a.json", id: "oauth-a", version: "4", priority: "10", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-a", baseURL: limited.URL, kind: "codex"},
			{file: "b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-b", baseURL: next.URL, kind: "codex"},
		},
	})
	got := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	if limitedHits.Load() != 1 || nextHits.Load() != 1 || got.status >= 500 {
		t.Fatalf("quota limited=%d next=%d status=%d body=%s", limitedHits.Load(), nextHits.Load(), got.status, got.body)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 2 || results.items[0].Outcome != "explicit_rejection" || results.items[1].Outcome != "success" || results.items[1].EndpointPin == nil {
		t.Fatalf("quota results %+v", results.items)
	}
}

func TestNativeSelfLoopPinDoesNotAuthorizeAnotherTarget(t *testing.T) {
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		hits.Add(1)
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex.json", "oauth-codex", upstream.URL, nativeModel)},
	})
	_, origin, fingerprint, err := coreauth.CanonicalDispatchURL(host.URL() + "/v1/responses")
	if err != nil {
		t.Fatal(err)
	}
	results.overridePins = true
	results.pins = []endpointPin{{Protocol: "responses", EndpointID: "self-loop", Origin: origin, EndpointFingerprint: fingerprint, HTTPMethod: "POST"}}
	_ = postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	item := soleResult(t, results)
	if hits.Load() != 0 || item.Outcome != "local_failure" || host.unavailable.Load() {
		t.Fatalf("self-loop hits=%d unavailable=%v item=%+v", hits.Load(), host.unavailable.Load(), item)
	}
}

func TestSourceNativeFactsFollowResolvedDefaults(t *testing.T) {
	const prod = "https://cloudcode-pa.googleapis.com"
	secret := "access-secret-value"
	cases := []struct {
		name string
		auth *coreauth.Auth
		want nativeFactSet
	}{
		{
			name: "codex empty attribute base",
			auth: &coreauth.Auth{Provider: "cpa", Metadata: map[string]any{"type": "codex", "base_url": "https://unused.example/codex", "access_token": secret, "ocg_oauth": true}},
			want: nativeFactSet{label: "codex", subtype: "codex", base: codexDefaultBase, authKind: "oauth"},
		},
		{
			name: "codex attribute base",
			auth: &coreauth.Auth{Provider: "codex", Attributes: map[string]string{"base_url": "https://chatgpt.com/backend-api/codex/"}, Metadata: map[string]any{"type": "codex", "auth_kind": "oauth"}},
			want: nativeFactSet{label: "codex", subtype: "codex", base: codexDefaultBase, authKind: "oauth"},
		},
		{
			name: "claude ignores metadata base",
			auth: &coreauth.Auth{Provider: "cpa", Metadata: map[string]any{"type": "claude", "base_url": "https://unused.example", "auth_kind": "oauth"}},
			want: nativeFactSet{label: "claude", subtype: "anthropic", mode: "claude", base: claudeDefaultBase, authKind: "oauth"},
		},
		{
			name: "claude attribute base and userinfo stays visible",
			auth: &coreauth.Auth{Provider: "claude", Attributes: map[string]string{"base_url": "https://user:pass@api.anthropic.com/v1"}, Metadata: map[string]any{"type": "claude", "access_token": secret}},
			want: nativeFactSet{label: "claude", subtype: "anthropic", mode: "claude", base: "https://user:pass@api.anthropic.com/v1", authKind: "oauth"},
		},
		{
			name: "xai oauth missing using_api is cli",
			auth: &coreauth.Auth{Provider: "cpa", Metadata: map[string]any{"type": "xai", "auth_kind": "oauth", "base_url": xaiDefaultAPIBase}},
			want: nativeFactSet{label: "xai", subtype: "xai", mode: "cli", base: xaiCLIChatProxyBase, authKind: "oauth"},
		},
		{
			name: "xai empty oauth base rewrites to cli proxy",
			auth: &coreauth.Auth{Provider: "xai", Attributes: map[string]string{"auth_kind": "oauth"}, Metadata: map[string]any{"type": "xai"}},
			want: nativeFactSet{label: "xai", subtype: "xai", mode: "cli", base: xaiCLIChatProxyBase, authKind: "oauth"},
		},
		{
			name: "xai api mode keeps official base",
			auth: &coreauth.Auth{Provider: "xai", Attributes: map[string]string{"using_api": "true", "auth_kind": "oauth"}, Metadata: map[string]any{"type": "xai"}},
			want: nativeFactSet{label: "xai", subtype: "xai", mode: "api", base: xaiDefaultAPIBase, authKind: "oauth"},
		},
		{
			name: "kimi bare type uses com coding base",
			auth: &coreauth.Auth{Provider: "cpa", FileName: "kimi.ai.json", Metadata: map[string]any{"type": "kimi", "access_token": secret}},
			want: nativeFactSet{label: "kimi", subtype: "kimi.com", base: kimiComCodingBase, authKind: "oauth"},
		},
		{
			name: "kimi-ai uses ai coding base",
			auth: &coreauth.Auth{Provider: "kimi", Metadata: map[string]any{"type": "kimi-ai", "refresh_token": secret}},
			want: nativeFactSet{label: "kimi-ai", subtype: "kimi.ai", base: kimiAICodingBase, authKind: "oauth"},
		},
		{
			name: "kimi attribute base wins and keeps coding v1",
			auth: &coreauth.Auth{
				Provider:   "kimi",
				Attributes: map[string]string{"base_url": "https://api.kimi.com/coding/v1/"},
				Metadata:   map[string]any{"type": "kimi", "base_url": "https://api.kimi.com/coding", "auth_kind": "oauth"},
			},
			want: nativeFactSet{label: "kimi", subtype: "kimi.com", base: "https://api.kimi.com/coding/v1", authKind: "oauth"},
		},
		{
			name: "kimi generic type keeps an explicit ai attribute domain",
			auth: &coreauth.Auth{
				Provider:   "kimi",
				FileName:   "kimi.com.json",
				Label:      "kimi.com",
				Attributes: map[string]string{"domain": "kimi.ai"},
				Metadata:   map[string]any{"type": "kimi", "access_token": secret},
			},
			want: nativeFactSet{label: "kimi", subtype: "kimi.ai", base: kimiAICodingBase, authKind: "oauth"},
		},
		{
			name: "kimi generic type keeps an explicit ai metadata domain",
			auth: &coreauth.Auth{
				Provider: "kimi",
				ID:       "kimi.com-user",
				FileName: "notes.txt",
				Label:    "Kimi",
				Metadata: map[string]any{"type": "kimi", "domain": "kimi.ai", "refresh_token": secret},
			},
			want: nativeFactSet{label: "kimi", subtype: "kimi.ai", base: kimiAICodingBase, authKind: "oauth"},
		},
		{
			name: "kimi attribute ai base beats the unused metadata com base",
			auth: &coreauth.Auth{
				Provider:   "kimi",
				Attributes: map[string]string{"base_url": "https://api.kimi.ai/coding"},
				Metadata:   map[string]any{"type": "kimi", "base_url": "https://api.kimi.com/coding/v1", "auth_kind": "oauth"},
			},
			want: nativeFactSet{label: "kimi", subtype: "kimi.ai", base: "https://api.kimi.ai/coding", authKind: "oauth"},
		},
		{
			name: "kimi ai coding v1 attribute stays visible",
			auth: &coreauth.Auth{
				Provider:   "kimi",
				Attributes: map[string]string{"base_url": "https://api.kimi.ai/coding/v1"},
				Metadata:   map[string]any{"type": "kimi", "auth_kind": "oauth"},
			},
			want: nativeFactSet{label: "kimi", subtype: "kimi.ai", base: "https://api.kimi.ai/coding/v1", authKind: "oauth"},
		},
		{
			name: "kimi custom base stays visible for a generic type",
			auth: &coreauth.Auth{
				Provider:   "kimi",
				Attributes: map[string]string{"base_url": "https://files.example/custom"},
				Metadata:   map[string]any{"type": "kimi", "auth_kind": "oauth"},
			},
			want: nativeFactSet{label: "kimi", subtype: "kimi.com", base: "https://files.example/custom", authKind: "oauth"},
		},
		{
			name: "kimi unsupported type keeps a custom base and an empty subtype",
			auth: &coreauth.Auth{
				Provider:   "kimi",
				FileName:   "kimi.ai.json",
				Label:      "kimi.ai",
				Attributes: map[string]string{"base_url": "https://files.example/custom"},
				Metadata:   map[string]any{"type": "moonshot", "access_token": secret},
			},
			want: nativeFactSet{label: "moonshot", base: "https://files.example/custom", authKind: "oauth"},
		},
		{
			name: "kimi conflicting host leaves subtype empty",
			auth: &coreauth.Auth{
				Provider:   "kimi",
				Attributes: map[string]string{"base_url": "https://api.kimi.com/coding/v1"},
				Metadata:   map[string]any{"type": "kimi-ai", "auth_kind": "oauth"},
			},
			want: nativeFactSet{label: "kimi-ai", base: "https://api.kimi.com/coding/v1", authKind: "oauth"},
		},
		{
			name: "kimi provider label alone is unproven",
			auth: &coreauth.Auth{Provider: "kimi", FileName: "notes.txt", Metadata: map[string]any{"access_token": secret}},
			want: nativeFactSet{label: "kimi", authKind: "oauth"},
		},
		{
			name: "antigravity empty base is daily generation",
			auth: &coreauth.Auth{Provider: "cpa", Metadata: map[string]any{"type": "antigravity", "auth_kind": "oauth"}},
			want: nativeFactSet{label: "antigravity", subtype: "antigravity", mode: "daily", base: antigravityDailyBase, authKind: "oauth"},
		},
		{
			name: "antigravity custom base is not rewritten to daily",
			auth: &coreauth.Auth{Provider: "antigravity", Attributes: map[string]string{"base_url": prod}, Metadata: map[string]any{"type": "antigravity", "base_url": antigravityDailyBase, "access_token": secret}},
			want: nativeFactSet{label: "antigravity", subtype: "antigravity", mode: "custom", base: prod, authKind: "oauth"},
		},
		{
			name: "product cpa without a native type",
			auth: &coreauth.Auth{Provider: "cpa", Metadata: map[string]any{"ocg_oauth": true, "access_token": secret}},
			want: nativeFactSet{},
		},
		{
			name: "oauth bool alone is not auth kind",
			auth: &coreauth.Auth{Provider: "codex", Metadata: map[string]any{"type": "codex", "ocg_oauth": true}},
			want: nativeFactSet{label: "codex", subtype: "codex", base: codexDefaultBase},
		},
	}
	for _, item := range cases {
		t.Run(item.name, func(t *testing.T) {
			got := nativeEffectiveFacts(item.auth)
			if got != item.want {
				t.Fatalf("facts = %+v want %+v", got, item.want)
			}
			blob := got.label + got.subtype + got.mode + got.base + got.authKind
			if strings.Contains(blob, secret) || got.base == prod && item.want.base != prod {
				t.Fatalf("facts leaked or used management prod: %+v", got)
			}
		})
	}
	ref := authRef{AuthID: "api", Provider: "cpa", ProviderID: "cpa"}
	raw, err := json.Marshal(ref)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(raw), "effective") || strings.Contains(string(raw), "rawProvider") {
		t.Fatalf("api ref emitted native facts: %s", raw)
	}
}
