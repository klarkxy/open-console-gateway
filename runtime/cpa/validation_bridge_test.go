package main

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestParseRequestDeadlineBounds(t *testing.T) {
	now := time.Date(2026, 10, 4, 12, 0, 0, 123456789, time.UTC)
	if _, err := parseRequestDeadline(now.Format(time.RFC3339Nano), now); err == nil {
		t.Fatal("equal deadline was accepted")
	}
	parsed, err := parseRequestDeadline(now.Add(time.Minute).Format(time.RFC3339Nano), now)
	if err != nil || !parsed.Equal(now.Add(time.Minute)) {
		t.Fatalf("fractional UTC deadline = %v %v", parsed, err)
	}
	zeroOffset := now.Add(time.Minute).Format("2006-01-02T15:04:05.000000000Z")
	if _, err := parseRequestDeadline(zeroOffset, now); err != nil {
		t.Fatal(err)
	}
	zoned := now.Add(time.Minute).In(time.FixedZone("CST", 8*3600)).Format(time.RFC3339Nano)
	if _, err := parseRequestDeadline(zoned, now); err == nil {
		t.Fatal("non-UTC deadline was accepted")
	}
	if _, err := parseRequestDeadline("2026-10-04 12:01:00Z", now); err == nil {
		t.Fatal("malformed deadline was accepted")
	}
}

func TestPrivateBridgeRejectsBeforeProvider(t *testing.T) {
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		t.Errorf("rejected request reached %s", r.URL.Path)
		w.WriteHeader(http.StatusOK)
	}))
	defer upstream.Close()
	policy, _ := bridgePolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startBridgeHost(t, policy.URL, upstream.URL, upstream.URL, upstream.URL, upstream.URL, upstream.URL, upstream.URL)
	_ = waitReady(t, directClient(), host, `"policyReady":true`)
	past := time.Now().UTC().Add(-time.Second).Format(time.RFC3339Nano)
	offset := time.Now().Add(2 * time.Minute).In(time.FixedZone("CST", 8*3600)).Format(time.RFC3339Nano)
	cases := []struct {
		name, kind, pin, protocol, path, deadline, want string
		omitDeadline                                    bool
	}{
		{name: "validated only", kind: "validated", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "validated pin without protocol", kind: "validated", pin: "acct-a", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "validated protocol without pin", kind: "validated", protocol: "chat_completions", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "accepted with pin", kind: "accepted", pin: "acct-a", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "omitted kind with pin", pin: "acct-a", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "omitted kind with protocol", protocol: "chat_completions", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "accepted with protocol", kind: "accepted", protocol: "responses", path: "/v1/responses", want: "invalid_private_bridge"},
		{name: "unknown kind", kind: "execute", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "bad protocol", kind: "validated", pin: "acct-a", protocol: "websocket", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "unknown pin", kind: "validated", pin: "missing-auth", protocol: "chat_completions", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "chat path with responses protocol", kind: "validated", pin: "acct-a", protocol: "responses", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "responses path with chat protocol", kind: "validated", pin: "acct-a", protocol: "chat_completions", path: "/v1/responses", want: "invalid_private_bridge"},
		{name: "messages protocol on chat path", kind: "validated", pin: "acct-a", protocol: "messages", path: "/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "chat protocol on messages path", kind: "validated", pin: "acct-a", protocol: "chat_completions", path: "/v1/messages", want: "invalid_private_bridge"},
		{name: "alias chat path with responses protocol", kind: "validated", pin: "acct-a", protocol: "responses", path: "/openai/v1/chat/completions", want: "invalid_private_bridge"},
		{name: "alias responses path with chat protocol", kind: "validated", pin: "acct-a", protocol: "chat_completions", path: "/openai/v1/responses", want: "invalid_private_bridge"},
		{name: "missing deadline", path: "/v1/chat/completions", omitDeadline: true, want: "invalid_request_deadline"},
		{name: "expired deadline", path: "/v1/chat/completions", deadline: past, want: "invalid_request_deadline"},
		{name: "malformed deadline", path: "/v1/chat/completions", deadline: "2026-10-04 00:00:00Z", want: "invalid_request_deadline"},
		{name: "non-utc deadline", path: "/v1/chat/completions", deadline: offset, want: "invalid_request_deadline"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := postBridge(t, host, bridgeCall{
				path: tc.path, kind: tc.kind, pin: tc.pin, protocol: tc.protocol,
				deadline: tc.deadline, omitDeadline: tc.omitDeadline,
			})
			if got.status != http.StatusBadRequest || !strings.Contains(got.body, tc.want) {
				t.Fatalf("status=%d body=%s", got.status, got.body)
			}
		})
	}
	if hits.Load() != 0 {
		t.Fatalf("provider hits=%d", hits.Load())
	}
}

