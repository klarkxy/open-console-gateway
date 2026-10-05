package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"
	"unsafe"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

const (
	nativeModel = "gpt-5.5"
	otherModel  = "gpt-6-sol"
	codexSSE    = "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"yes\"}]}}\n\n" +
		"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
)

func TestNativeRefreshResendAndFacts(t *testing.T) {
	var sends atomic.Int32
	var auths []string
	var paths []string
	var mu sync.Mutex
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := sends.Add(1)
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
	defer upstream.Close()
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"access-new","refresh_token":"refresh-1","id_token":"not-a-jwt","expires_in":3600}`))
	}))
	defer tokenSrv.Close()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusUnauthorized {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{
			file: "codex-a.json", id: "oauth-a", version: "4", priority: "1", provider: "canonical-oauth",
			models: []string{nativeModel}, access: "access-old", refresh: "refresh-1", baseURL: upstream.URL, tokenURL: tokenSrv.URL,
			expired: time.Now().Add(48 * time.Hour).UTC().Format(time.RFC3339),
		}},
	})
	got := responses(t, host, "41", "7", nativeModel, "")
	if sends.Load() != 2 {
		t.Fatalf("refresh resend sends=%d status=%d body=%s paths=%v", sends.Load(), got.status, got.body, paths)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(auths) != 2 || auths[0] != "Bearer access-old" || auths[1] != "Bearer access-new" {
		t.Fatalf("authorization = %v", auths)
	}
	for _, path := range paths {
		if !strings.Contains(path, "/responses") {
			t.Fatalf("path = %v", paths)
		}
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 2 || results.items[0].CredentialVersion != "4" || results.items[1].CredentialVersion != "4" {
		t.Fatalf("credential version moved = %+v", results.items)
	}
	if results.items[0].MaterialRevision == "" || results.items[0].MaterialRevision == results.items[1].MaterialRevision {
		t.Fatalf("material was not refreshed = %+v", results.items)
	}
	if results.items[0].Outcome != "explicit_rejection" || results.items[0].Observation == nil || results.items[1].Outcome != "success" {
		t.Fatalf("facts = %+v", results.items)
	}
	if results.items[1].ReportedUsage == nil || !strings.Contains(string(results.items[1].ReportedUsage), `"inputTokens":2`) {
		t.Fatalf("usage = %s", results.items[1].ReportedUsage)
	}
}

func TestNative429BodyLossAndStreamFacts(t *testing.T) {
	var mu sync.Mutex
	var order []string
	note := func(name string, r *http.Request) {
		mu.Lock()
		order = append(order, name+":"+r.URL.Path)
		mu.Unlock()
	}
	a := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		note("A", r)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"limited"}}`))
	}))
	defer a.Close()
	b := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		note("B", r)
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer b.Close()
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
			{file: "codex-a.json", id: "oauth-a", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-a", refresh: "refresh-a", baseURL: a.URL},
			{file: "codex-b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-b", refresh: "refresh-b", baseURL: b.URL},
		},
	})
	got := responses(t, host, "41", "7", nativeModel, "")
	mu.Lock()
	if len(order) != 2 || !strings.HasPrefix(order[0], "A:") || !strings.HasPrefix(order[1], "B:") {
		t.Fatalf("429 order = %v status=%d body=%s", order, got.status, got.body)
	}
	mu.Unlock()
	results.mu.Lock()
	if len(results.items) < 2 || results.items[0].Outcome != "explicit_rejection" || results.items[1].Outcome != "success" || results.items[1].StreamStarted == nil || !*results.items[1].StreamStarted || results.items[1].BodyComplete == nil || !*results.items[1].BodyComplete {
		t.Fatalf("429 facts = %+v", results.items)
	}
	results.mu.Unlock()

	lost := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hj, ok := w.(http.Hijacker)
		if !ok {
			http.Error(w, "hijack", http.StatusInternalServerError)
			return
		}
		conn, rw, err := hj.Hijack()
		if err != nil {
			return
		}
		_, _ = rw.WriteString("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
		_ = rw.Flush()
		_ = conn.Close()
	}))
	defer lost.Close()
	policy2, results2 := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy2.Close()
	host2 := startNative(t, policy2.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{file: "codex-a.json", id: "oauth-a", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-a", refresh: "refresh-a", baseURL: lost.URL}},
	})
	_ = responses(t, host2, "41", "7", nativeModel, "")
	results2.mu.Lock()
	defer results2.mu.Unlock()
	if len(results2.items) != 1 || results2.items[0].Outcome != "uncertain" || results2.items[0].ErrorCode == "" {
		t.Fatalf("body loss = %+v", results2.items)
	}
}