func TestValidatedPinFiltersProtocolAndValidationOnlyRoutes(t *testing.T) {
	var aChatHits, aRespHits, bChatHits, bRespHits, baseAHits, baseBHits atomic.Int32
	var mode atomic.Int32
	aChat := markerServer(t, &aChatHits, func(w http.ResponseWriter, _ *http.Request) {
		switch mode.Load() {
		case 1:
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusInternalServerError)
			_, _ = w.Write([]byte(`{"error":{"message":"down"}}`))
		case 2:
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte("data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"yes\"}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n"))
		case 3:
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusTooManyRequests)
			_, _ = w.Write([]byte(`{"error":{"message":"limited"}}`))
		default:
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte(`{"id":"ok","choices":[{"message":{"role":"assistant","content":"yes"}}],"usage":{"prompt_tokens":2,"completion_tokens":1}}`))
		}
	})
	defer aChat.Close()
	aResp := markerServer(t, &aRespHits, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"id":"resp","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"yes"}]}],"usage":{"input_tokens":2,"output_tokens":1}}`))
	})
	defer aResp.Close()
	bChat := markerServer(t, &bChatHits, func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	})
	defer bChat.Close()
	bResp := markerServer(t, &bRespHits, func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	})
	defer bResp.Close()
	baseA := markerServer(t, &baseAHits, func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	})
	defer baseA.Close()
	baseB := markerServer(t, &baseBHits, func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	})
	defer baseB.Close()
	policy, results := bridgePolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusTooManyRequests {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host := startBridgeHost(t, policy.URL, baseA.URL, baseB.URL, aChat.URL, aResp.URL, bChat.URL, bResp.URL)
	_ = waitReady(t, directClient(), host, "acct-a")
	assertStampedValidationOnly(t, host)

	mode.Store(1)
	beforeA, beforeN := snapshotPolicy(results)
	accepted := postBridge(t, host, bridgeCall{kind: "accepted", path: "/v1/chat/completions"})
	if aChatHits.Load() != 1 || aRespHits.Load() != 0 || bChatHits.Load() != 0 || bRespHits.Load() != 0 || baseAHits.Load() != 0 || baseBHits.Load() != 0 {
		t.Fatalf("accepted 500 escaped chat=%d resp=%d bChat=%d bResp=%d baseA=%d baseB=%d status=%d body=%s", aChatHits.Load(), aRespHits.Load(), bChatHits.Load(), bRespHits.Load(), baseAHits.Load(), baseBHits.Load(), accepted.status, accepted.body)
	}
	assertNewKinds(t, results, beforeA, beforeN, "accepted", "acct-a")

	beforeA, beforeN = snapshotPolicy(results)
	validatedResp := postBridge(t, host, bridgeCall{
		kind: "validated", pin: "acct-a", protocol: "responses", path: "/v1/responses",
		body: `{"model":"public-model","input":"hello"}`,
		extra: map[string]string{
			"X-OCG-Endpoint":           bResp.URL,
			"X-OCG-Request-Kind":       "validated",
			"X-OCG-Pinned-Auth-Id":     "acct-a",
			"X-OCG-Validated-Protocol": "responses",
		},
	})
	if aRespHits.Load() != 1 || aChatHits.Load() != 1 || bChatHits.Load() != 0 || bRespHits.Load() != 0 || baseAHits.Load() != 0 || baseBHits.Load() != 0 {
		t.Fatalf("validated responses escaped chat=%d resp=%d bChat=%d bResp=%d baseA=%d baseB=%d status=%d body=%s", aChatHits.Load(), aRespHits.Load(), bChatHits.Load(), bRespHits.Load(), baseAHits.Load(), baseBHits.Load(), validatedResp.status, validatedResp.body)
	}
	assertNewKinds(t, results, beforeA, beforeN, "validated", "acct-a")

	mode.Store(2)
	beforeA, beforeN = snapshotPolicy(results)
	streamBody := strings.TrimSuffix(chatPayload, "}") + `,"stream":true}`
	streamed := postBridge(t, host, bridgeCall{kind: "validated", pin: "acct-a", protocol: "chat_completions", path: "/v1/chat/completions", body: streamBody})
	if aChatHits.Load() != 2 || aRespHits.Load() != 1 || bChatHits.Load() != 0 || bRespHits.Load() != 0 || baseAHits.Load() != 0 || baseBHits.Load() != 0 {
		t.Fatalf("stream escaped chat=%d resp=%d bChat=%d bResp=%d baseA=%d baseB=%d status=%d body=%s", aChatHits.Load(), aRespHits.Load(), bChatHits.Load(), bRespHits.Load(), baseAHits.Load(), baseBHits.Load(), streamed.status, streamed.body)
	}
	assertNewKinds(t, results, beforeA, beforeN, "validated", "acct-a")

	alias := postBridge(t, host, bridgeCall{kind: "validated", pin: "acct-a", protocol: "chat_completions", path: "/openai/v1/chat/completions"})
	if alias.status == http.StatusBadRequest && strings.Contains(alias.body, "invalid_private_bridge") {
		t.Fatalf("openai alias path rejected: %d %s", alias.status, alias.body)
	}
	if aChatHits.Load() != 2 || aRespHits.Load() != 1 || bChatHits.Load() != 0 || bRespHits.Load() != 0 || baseAHits.Load() != 0 || baseBHits.Load() != 0 {
		t.Fatalf("alias path sent chat=%d resp=%d bChat=%d bResp=%d baseA=%d baseB=%d status=%d body=%s", aChatHits.Load(), aRespHits.Load(), bChatHits.Load(), bRespHits.Load(), baseAHits.Load(), baseBHits.Load(), alias.status, alias.body)
	}

	mode.Store(3)
	beforeA, beforeN = snapshotPolicy(results)
	limited := postBridge(t, host, bridgeCall{kind: "validated", pin: "acct-a", protocol: "chat_completions", path: "/v1/chat/completions"})
	if aChatHits.Load() != 3 || aRespHits.Load() != 1 || bChatHits.Load() != 0 || bRespHits.Load() != 0 || baseAHits.Load() != 0 || baseBHits.Load() != 0 {
		t.Fatalf("429 skip escaped chat=%d resp=%d bChat=%d bResp=%d baseA=%d baseB=%d status=%d body=%s", aChatHits.Load(), aRespHits.Load(), bChatHits.Load(), bRespHits.Load(), baseAHits.Load(), baseBHits.Load(), limited.status, limited.body)
	}
	assertNewKinds(t, results, beforeA, beforeN, "validated", "acct-a")
}

func TestHTTPDeadlineRetainsCapturePastFallback(t *testing.T) {
	entered := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	var enterOnce sync.Once
	releaseOnce := func() { once.Do(func() { close(release) }) }
	t.Cleanup(releaseOnce)
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		for key := range r.Header {
			if strings.HasPrefix(strings.ToLower(key), "x-ocg-") {
				t.Errorf("upstream saw %s", key)
			}
		}
		enterOnce.Do(func() { close(entered) })
		<-release
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"id":"ok","choices":[{"message":{"role":"assistant","content":"yes"}}],"usage":{"prompt_tokens":2,"completion_tokens":1}}`))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := twoRouteHost(t, policy.URL, upstream.URL, upstream.URL)
	deadline := time.Now().UTC().Add(3 * time.Minute)
	deadlineHeader := deadline.Format(time.RFC3339Nano)
	done := make(chan chatResult, 1)
	go func() {
		done <- postBridge(t, host, bridgeCall{path: "/v1/chat/completions", deadline: deadlineHeader})
	}()
	select {
	case <-entered:
	case <-time.After(8 * time.Second):
		t.Fatal("upstream was not reached")
	}
	var found []attemptCapture
	host.captures.Range(func(_, value any) bool {
		if item, ok := value.(attemptCapture); ok {
			found = append(found, item)
		}
		return true
	})
	if len(found) != 1 || found[0].deadline.IsZero() {
		t.Fatalf("captures=%d", len(found))
	}
	parsed, err := time.Parse(time.RFC3339Nano, deadlineHeader)
	if err != nil {
		t.Fatal(err)
	}
	if !found[0].deadline.Equal(parsed) {
		t.Fatalf("capture deadline=%s header=%s", found[0].deadline.Format(time.RFC3339Nano), deadlineHeader)
	}
	if !captureExpireAt(found[0]).Equal(found[0].deadline.Add(publicationGrace)) {
		t.Fatalf("expire=%s", captureExpireAt(found[0]).Format(time.RFC3339Nano))
	}
	fallback := found[0].storedAt.Add(idleCapture + publicationGrace)
	if !captureExpireAt(found[0]).After(fallback) {
		t.Fatal("deadline retention did not pass the 90s fallback")
	}
	nowFn = func() time.Time { return found[0].storedAt.Add(160 * time.Second) }
	t.Cleanup(func() { nowFn = time.Now })
	if !hostNow().After(fallback) {
		t.Fatal("test clock did not pass the 90s fallback")
	}
	generationBoundary{host: host}.sweepCaptures()
	if mapLen(&host.captures) != 1 {
		t.Fatal("sweep deleted a capture inside deadline plus 60s")
	}
	releaseOnce()
	var got chatResult
	select {
	case got = <-done:
	case <-time.After(8 * time.Second):
		t.Fatal("request did not finish")
	}
	if hits.Load() != 1 || got.status == 0 {
		t.Fatalf("hits=%d status=%d body=%s", hits.Load(), got.status, got.body)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 1 || results.items[0].Kind != "accepted" || results.items[0].Outcome != "success" || results.items[0].ReportedUsage == nil || !strings.Contains(string(results.items[0].ReportedUsage), `"inputTokens":2`) {
		t.Fatalf("published result = %+v status=%d body=%s", results.items, got.status, got.body)
	}
}

func TestNativeValidatedPinRefreshStaysOnPinnedAuth(t *testing.T) {
	var aHits, bHits, evilHits atomic.Int32
	var mu sync.Mutex
	var auths, paths []string
	a := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := aHits.Add(1)
		for key := range r.Header {
			if strings.HasPrefix(strings.ToLower(key), "x-ocg-") {
				t.Errorf("upstream saw %s", key)
			}
		}
		mu.Lock()
		auths = append(auths, r.Header.Get("Authorization"))
		paths = append(paths, r.URL.Path)
		mu.Unlock()
		if n == 1 {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":{"message":"unauthorized"}}`))
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer a.Close()
	b := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		bHits.Add(1)
		t.Errorf("pinned refresh reached account B %s", r.URL.Path)
		w.WriteHeader(http.StatusOK)
	}))
	defer b.Close()
	evil := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		evilHits.Add(1)
		t.Errorf("caller endpoint reached %s", r.URL.String())
		w.WriteHeader(http.StatusOK)
	}))
	defer evil.Close()
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"access-new","refresh_token":"refresh-1","id_token":"not-a-jwt","expires_in":3600}`))
	}))
	defer tokenSrv.Close()
	policy, results := bridgePolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusUnauthorized {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{file: "codex-a.json", id: "oauth-a", version: "4", priority: "10", provider: "canonical-oauth", models: []string{nativeModel}, access: "access-old", refresh: "refresh-1", baseURL: a.URL, tokenURL: tokenSrv.URL},
			{file: "codex-b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-b", baseURL: b.URL},
		},
	})
	pin := readyAuthID(t, host, "oauth-a")
	mismatch := postBridge(t, host, bridgeCall{kind: "validated", pin: pin, protocol: "responses", path: "/v1/chat/completions", body: `{"model":"` + nativeModel + `","messages":[{"role":"user","content":"hi"}]}`})
	if mismatch.status != http.StatusBadRequest || !strings.Contains(mismatch.body, "invalid_private_bridge") || aHits.Load() != 0 || bHits.Load() != 0 || evilHits.Load() != 0 {
		t.Fatalf("path mismatch status=%d body=%s hits=%d/%d/%d", mismatch.status, mismatch.body, aHits.Load(), bHits.Load(), evilHits.Load())
	}
	chatProtocol := postBridge(t, host, bridgeCall{kind: "validated", pin: pin, protocol: "chat_completions", path: "/v1/responses", body: `{"model":"` + nativeModel + `","input":"hello"}`})
	if chatProtocol.status != http.StatusBadRequest || !strings.Contains(chatProtocol.body, "invalid_private_bridge") || aHits.Load() != 0 {
		t.Fatalf("protocol mismatch status=%d body=%s hits=%d", chatProtocol.status, chatProtocol.body, aHits.Load())
	}
	beforeA, beforeN := snapshotPolicy(results)
	got := postBridge(t, host, bridgeCall{
		kind: "validated", pin: pin, protocol: "responses", path: "/v1/responses",
		body:  `{"model":"` + nativeModel + `","input":"hello"}`,
		extra: map[string]string{"X-OCG-Endpoint": evil.URL + "/v1/responses"},
	})
	if aHits.Load() != 2 || bHits.Load() != 0 || evilHits.Load() != 0 {
		t.Fatalf("refresh hits A=%d B=%d evil=%d status=%d body=%s", aHits.Load(), bHits.Load(), evilHits.Load(), got.status, got.body)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(auths) != 2 || auths[0] != "Bearer access-old" || auths[1] != "Bearer access-new" {
		t.Fatalf("authorization = %v", auths)
	}
	for _, path := range paths {
		if !strings.Contains(path, "/responses") {
			t.Fatalf("paths = %v", paths)
		}
	}
	assertNewKinds(t, results, beforeA, beforeN, "validated", pin)
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < beforeN+2 || results.items[beforeN].Outcome != "explicit_rejection" || results.items[beforeN+1].Outcome != "success" {
		t.Fatalf("refresh facts = %+v", results.items[beforeN:])
	}
}

func bridgePolicy(t *testing.T, resultAction func(policyRequest) string) (*httptest.Server, *policyLog) {
	t.Helper()
	t.Cleanup(resetNativeGrants)
	log := &policyLog{admits: map[string]policyRequest{}}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		log.mu.Lock()
		defer log.mu.Unlock()
		switch req.Operation {
		case "ready":
			if req.AuthID != "" || req.AttemptID != "" || req.RequestID != "" || req.ProcessGeneration == "" || req.ProjectionDigest == "" {
				http.Error(w, "ready", http.StatusBadRequest)
				return
			}
			writeReady(w, req)
		case "admit":
			if !canonicalUUID(req.AttemptID) || !canonicalUUID(req.RequestID) || req.ProviderID == "" || req.ProviderID == req.AuthID || req.ProjectionDigest == "" || (req.Kind != "accepted" && req.Kind != "validated") {
				http.Error(w, "admit", http.StatusBadRequest)
				return
			}
			log.admits[req.AttemptID] = req
			log.seq = append(log.seq, "admit")
			pins, pinErr := allowPins(log, req)
			if pinErr != nil {
				http.Error(w, pinErr.Error(), http.StatusBadRequest)
				return
			}
			writeDecisionPins(w, "allow", "eligible", pins)
		case "result":
			prior, ok := log.admits[req.AttemptID]
			if !ok || prior.ProjectionRevision != req.ProjectionRevision || prior.ProjectionDigest != req.ProjectionDigest || prior.ProcessGeneration != req.ProcessGeneration || prior.Kind != req.Kind {
				http.Error(w, "stale", http.StatusConflict)
				return
			}
			if req.Outcome == "explicit_rejection" && req.Observation == nil {
				http.Error(w, "observation", http.StatusBadRequest)
				return
			}
			action := resultAction(req)
			if action == "" || action == "allow" {
				http.Error(w, "code", http.StatusBadRequest)
				return
			}
			log.items = append(log.items, req)
			log.seq = append(log.seq, "result")
			writeDecision(w, action, req.Outcome)
		default:
			http.Error(w, "op", http.StatusBadRequest)
		}
	}))
	return server, log
}

type bridgeCall struct {
	path, body, kind, pin, protocol, deadline string
	omitDeadline                              bool
	extra                                     map[string]string
}

func postBridge(t *testing.T, host *Host, call bridgeCall) chatResult {
	t.Helper()
	if call.path == "" {
		call.path = "/v1/chat/completions"
	}
	if call.body == "" {
		call.body = chatPayload
	}
	req, err := http.NewRequest(http.MethodPost, host.URL()+call.path, strings.NewReader(call.body))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	req.Header.Set("X-OCG-Request-Id", newID())
	req.Header.Set("X-OCG-Process-Generation", "41")
	req.Header.Set("X-OCG-Projection-Revision", "7")
	if call.kind != "" {
		req.Header.Set("X-OCG-Request-Kind", call.kind)
	}
	if call.pin != "" {
		req.Header.Set("X-OCG-Pinned-Auth-Id", call.pin)
	}
	if call.protocol != "" {
		req.Header.Set("X-OCG-Validated-Protocol", call.protocol)
	}
	switch {
	case call.omitDeadline:
	case call.deadline != "":
		req.Header.Set("X-OCG-Request-Deadline", call.deadline)
	default:
		req.Header.Set("X-OCG-Request-Deadline", generatedRequestDeadline())
	}
	for key, value := range call.extra {
		req.Header.Set(key, value)
	}
	resp, err := directClient().Do(req)
	if err != nil {
		return chatResult{status: 0, body: err.Error()}
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return chatResult{status: resp.StatusCode, body: string(raw)}
}

func markerServer(t *testing.T, hits *atomic.Int32, respond func(http.ResponseWriter, *http.Request)) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		for key := range r.Header {
			if strings.HasPrefix(strings.ToLower(key), "x-ocg-") {
				t.Errorf("upstream saw %s", key)
			}
		}
		respond(w, r)
	}))
}

func startBridgeHost(t *testing.T, policyURL, baseA, baseB, aChat, aResp, bChat, bResp string) *Host {
	t.Helper()
	dir := testWorkDir(t)
	port := freeListenPort(t)
	body := baseConfig(port, policyURL, "fill-first", false, false, "")
	body = strings.ReplaceAll(body, "AUTHDIR", yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.Replace(body, "request-retry: 0\n", "request-retry: 0\ndisable-cooling: true\n", 1)
	body = strings.Replace(body, "openai-compatibility: []\n", "openai-compatibility:\n  - name: acct-a\n    priority: 10\n    disable-cooling: true\n    base-url: "+baseA+"/v1\n    api-key-entries:\n      - api-key: key-a\n    models:\n      - name: upstream-model\n        alias: public-model\n  - name: acct-b\n    priority: 1\n    disable-cooling: true\n    base-url: "+baseB+"/v1\n    api-key-entries:\n      - api-key: key-b\n    models:\n      - name: upstream-model\n        alias: public-model\n", 1)
	body = strings.Replace(body, "  credentials: []\n", "  credentials:\n    - namespace: acct-a\n      auth-id: acct-a\n      credential-id: cred-a\n      credential-version: \"3\"\n      binding-id: bind-a\n      material-revision: material-a\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: responses\n          endpoint: "+aResp+"/v1/responses\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n          validation-only: true\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: chat_completions\n          endpoint: "+aChat+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n    - namespace: acct-b\n      auth-id: acct-b\n      credential-id: cred-b\n      credential-version: \"3\"\n      binding-id: bind-b\n      material-revision: material-b\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: chat_completions\n          endpoint: "+bChat+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: responses\n          endpoint: "+bResp+"/v1/responses\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n", 1)
	if err := os.MkdirAll(filepath.FromSlash(absoluteAuthDir(t, dir)), 0o700); err != nil {
		t.Fatal(err)
	}
	return startConfig(t, writeConfig(t, dir, body))
}

func assertStampedValidationOnly(t *testing.T, host *Host) {
	t.Helper()
	a := compatAuth(t, host, "acct-a").Attributes["ocg_protocol_routes"]
	b := compatAuth(t, host, "acct-b").Attributes["ocg_protocol_routes"]
	if !strings.Contains(a, `"validationOnly":true`) || !strings.Contains(a, `"validationOnly":false`) || strings.Contains(a, `"validationOnly":"`) {
		t.Fatalf("acct-a routes = %s", a)
	}
	if !strings.Contains(b, `"validationOnly":false`) || strings.Contains(b, `"validationOnly":true`) || strings.Contains(b, `"validationOnly":"`) {
		t.Fatalf("acct-b routes = %s", b)
	}
	var decoded []stampedRoute
	if err := json.Unmarshal([]byte(a), &decoded); err != nil {
		t.Fatal(err)
	}
	if len(decoded) != 2 || !decoded[0].ValidationOnly || decoded[0].Protocol != "responses" || decoded[1].ValidationOnly || decoded[1].Protocol != "chat_completions" {
		t.Fatalf("decoded = %+v", decoded)
	}
}