func TestNativeStreamEdgesAndCancellation(t *testing.T) {
	cases := []struct {
		name    string
		write   func(http.ResponseWriter)
		outcome string
		code    string
	}{
		{name: "empty", write: func(w http.ResponseWriter) {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
		}, outcome: "uncertain"},
		{name: "truncated", write: func(w http.ResponseWriter) {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte("data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n"))
		}, outcome: "uncertain"},
		{name: "post-output", write: func(w http.ResponseWriter) {
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte("data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\ndata: {\"type\":\"error\",\"error\":{\"message\":\"late\"}}\n\n"))
		}, outcome: "uncertain"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { tc.write(w) }))
			defer upstream.Close()
			policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
			defer policy.Close()
			host := startNative(t, policy.URL, nativeSpec{strategy: "fill-first", bindings: []nativeBinding{binding("codex-a.json", "oauth-a", upstream.URL, nativeModel)}})
			_ = responses(t, host, "41", "7", nativeModel, "")
			results.mu.Lock()
			defer results.mu.Unlock()
			if len(results.items) != 1 || results.items[0].Outcome != tc.outcome {
				t.Fatalf("%s facts = %+v", tc.name, results.items)
			}
		})
	}

	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n"))
		if flusher, ok := w.(http.Flusher); ok {
			flusher.Flush()
		}
		time.Sleep(2 * time.Second)
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{strategy: "fill-first", bindings: []nativeBinding{binding("codex-a.json", "oauth-a", upstream.URL, nativeModel)}})
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(100 * time.Millisecond)
		cancel()
	}()
	_ = responsesContext(t, host, ctx, "41", "7", nativeModel, "")
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		results.mu.Lock()
		ready := len(results.items) > 0
		results.mu.Unlock()
		if ready {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 1 || (results.items[0].Outcome != "cancelled" && results.items[0].Outcome != "deadline") {
		t.Fatalf("cancel facts = %+v", results.items)
	}
	if host.unavailable.Load() {
		t.Fatal("caller cancellation marked execution unavailable")
	}
}

func TestMixedRankRoundRobinScopeAndProxy(t *testing.T) {
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	assertChosen(t, policy.URL, "5", 1, "native")
	assertChosen(t, policy.URL, "1", 5, "api")
	assertRoundRobin(t, policy.URL)
	assertScopeAndDisabled(t, policy.URL)
	assertProxyLegs(t, policy.URL)
}

func TestStickyGlobalAndConversation(t *testing.T) {
	var mu sync.Mutex
	var tokens []string
	upstream := func(token string) *httptest.Server {
		return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			mu.Lock()
			tokens = append(tokens, token+":"+r.URL.Path)
			mu.Unlock()
			body := readPeek(r)
			model := jsonModel(body)
			limitA := token == "A" && (model == nativeModel || (model == "" && strings.Contains(body, nativeModel) && !strings.Contains(body, otherModel)))
			if limitA {
				w.Header().Set("Content-Type", "application/json")
				w.WriteHeader(http.StatusTooManyRequests)
				_, _ = w.Write([]byte(`{"error":{"message":"limited"}}`))
				return
			}
			w.Header().Set("Content-Type", "text/event-stream")
			_, _ = w.Write([]byte(codexSSE))
		}))
	}
	a := upstream("A")
	defer a.Close()
	b := upstream("B")
	defer b.Close()
	policy, _ := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusTooManyRequests {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		sticky:   true,
		bindings: []nativeBinding{
			{file: "codex-a.json", id: "oauth-a", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel, otherModel}, access: "token-a", refresh: "refresh-a", baseURL: a.URL},
			{file: "codex-b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel, otherModel}, access: "token-b", refresh: "refresh-b", baseURL: b.URL},
		},
	})
	_ = responses(t, host, "41", "7", nativeModel, "")
	mu.Lock()
	tokens = nil
	mu.Unlock()
	_ = responses(t, host, "41", "7", otherModel, "")
	mu.Lock()
	if len(tokens) == 0 || !strings.HasPrefix(tokens[0], "B:") {
		t.Fatalf("global slot did not retain B for the other model: %v", tokens)
	}
	mu.Unlock()

	convA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		tokens = append(tokens, "A")
		mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer convA.Close()
	convB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		tokens = append(tokens, "B")
		mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer convB.Close()
	conv := startNative(t, policy.URL, nativeSpec{
		strategy:     "round-robin",
		conversation: true,
		bindings: []nativeBinding{
			{file: "codex-a.json", id: "oauth-a", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-a", refresh: "refresh-a", baseURL: convA.URL},
			{file: "codex-b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth", models: []string{nativeModel}, access: "token-b", refresh: "refresh-b", baseURL: convB.URL},
		},
	})
	mu.Lock()
	tokens = nil
	mu.Unlock()
	_ = responses(t, conv, "41", "7", nativeModel, "conversation-x")
	_ = responses(t, conv, "41", "7", nativeModel, "conversation-x")
	_ = responses(t, conv, "41", "7", nativeModel, "conversation-y")
	_ = responses(t, conv, "41", "7", nativeModel, "conversation-y")
	mu.Lock()
	defer mu.Unlock()
	if strings.Join(tokens, ",") != "A,A,B,B" && strings.Join(tokens, ",") != "B,B,A,A" {
		t.Fatalf("conversation affinity = %v", tokens)
	}
}