func snapshotPolicy(log *policyLog) (map[string]struct{}, int) {
	log.mu.Lock()
	defer log.mu.Unlock()
	ids := make(map[string]struct{}, len(log.admits))
	for id := range log.admits {
		ids[id] = struct{}{}
	}
	return ids, len(log.items)
}

func assertNewKinds(t *testing.T, log *policyLog, before map[string]struct{}, beforeResults int, kind, authID string) {
	t.Helper()
	log.mu.Lock()
	defer log.mu.Unlock()
	found := 0
	for id, admit := range log.admits {
		if _, ok := before[id]; ok {
			continue
		}
		found++
		if admit.Kind != kind || admit.AuthID != authID {
			t.Fatalf("admit kind=%s auth=%s want %s %s", admit.Kind, admit.AuthID, kind, authID)
		}
	}
	if found == 0 || len(log.items) <= beforeResults {
		t.Fatalf("new admits=%d results=%d", found, len(log.items)-beforeResults)
	}
	for _, item := range log.items[beforeResults:] {
		if item.Kind != kind || item.AuthID != authID {
			t.Fatalf("result kind=%s auth=%s want %s %s", item.Kind, item.AuthID, kind, authID)
		}
	}
}

func readyAuthID(t *testing.T, host *Host, credentialID string) string {
	t.Helper()
	raw := waitReady(t, directClient(), host, credentialID)
	var doc readyDocument
	if err := json.Unmarshal([]byte(raw), &doc); err != nil {
		t.Fatal(err)
	}
	for _, ref := range doc.AuthRefs {
		if ref.CredentialID == credentialID && ref.AuthID != "" {
			return ref.AuthID
		}
	}
	t.Fatalf("ready auth %s missing: %s", credentialID, raw)
	return ""
}