func TestOAuthDiscoveryBlankKeyAndCatalogue(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "" || r.Header.Get("x-api-key") != "" {
			t.Errorf("blank route sent credentials: auth=%q x-api-key=%q", r.Header.Get("Authorization"), r.Header.Get("x-api-key"))
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstream.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	var discoveryHits atomic.Int32
	var discoveryAuth string
	var discoveryMu sync.Mutex
	discovery := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		discoveryHits.Add(1)
		discoveryMu.Lock()
		discoveryAuth = r.Header.Get("Authorization")
		discoveryMu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer discovery.Close()
	dir := testWorkDir(t)
	authDir := absoluteAuthDir(t, dir)
	if err := os.MkdirAll(filepath.FromSlash(authDir), 0o700); err != nil {
		t.Fatal(err)
	}
	authFile := `{"type":"codex","access_token":"secret-access","plan_type":"pro","expired":"` + time.Now().Add(48*time.Hour).UTC().Format(time.RFC3339) + `","base_url":"` + discovery.URL + `"}`
	if err := os.WriteFile(filepath.Join(filepath.FromSlash(authDir), "codex-a.json"), []byte(authFile), 0o600); err != nil {
		t.Fatal(err)
	}
	port := freeListenPort(t)
	body := baseConfig(port, policy.URL, "fill-first", false, false, "")
	body = strings.ReplaceAll(body, "AUTHDIR", yamlQuote(authDir))
	configPath := writeConfig(t, dir, body)
	host, err := Start(t.Context(), configPath)
	if err != nil {
		t.Fatalf("oauth-only start: %v", err)
	}
	t.Cleanup(host.Close)
	readyBody := waitReady(t, directClient(), host, "codex-a.json")
	if strings.Contains(readyBody, "secret-access") || strings.Contains(readyBody, "secret-refresh") || strings.Contains(readyBody, "hop-secret") {
		t.Fatal("ready exposed token material")
	}
	var ready readyDocument
	if err := json.Unmarshal([]byte(readyBody), &ready); err != nil {
		t.Fatal(err)
	}
	found := false
	for _, ref := range ready.AuthRefs {
		if strings.Contains(ref.RelativePath, "codex-a.json") && !ref.Mapped && len(ref.Models) > 0 && ref.Upstream != "" && ref.RegistrationEpoch != "" {
			found = true
			if _, err := strconv.ParseUint(ref.RegistrationEpoch, 10, 64); err != nil {
				t.Fatalf("registration epoch = %q", ref.RegistrationEpoch)
			}
		}
	}
	if !found {
		t.Fatalf("unmapped discovery = %+v", ready.AuthRefs)
	}
	bound := strings.Replace(body, "  oauth-bindings: []\n", "  oauth-bindings:\n    - relative-path: codex-a.json\n      credential-id: oauth-a\n      credential-version: \"4\"\n      priority: \"1\"\n      provider-id: canonical-oauth\n      models: [\""+nativeModel+"\"]\n", 1)
	replaceConfig(t, configPath, bound)
	waitReady(t, directClient(), host, `"mapped":true`)
	noteNativeGrant("codex", discovery.URL)
	gotNative := responses(t, host, "41", "7", nativeModel, "")
	if discoveryHits.Load() != 1 {
		t.Fatalf("mapped native send hits=%d status=%d body=%s", discoveryHits.Load(), gotNative.status, gotNative.body)
	}
	discoveryMu.Lock()
	if discoveryAuth != "Bearer secret-access" {
		t.Fatalf("mapped native authorization = %q", discoveryAuth)
	}
	discoveryMu.Unlock()

	blankDir := testWorkDir(t)
	blankPort := freeListenPort(t)
	blank := strings.ReplaceAll(baseConfig(blankPort, policy.URL, "fill-first", false, false, ""), "AUTHDIR", yamlQuote(absoluteAuthDir(t, blankDir)))
	blank = strings.Replace(blank, "openai-compatibility: []\n", "openai-compatibility:\n  - name: none-ns\n    priority: 1\n    base-url: "+upstream.URL+"/v1\n    api-key-entries:\n      - api-key: \"\"\n    models:\n      - name: upstream-model\n        alias: public-model\n", 1)
	blank = strings.Replace(blank, "  credentials: []\n", `  credentials:
    - namespace: none-ns
      auth-id: none-ns
      credential-id: cred-none
      credential-version: "3"
      binding-id: bind-none
      material-revision: material-none
      provider-id: canonical-provider
      opaque-remote: false
      routes:
        - public-model: public-model
          upstream-model: upstream-model
          protocol: chat_completions
          endpoint: `+upstream.URL+`/v1/chat/completions
          auth-scheme: none
          request-identity: none
          wire: none
`, 1)
	blankHost := startConfig(t, writeConfig(t, blankDir, blank))
	got := chat(t, directClient(), blankHost, "41", "7", chatPayload)
	if got.status >= 500 {
		t.Fatalf("blank all-none status=%d body=%s", got.status, got.body)
	}
	bad := strings.Replace(blank, "auth-scheme: none", "auth-scheme: bearer", 1)
	if _, err := Start(t.Context(), writeConfig(t, testWorkDir(t), bad)); err == nil {
		t.Fatal("blank bearer key started")
	}

	req, err := http.NewRequest(http.MethodGet, blankHost.URL()+"/v1/models", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	resp, err := directClient().Do(req)
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("catalogue = %d %s", resp.StatusCode, raw)
	}
	badHop, err := http.NewRequest(http.MethodGet, blankHost.URL()+"/v1/models", nil)
	if err != nil {
		t.Fatal(err)
	}
	badHop.Header.Set("Authorization", "Bearer wrong")
	badResp, err := directClient().Do(badHop)
	if err != nil {
		t.Fatal(err)
	}
	_ = badResp.Body.Close()
	if badResp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("bad hop = %d", badResp.StatusCode)
	}
	gen, err := http.NewRequest(http.MethodPost, blankHost.URL()+"/v1beta/models/gemini-test:generateContent", strings.NewReader(`{"contents":[]}`))
	if err != nil {
		t.Fatal(err)
	}
	gen.Header.Set("Authorization", "Bearer hop-secret")
	genResp, err := directClient().Do(gen)
	if err != nil {
		t.Fatal(err)
	}
	genRaw, _ := io.ReadAll(genResp.Body)
	_ = genResp.Body.Close()
	if genResp.StatusCode == http.StatusOK || !strings.Contains(string(genRaw), "missing_policy_identity") {
		t.Fatalf("gemini gate = %d %s", genResp.StatusCode, genRaw)
	}
}

func TestCaptureGraceAndPublicationFailure(t *testing.T) {
	started := time.Date(2026, 10, 4, 12, 0, 0, 0, time.UTC)
	nowFn = func() time.Time { return started }
	t.Cleanup(func() { nowFn = time.Now })
	var results []policyRequest
	var mu sync.Mutex
	policy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		if req.Operation == "admit" {
			writeDecision(w, "allow", "eligible")
			return
		}
		if req.Operation == "result" {
			mu.Lock()
			results = append(results, req)
			mu.Unlock()
		}
		writeDecision(w, "stop", "recorded")
	}))
	defer policy.Close()
	host := &Host{policyURL: policy.URL, policyToken: "policy-secret", policyOrigin: "http://127.0.0.1", policyClient: newPolicyClient(), processGeneration: "41"}
	host.appliedRevision.Store("7")
	host.appliedDigest.Store(strings.Repeat("ab", 32))
	host.policyReady.Store(true)
	auth := &coreauth.Auth{ID: "auth-ns", Attributes: map[string]string{"ocg_provider_id": "canonical-provider"}, Metadata: map[string]any{"ocg_credential_id": "cred-1", "ocg_credential_version": "3", "ocg_material_revision": "material-1"}}
	boundary := generationBoundary{host: host}
	ctxA, cancelA := context.WithDeadline(withTestProtocol(withRequestID(context.Background(), "00000000-0000-4000-8000-0000000000a1")), started.Add(2*time.Hour))
	defer cancelA()
	first, err := boundary.BeforeSend(ctxA, auth, "execute", "upstream", "public")
	if err != nil {
		t.Fatal(err)
	}
	started = started.Add(91 * time.Second)
	ctxB := withTestProtocol(withRequestID(context.Background(), "00000000-0000-4000-8000-0000000000b2"))
	if _, err := boundary.BeforeSend(ctxB, auth, "execute", "upstream", "public"); err != nil {
		t.Fatal(err)
	}
	resultCtx := coreauth.WithAttemptContext(ctxA, first.AttemptID)
	coreauth.NoteGenerationSent(resultCtx)
	coreauth.NoteGenerationStatus(resultCtx, http.StatusUnauthorized, nil, []byte(`{"error":"unauthorized"}`))
	coreauth.NoteGenerationBodyComplete(resultCtx)
	if err := boundary.AfterResult(resultCtx, auth, "execute", "upstream", "public", &coreauth.Result{AuthID: auth.ID}); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	if len(results) == 0 || results[0].AttemptID != first.AttemptID || results[0].Outcome != "explicit_rejection" {
		t.Fatalf("A was swept = %+v", results)
	}
	mu.Unlock()
	for i := 0; i < 200; i++ {
		id := "00000000-0000-4000-8000-" + leftPad(strconv.Itoa(i), 12)
		decision, err := boundary.BeforeSend(withTestProtocol(withRequestID(context.Background(), id)), auth, "execute", "upstream", "public")
		if err != nil {
			t.Fatal(err)
		}
		ctx := coreauth.WithAttemptContext(context.Background(), decision.AttemptID)
		coreauth.NoteGenerationSent(ctx)
		coreauth.NoteGenerationStatus(ctx, 200, nil, []byte(`{"ok":true}`))
		coreauth.NoteGenerationBodyComplete(ctx)
		_ = boundary.AfterResult(ctx, auth, "execute", "upstream", "public", &coreauth.Result{AuthID: auth.ID, Success: true})
	}
	if n := mapLen(&host.published); n > duplicateTombstoneCap {
		t.Fatalf("published tombstones = %d", n)
	}
	if err := boundary.AfterResult(resultCtx, auth, "execute", "upstream", "public", &coreauth.Result{AuthID: auth.ID}); err == nil {
		t.Fatal("duplicate or swept result was accepted")
	}

	failPolicy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil || req.Operation == "result" {
			http.Error(w, "down", http.StatusInternalServerError)
			return
		}
		writeDecision(w, "allow", "eligible")
	}))
	defer failPolicy.Close()
	host.policyURL = failPolicy.URL
	host.unavailable.Store(false)
	ctx := withTestProtocol(withRequestID(context.Background(), "00000000-0000-4000-8000-0000000000c3"))
	decision, err := boundary.BeforeSend(ctx, auth, "execute", "upstream", "public")
	if err != nil {
		t.Fatal(err)
	}
	cancelled := coreauth.WithCallerErr(coreauth.WithAttemptContext(ctx, decision.AttemptID), context.Canceled)
	coreauth.NoteGenerationSent(cancelled)
	coreauth.NoteGenerationStatus(cancelled, http.StatusUnauthorized, nil, []byte(`{"error":"unauthorized"}`))
	coreauth.NoteGenerationBodyComplete(cancelled)
	_ = boundary.AfterResult(cancelled, auth, "execute", "upstream", "public", &coreauth.Result{AuthID: auth.ID})
	if !host.unavailable.Load() {
		t.Fatal("publication failure after cancellation stayed available")
	}
	if _, err := boundary.BeforeSend(withRequestID(context.Background(), "00000000-0000-4000-8000-0000000000d4"), auth, "execute", "upstream", "public"); err == nil {
		t.Fatal("later admission ran while unavailable")
	}
}

func TestNativeDeadlineStops(t *testing.T) {
	var sends atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sends.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		if flusher, ok := w.(http.Flusher); ok {
			_, _ = w.Write([]byte("data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n"))
			flusher.Flush()
		}
		time.Sleep(2 * time.Second)
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{strategy: "fill-first", bindings: []nativeBinding{binding("codex-a.json", "oauth-a", upstream.URL, nativeModel)}})
	ctx, cancel := context.WithTimeout(context.Background(), 80*time.Millisecond)
	defer cancel()
	_ = responsesContext(t, host, ctx, "41", "7", nativeModel, "")
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		results.mu.Lock()
		ready := len(results.items) > 0
		results.mu.Unlock()
		if ready {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 1 || (results.items[0].Outcome != "deadline" && results.items[0].Outcome != "cancelled") {
		t.Fatalf("deadline facts = %+v sends=%d", results.items, sends.Load())
	}
	if sends.Load() != 1 {
		t.Fatalf("deadline replayed sends=%d", sends.Load())
	}
	if host.unavailable.Load() {
		t.Fatal("deadline marked execution unavailable")
	}
}

func TestDelayedStreamRoutePublication(t *testing.T) {
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusTooManyRequests {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	var firstHits, secondHits atomic.Int32
	first := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		firstHits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"limited"}}`))
	}))
	defer first.Close()
	second := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		secondHits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		flusher, _ := w.(http.Flusher)
		_, _ = w.Write([]byte("data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n"))
		if flusher != nil {
			flusher.Flush()
		}
		time.Sleep(200 * time.Millisecond)
		_, _ = w.Write([]byte("data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n"))
	}))
	defer second.Close()
	host := twoRouteHost(t, policy.URL, first.URL, second.URL)
	got := streamChat(t, directClient(), host, chatPayload)
	if firstHits.Load() != 1 || secondHits.Load() != 1 {
		t.Fatalf("route hits first=%d second=%d status=%d body=%s", firstHits.Load(), secondHits.Load(), got.status, got.body)
	}
	results.mu.Lock()
	if len(results.items) != 2 || len(results.admits) != 2 {
		t.Fatalf("delayed publication admits=%d results=%+v", len(results.admits), results.items)
	}
	if results.items[0].Outcome != "explicit_rejection" || results.items[1].Outcome != "success" || results.items[1].ReportedUsage == nil || !strings.Contains(string(results.items[1].ReportedUsage), `"inputTokens":2`) {
		t.Fatalf("delayed facts = %+v", results.items)
	}
	results.mu.Unlock()

	var truncatedHits atomic.Int32
	truncated := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		truncatedHits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"}}]}\n\n"))
	}))
	defer truncated.Close()
	policy2, results2 := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusTooManyRequests {
			return "skip"
		}
		return "stop"
	})
	defer policy2.Close()
	limited := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"limited"}}`))
	}))
	defer limited.Close()
	host2 := twoRouteHost(t, policy2.URL, limited.URL, truncated.URL)
	_ = streamChat(t, directClient(), host2, chatPayload)
	results2.mu.Lock()
	defer results2.mu.Unlock()
	if truncatedHits.Load() != 1 || len(results2.items) != 2 || results2.items[1].Outcome != "uncertain" {
		t.Fatalf("truncated replay hits=%d facts=%+v", truncatedHits.Load(), results2.items)
	}
}

func twoRouteHost(t *testing.T, policyURL, firstURL, secondURL string) *Host {
	t.Helper()
	dir := testWorkDir(t)
	port := freeListenPort(t)
	body := strings.ReplaceAll(baseConfig(port, policyURL, "fill-first", false, false, ""), "AUTHDIR", yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.Replace(body, "openai-compatibility: []\n", "openai-compatibility:\n  - name: routed\n    priority: 1\n    base-url: "+firstURL+"/v1\n    api-key-entries:\n      - api-key: route-key\n    models:\n      - name: upstream-model\n        alias: public-model\n", 1)
	body = strings.Replace(body, "  credentials: []\n", "  credentials:\n    - namespace: routed\n      auth-id: routed\n      credential-id: cred-routed\n      credential-version: \"3\"\n      binding-id: bind-routed\n      material-revision: material-routed\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: chat_completions\n          endpoint: "+firstURL+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: chat_completions\n          endpoint: "+secondURL+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n", 1)
	if err := os.MkdirAll(filepath.FromSlash(absoluteAuthDir(t, dir)), 0o700); err != nil {
		t.Fatal(err)
	}
	return startConfig(t, writeConfig(t, dir, body))
}

type nativeBinding struct {
	file, id, version, priority, provider, access, refresh, baseURL, tokenURL, filePriority, expired, kind, project, usingAPI string
	models                                                                                                                    []string
	disabled                                                                                                                  bool
	domain                                                                                                                    string
	headers                                                                                                                   map[string]string
}

type apiSpec struct {
	name, key, baseURL, model string
	priority                  int
}

type nativeSpec struct {
	strategy, proxyURL string
	sticky             bool
	conversation       bool
	bindings           []nativeBinding
	api                *apiSpec
	proxyList          string
}

func binding(file, id, baseURL, models string) nativeBinding {
	return nativeBinding{file: file, id: id, version: "4", priority: "1", provider: "canonical-oauth", models: strings.Split(models, ","), access: "token-" + id, refresh: "refresh-" + id, baseURL: baseURL}
}

func startNative(t *testing.T, policyURL string, spec nativeSpec) *Host {
	t.Helper()
	dir := testWorkDir(t)
	authDir := absoluteAuthDir(t, dir)
	if err := os.MkdirAll(filepath.FromSlash(authDir), 0o700); err != nil {
		t.Fatal(err)
	}
	port := freeListenPort(t)
	body := baseConfig(port, policyURL, spec.strategy, spec.sticky, spec.conversation, spec.proxyURL)
	body = strings.ReplaceAll(body, "AUTHDIR", yamlQuote(authDir))
	if spec.proxyList != "" {
		body = strings.Replace(body, "  credentials:", spec.proxyList+"  credentials:", 1)
	}
	var bindings strings.Builder
	bindings.WriteString("  oauth-bindings:\n")
	for _, item := range spec.bindings {
		kind := item.kind
		if kind == "" {
			kind = "codex"
		}
		payload := map[string]any{"type": kind, "access_token": item.access, "plan_type": "pro", "base_url": item.baseURL, "expired": item.expired}
		if item.expired == "" {
			payload["expired"] = time.Now().Add(48 * time.Hour).UTC().Format(time.RFC3339)
		}
		if item.project != "" {
			payload["project_id"] = item.project
		}
		if item.refresh != "" && item.tokenURL != "" {
			payload["refresh_token"] = item.refresh
			payload["token_url"] = item.tokenURL
		}
		if item.usingAPI == "true" || item.usingAPI == "false" {
			payload["using_api"] = item.usingAPI == "true"
		}
		if item.filePriority != "" {
			payload["priority"] = item.filePriority
		}
		if item.disabled {
			payload["disabled"] = true
		}
		if item.domain != "" {
			payload["domain"] = item.domain
		}
		if len(item.headers) > 0 {
			payload["headers"] = item.headers
		}
		raw, err := json.Marshal(payload)
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(filepath.FromSlash(authDir), item.file), raw, 0o600); err != nil {
			t.Fatal(err)
		}
		noteNativeGrant(kind, item.baseURL)
		version := item.version
		if version == "" {
			version = "4"
		}
		bindings.WriteString("    - relative-path: " + item.file + "\n")
		bindings.WriteString("      credential-id: " + item.id + "\n")
		bindings.WriteString("      credential-version: \"" + version + "\"\n")
		bindings.WriteString("      priority: \"" + item.priority + "\"\n")
		bindings.WriteString("      provider-id: " + item.provider + "\n")
		bindings.WriteString("      models: [" + quoteList(item.models) + "]\n")
	}
	body = strings.Replace(body, "  oauth-bindings: []\n", bindings.String(), 1)
	if spec.api != nil {
		body = strings.Replace(body, "openai-compatibility: []\n", "openai-compatibility:\n  - name: "+spec.api.name+"\n    priority: "+strconv.Itoa(spec.api.priority)+"\n    base-url: "+spec.api.baseURL+"/v1\n    api-key-entries:\n      - api-key: "+spec.api.key+"\n    models:\n      - name: "+spec.api.model+"\n        alias: "+spec.api.model+"\n", 1)
		body = strings.Replace(body, "  credentials: []\n", "  credentials:\n    - namespace: "+spec.api.name+"\n      auth-id: "+spec.api.name+"\n      credential-id: cred-api\n      credential-version: \"3\"\n      binding-id: bind-api\n      material-revision: material-api\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: "+spec.api.model+"\n          upstream-model: "+spec.api.model+"\n          protocol: chat_completions\n          endpoint: "+spec.api.baseURL+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n", 1)
	}
	return startConfig(t, writeConfig(t, dir, body))
}

func assertChosen(t *testing.T, policyURL, nativePriority string, apiPriority int, want string) {
	t.Helper()
	var nativeHits, apiHits atomic.Int32
	nativeSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		nativeHits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer nativeSrv.Close()
	apiSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		apiHits.Add(1)
		_, _ = w.Write([]byte(okBody))
	}))
	defer apiSrv.Close()
	host := startNative(t, policyURL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{file: "codex-a.json", id: "oauth-a", version: "4", priority: nativePriority, provider: "canonical-oauth", models: []string{nativeModel}, access: "native-token", refresh: "refresh-a", baseURL: nativeSrv.URL, filePriority: "100"}},
		api:      &apiSpec{name: "api-ns", priority: apiPriority, key: "api-key", baseURL: apiSrv.URL, model: nativeModel},
	})
	got := responses(t, host, "41", "7", nativeModel, "")
	if want == "native" && (nativeHits.Load() != 1 || apiHits.Load() != 0) {
		t.Fatalf("native priority %s won api hits=%d native hits=%d status=%d body=%s", nativePriority, apiHits.Load(), nativeHits.Load(), got.status, got.body)
	}
	if want == "api" && (apiHits.Load() != 1 || nativeHits.Load() != 0) {
		t.Fatalf("api priority %d won api hits=%d native hits=%d status=%d body=%s", apiPriority, apiHits.Load(), nativeHits.Load(), got.status, got.body)
	}
}

func assertRoundRobin(t *testing.T, policyURL string) {
	t.Helper()
	var aHits, bHits atomic.Int32
	a := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		aHits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer a.Close()
	b := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		bHits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer b.Close()
	host := startNative(t, policyURL, nativeSpec{
		strategy: "round-robin",
		bindings: []nativeBinding{
			binding("codex-a.json", "oauth-a", a.URL, nativeModel),
			binding("codex-b.json", "oauth-b", b.URL, nativeModel),
		},
	})
	for i := 0; i < 4; i++ {
		_ = responses(t, host, "41", "7", nativeModel, "")
	}
	if aHits.Load() == 0 || bHits.Load() == 0 {
		t.Fatalf("round robin A=%d B=%d", aHits.Load(), bHits.Load())
	}
}

func assertScopeAndDisabled(t *testing.T, policyURL string) {
	t.Helper()
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		hits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	host := startNative(t, policyURL, nativeSpec{strategy: "fill-first", bindings: []nativeBinding{binding("codex-a.json", "oauth-a", upstream.URL, nativeModel)}})
	_ = responses(t, host, "41", "7", otherModel, "")
	if hits.Load() != 0 {
		t.Fatalf("out of scope model was sent: %d", hits.Load())
	}
	disabled := binding("codex-off.json", "oauth-off", upstream.URL, nativeModel)
	disabled.disabled = true
	off := startNative(t, policyURL, nativeSpec{strategy: "fill-first", bindings: []nativeBinding{disabled}})
	before := hits.Load()
	_ = responses(t, off, "41", "7", nativeModel, "")
	if hits.Load() != before {
		t.Fatalf("disabled auth was sent")
	}
}

func assertProxyLegs(t *testing.T, policyURL string) {
	t.Helper()
	var upstreamHits, proxyHits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		upstreamHits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstream.Close()
	proxy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		proxyHits.Add(1)
		target := r.URL
		if target.Host == "" {
			http.Error(w, "proxy target", http.StatusBadGateway)
			return
		}
		req, err := http.NewRequest(r.Method, target.String(), r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadGateway)
			return
		}
		resp, err := directClient().Do(req)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadGateway)
			return
		}
		defer resp.Body.Close()
		for key, values := range resp.Header {
			for _, value := range values {
				w.Header().Add(key, value)
			}
		}
		w.WriteHeader(resp.StatusCode)
		_, _ = io.Copy(w, resp.Body)
	}))
	defer proxy.Close()
	host := startNative(t, policyURL, nativeSpec{
		strategy:  "fill-first",
		api:       &apiSpec{name: "api-ns", priority: 1, key: "api-key", baseURL: upstream.URL, model: "public-model"},
		proxyList: "  proxy-list:\n    direction: whitelist\n    models: [\"public-model\"]\n    proxy-url: " + proxy.URL + "\n",
	})
	_ = chat(t, directClient(), host, "41", "7", `{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`)
	if proxyHits.Load() == 0 || upstreamHits.Load() == 0 {
		t.Fatalf("whitelist proxy=%d upstream=%d", proxyHits.Load(), upstreamHits.Load())
	}
	proxyHits.Store(0)
	upstreamHits.Store(0)
	directHost := startNative(t, policyURL, nativeSpec{
		strategy:  "fill-first",
		proxyURL:  proxy.URL,
		api:       &apiSpec{name: "api-ns", priority: 1, key: "api-key", baseURL: upstream.URL, model: "public-model"},
		proxyList: "  proxy-list:\n    direction: whitelist\n    models: [\"public-model\"]\n    proxy-url: direct\n",
	})
	_ = chat(t, directClient(), directHost, "41", "7", `{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`)
	if proxyHits.Load() != 0 || upstreamHits.Load() == 0 {
		t.Fatalf("direct sentinel proxy=%d upstream=%d", proxyHits.Load(), upstreamHits.Load())
	}
	proxyHits.Store(0)
	upstreamHits.Store(0)
	manualDir := testWorkDir(t)
	manualPort := freeListenPort(t)
	body := strings.ReplaceAll(baseConfig(manualPort, policyURL, "fill-first", false, false, ""), "AUTHDIR", yamlQuote(absoluteAuthDir(t, manualDir)))
	body = strings.Replace(body, "openai-compatibility: []\n", "openai-compatibility:\n  - name: api-ns\n    priority: 1\n    base-url: "+upstream.URL+"/v1\n    api-key-entries:\n      - api-key: api-key\n        proxy-url: "+proxy.URL+"\n    models:\n      - name: public-model\n        alias: public-model\n", 1)
	body = strings.Replace(body, "  credentials: []\n", "  credentials:\n    - namespace: api-ns\n      auth-id: api-ns\n      credential-id: cred-api\n      credential-version: \"3\"\n      binding-id: bind-api\n      material-revision: material-api\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: public-model\n          upstream-model: public-model\n          protocol: chat_completions\n          endpoint: "+upstream.URL+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n", 1)
	manualHost := startConfig(t, writeConfig(t, manualDir, body))
	_ = chat(t, directClient(), manualHost, "41", "7", `{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`)
	if proxyHits.Load() == 0 || upstreamHits.Load() == 0 {
		t.Fatalf("manual proxy=%d upstream=%d", proxyHits.Load(), upstreamHits.Load())
	}
}

func baseConfig(port int, policyURL, strategy string, sticky, conversation bool, proxyURL string) string {
	proxy := ""
	if strings.TrimSpace(proxyURL) != "" {
		proxy = "proxy-url: " + proxyURL + "\n"
	}
	stickyText := "false"
	if sticky {
		stickyText = "true"
	}
	conversationText := "false"
	if conversation {
		conversationText = "true"
	}
	if strings.TrimSpace(strategy) == "" {
		strategy = "fill-first"
	}
	body := strings.ReplaceAll(strings.ReplaceAll(`host: 127.0.0.1
port: PORT
auth-dir: AUTHDIR
api-keys:
  - hop-secret
`+proxy+`remote-management:
  secret-key: ""
  allow-remote: false
  disable-control-panel: true
routing:
  strategy: STRATEGY
request-retry: 0
commercial-mode: true
openai-compatibility: []
ocg:
  protocol-version: 1
  process-generation: "41"
  projection-revision: "7"
  ready-key: ready-secret
  policy:
    url: POLICY
    token: policy-secret
    origin: http://127.0.0.1
  routing:
    sticky-global: STICKY
    conversation-sticky: CONVERSATION
    conversation-ttl-seconds: 1800
  credentials: []
  oauth-bindings: []
`, "PORT", strconv.Itoa(port)), "POLICY", policyURL)
	body = strings.ReplaceAll(body, "STRATEGY", strategy)
	body = strings.ReplaceAll(body, "STICKY", stickyText)
	body = strings.ReplaceAll(body, "CONVERSATION", conversationText)
	return body
}

func writeConfig(t *testing.T, dir, body string) string {
	t.Helper()
	if strings.Contains(body, "STRATEGY") || strings.Contains(body, "STICKY") || strings.Contains(body, "CONVERSATION") {
		t.Fatalf("config placeholders were not substituted")
	}
	configPath, err := filepath.Abs(filepath.Join(dir, "config.yaml"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(configPath, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	return configPath
}

func startConfig(t *testing.T, configPath string) *Host {
	t.Helper()
	host, err := Start(t.Context(), configPath)
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	t.Cleanup(host.Close)
	return host
}

func responses(t *testing.T, host *Host, generation, revision, model, session string) chatResult {
	t.Helper()
	return responsesContext(t, host, context.Background(), generation, revision, model, session)
}

func responsesContext(t *testing.T, host *Host, ctx context.Context, generation, revision, model, session string) chatResult {
	t.Helper()
	payload := `{"model":"` + model + `","input":"hello"}`
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, host.URL()+"/v1/responses", strings.NewReader(payload))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	stampPolicyIdentity(req, newID(), generation, revision)
	if session != "" {
		req.Header.Set("X-Session-ID", session)
	}
	resp, err := directClient().Do(req)
	if err != nil {
		return chatResult{status: 0, body: err.Error()}
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return chatResult{status: resp.StatusCode, body: string(raw)}
}

func replaceConfig(t *testing.T, path, body string) {
	t.Helper()
	tmp := path + ".next"
	var err error
	for attempt := 0; attempt < 8; attempt++ {
		if err = os.WriteFile(tmp, []byte(body), 0o600); err != nil {
			if attempt < 7 && replaceRetryable(err) {
				time.Sleep(40 * time.Millisecond)
				continue
			}
			t.Fatal(err)
		}
		if err = atomicReplace(tmp, path); err != nil {
			if attempt < 7 && replaceRetryable(err) {
				time.Sleep(40 * time.Millisecond)
				continue
			}
			t.Fatal(err)
		}
		return
	}
}

func replaceRetryable(err error) bool {
	errno, ok := err.(syscall.Errno)
	return ok && (errno == syscall.Errno(5) || errno == syscall.Errno(32))
}

func atomicReplace(src, dst string) error {
	if runtime.GOOS == "windows" {
		return moveFileEx(src, dst)
	}
	return os.Rename(src, dst)
}

func moveFileEx(src, dst string) error {
	from, err := syscall.UTF16PtrFromString(src)
	if err != nil {
		return err
	}
	to, err := syscall.UTF16PtrFromString(dst)
	if err != nil {
		return err
	}
	r, _, callErr := syscall.NewLazyDLL("kernel32.dll").NewProc("MoveFileExW").Call(
		uintptr(unsafe.Pointer(from)),
		uintptr(unsafe.Pointer(to)),
		uintptr(1|8),
	)
	if r == 0 {
		if callErr != syscall.Errno(0) {
			return callErr
		}
		return fmt.Errorf("MoveFileExW failed")
	}
	return nil
}

func quoteList(values []string) string {
	parts := make([]string, 0, len(values))
	for _, value := range values {
		parts = append(parts, `"`+value+`"`)
	}
	return strings.Join(parts, ", ")
}

func mapLen(values *sync.Map) int {
	count := 0
	values.Range(func(_, _ any) bool {
		count++
		return true
	})
	return count
}

func leftPad(value string, width int) string {
	for len(value) < width {
		value = "0" + value
	}
	return value
}

func readPeek(r *http.Request) string {
	raw, _ := io.ReadAll(r.Body)
	r.Body = io.NopCloser(strings.NewReader(string(raw)))
	return string(raw)
}

func jsonModel(body string) string {
	var payload struct {
		Model string `json:"model"`
	}
	if err := json.Unmarshal([]byte(body), &payload); err != nil {
		return ""
	}
	return payload.Model
}
