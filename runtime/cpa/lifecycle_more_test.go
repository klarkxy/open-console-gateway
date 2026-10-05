package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestLifecycleLostBodyAndParse(t *testing.T) {
	var phase atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if phase.Add(1) == 1 {
			dropLimitedBody(w)
			return
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer upstreamA.Close()
	var sendsB atomic.Int32
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"id":"b","choices":[{"message":{"role":"assistant","content":"no"}}]}`))
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.ErrorCode != "" && !acceptedCode(req.ErrorCode) {
			return ""
		}
		return "stop"
	})
	defer policy.Close()

	host, body := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	client := directClient()
	first := chat(t, client, host, "41", "7", `{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`)
	if first.status == 0 {
		t.Fatal("lost body produced no response")
	}
	second := chat(t, client, host, "41", "7", `{"model":"public-model","messages":[{"role":"user","content":"again"}]}`)
	if sendsB.Load() != 0 {
		t.Fatalf("credential B was sent after a halted result: %d", sendsB.Load())
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 2 {
		t.Fatalf("results = %+v first=%d second=%d", results.items, first.status, second.status)
	}
	lost, parsed := results.items[0], results.items[1]
	if lost.AttemptID == "" || lost.AttemptID != admitsID(results, lost.AttemptID) || lost.BodyComplete == nil || *lost.BodyComplete || lost.Sent == nil || !*lost.Sent {
		t.Fatalf("lost 429 body facts = %+v", lost)
	}
	if lost.ErrorCode != "body_lost" || lost.Outcome != "uncertain" {
		t.Fatalf("lost 429 body code = %s %s", lost.Outcome, lost.ErrorCode)
	}
	if parsed.Status == nil || *parsed.Status != http.StatusOK || parsed.BodyComplete == nil || !*parsed.BodyComplete || parsed.ErrorCode != "parser" || parsed.Outcome != "uncertain" || parsed.Sent == nil || !*parsed.Sent {
		t.Fatalf("200 parse facts = %+v body=%s", parsed, second.body)
	}
	if strings.Contains(string(parsed.ReportedUsage), "total_tokens") && strings.Contains(string(second.body), "not-json") {
		t.Fatal("parse failure invented usage")
	}
	same, err := os.ReadFile(host.configPath)
	if err != nil || string(same) != body {
		t.Fatal("host rewrote the private config")
	}
}

func TestLifecycleRoutesReloadAndBoundary(t *testing.T) {
	var sendsA, sendsMsg, sendsB atomic.Int32
	var sawTool, sawSticky atomic.Bool
	var msgBody atomic.Value
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sendsA.Add(1)
		raw, _ := io.ReadAll(r.Body)
		if strings.Contains(string(raw), "lookup") {
			sawTool.Store(true)
		}
		if r.Header.Get("X-Session-ID") == "ocg-sticky-global" {
			sawSticky.Store(true)
		}
		w.Header().Set("Retry-After", "1")
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = w.Write([]byte(`{"error":{"message":"unauthorized"}}`))
	}))
	defer upstreamA.Close()
	messages := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sendsMsg.Add(1)
		raw, _ := io.ReadAll(r.Body)
		msgBody.Store(string(raw))
		if !strings.HasSuffix(r.URL.Path, "/messages") {
			http.Error(w, "path", http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"id":"msg_1","type":"message","role":"assistant","model":"upstream-model","stop_reason":"end_turn","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":2,"output_tokens":1}}`))
	}))
	defer messages.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"id":"b","choices":[{"message":{"role":"assistant","content":"no"}}]}`))
	}))
	defer upstreamB.Close()
	codexSink := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = w.Write([]byte(`{"error":{"message":"unauthorized"}}`))
	}))
	defer codexSink.Close()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusUnauthorized {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()

	dir := testWorkDir(t)
	port := freeListenPort(t)
	authDir := absoluteAuthDir(t, dir)
	refresh := "oauth-refresh-secret"
	sum := sha256.Sum256([]byte(refresh))
	material := hex.EncodeToString(sum[:])
	if err := os.MkdirAll(filepath.FromSlash(authDir), 0o700); err != nil {
		t.Fatal(err)
	}
	codexFile := `{"type":"codex","refresh_token":"` + refresh + `","base_url":"` + codexSink.URL + `","token_url":"` + codexSink.URL + `"}`
	if err := os.WriteFile(filepath.Join(filepath.FromSlash(authDir), "codex-user.json"), []byte(codexFile), 0o600); err != nil {
		t.Fatal(err)
	}
	body := routedConfig(strconv.Itoa(port), "7", "41", policy.URL, material)
	body = strings.ReplaceAll(body, "auth-dir: AUTHDIR", "auth-dir: "+yamlQuote(authDir))
	body = strings.ReplaceAll(body, "http://127.0.0.1:19001/v1", upstreamA.URL+"/v1")
	body = strings.ReplaceAll(body, "http://127.0.0.1:19002/v1", upstreamB.URL+"/v1")
	body = strings.ReplaceAll(body, "http://127.0.0.1:19003/v1", messages.URL+"/v1")
	body = strings.ReplaceAll(body, "origin: http://127.0.0.1:9", "origin: http://127.0.0.1")
	configPath, err := filepath.Abs(filepath.Join(dir, "config.yaml"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(configPath, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()
	host, err := Start(ctx, configPath)
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	t.Cleanup(host.Close)
	client := directClient()
	readyBody := waitReady(t, client, host, "codex-user.json")
	if strings.Contains(readyBody, refresh) || strings.Contains(readyBody, "hop-secret") || strings.Contains(readyBody, "policy-secret") || strings.Contains(readyBody, "upstream-a") || strings.Contains(readyBody, authDir) {
		t.Fatal("ready exposed a secret, token, or private path")
	}
	var ready readyDocument
	if err := json.Unmarshal([]byte(readyBody), &ready); err != nil {
		t.Fatal(err)
	}
	if !regexp.MustCompile(`^[0-9a-f]{64}$`).MatchString(ready.Artifact.ExecutableSHA256) {
		t.Fatalf("artifact hash = %q", ready.Artifact.ExecutableSHA256)
	}
	mapped := false
	for _, ref := range ready.AuthRefs {
		if strings.Contains(ref.RelativePath, "codex-user.json") && ref.Provider == "canonical-oauth" && ref.Mapped && ref.MaterialRevision == material {
			mapped = true
		}
	}
	if !mapped {
		t.Fatalf("oauth ref = %+v", ready.AuthRefs)
	}

	payload := `{"model":"public-model","messages":[{"role":"user","content":"hi"}],"tools":[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]}`
	first := chat(t, client, host, "41", "7", payload)
	if sendsA.Load() == 0 || sendsMsg.Load() == 0 || sendsB.Load() != 0 {
		t.Fatalf("routes A=%d msg=%d B=%d status=%d body=%s upstream=%s", sendsA.Load(), sendsMsg.Load(), sendsB.Load(), first.status, first.body, msgBody.Load())
	}
	if !sawTool.Load() || !sawSticky.Load() {
		t.Fatalf("tool=%v sticky=%v", sawTool.Load(), sawSticky.Load())
	}
	results.mu.Lock()
	if len(results.items) < 2 || results.items[0].AttemptID == results.items[1].AttemptID || results.items[1].Kind != "accepted" || results.items[0].Kind != "accepted" {
		t.Fatalf("route attempts = %+v", results.items)
	}
	if results.items[0].AuthID != "auth-ns" || results.items[0].ProviderID != "canonical-provider" || results.items[0].ProjectionRevision != "7" {
		t.Fatalf("first route identity = %+v", results.items[0])
	}
	if results.items[1].ReportedUsage == nil || !strings.Contains(string(results.items[1].ReportedUsage), `"inputTokens":2`) || strings.Contains(string(results.items[1].ReportedUsage), "input_tokens") {
		t.Fatalf("messages usage = %s", results.items[1].ReportedUsage)
	}
	beforeReload := len(results.items)
	results.mu.Unlock()

	bare, err := http.NewRequest(http.MethodPost, host.URL()+"/v1/chat/completions", strings.NewReader(payload))
	if err != nil {
		t.Fatal(err)
	}
	stampPolicyIdentity(bare, "00000000-0000-4000-8000-0000000000bb", "41", "7")
	bareResp, err := client.Do(bare)
	if err != nil {
		t.Fatal(err)
	}
	_ = bareResp.Body.Close()
	if bareResp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("missing hop status %d", bareResp.StatusCode)
	}

	reloaded := strings.Replace(body, `projection-revision: "7"`, `projection-revision: "9"`, 1)
	if err := os.WriteFile(configPath, []byte(reloaded), 0o600); err != nil {
		t.Fatal(err)
	}
	if status, err := host.Reload(ctx, configPath); err != nil || status != "applied" {
		t.Fatalf("reload %s: %v", status, err)
	}
	kept, err := os.ReadFile(configPath)
	if err != nil || string(kept) != reloaded {
		t.Fatal("reload rewrote the private config")
	}
	stale := chat(t, client, host, "41", "7", payload)
	if stale.status != http.StatusConflict {
		t.Fatalf("stale revision status %d body %s", stale.status, stale.body)
	}
	results.mu.Lock()
	if len(results.items) != beforeReload {
		t.Fatal("stale revision reached admission")
	}
	results.mu.Unlock()
	fresh := chat(t, client, host, "41", "9", payload)
	if fresh.status == http.StatusConflict {
		t.Fatalf("reloaded revision was rejected: %s", fresh.body)
	}
	newSum := sha256.Sum256([]byte(reloaded))
	newDigest := hex.EncodeToString(newSum[:])
	results.mu.Lock()
	last := results.items[len(results.items)-1]
	results.mu.Unlock()
	if last.ProjectionRevision != "9" || last.ProjectionDigest != newDigest || last.ProcessGeneration != "41" {
		t.Fatalf("reload capture = %+v", last)
	}

	host.Close()
	nextPort := freeListenPort(t)
	restartedBody := strings.Replace(reloaded, "port: "+strconv.Itoa(port), "port: "+strconv.Itoa(nextPort), 1)
	restartedBody = strings.Replace(restartedBody, `process-generation: "41"`, `process-generation: "42"`, 1)
	restartPath := filepath.Join(dir, "restart.yaml")
	if err := os.WriteFile(restartPath, []byte(restartedBody), 0o600); err != nil {
		t.Fatal(err)
	}
	restarted, err := Start(ctx, restartPath)
	if err != nil {
		t.Fatalf("restart: %v", err)
	}
	t.Cleanup(restarted.Close)
	oldGen := chat(t, client, restarted, "41", "9", payload)
	if oldGen.status != http.StatusConflict {
		t.Fatalf("restarted generation status %d", oldGen.status)
	}
}

type policyLog struct {
	mu           sync.Mutex
	items        []policyRequest
	admits       map[string]policyRequest
	seq          []string
	overridePins bool
	pins         []endpointPin
}

func strictPolicy(t *testing.T, resultAction func(policyRequest) string) (*httptest.Server, *policyLog) {
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
			if !canonicalUUID(req.AttemptID) || !canonicalUUID(req.RequestID) || req.ProviderID == "" || req.ProviderID == req.AuthID || req.ProjectionDigest == "" || req.Kind != "accepted" {
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

func admitsID(log *policyLog, id string) string {
	return log.admits[id].AttemptID
}

func acceptedCode(code string) bool {
	for _, item := range acceptedErrorCodes {
		if item == code {
			return true
		}
	}
	return false
}

type chatResult struct {
	status int
	body   string
}

func directClient() *http.Client {
	return &http.Client{Timeout: 20 * time.Second, Transport: &http.Transport{Proxy: nil}}
}

func generatedRequestDeadline() string {
	return time.Now().UTC().Add(2 * time.Minute).Format(time.RFC3339Nano)
}

func stampPolicyIdentity(req *http.Request, id, generation, revision string) {
	req.Header.Set("X-OCG-Request-Id", id)
	req.Header.Set("X-OCG-Process-Generation", generation)
	req.Header.Set("X-OCG-Projection-Revision", revision)
	req.Header.Set("X-OCG-Request-Deadline", generatedRequestDeadline())
}

func chat(t *testing.T, client *http.Client, host *Host, generation, revision, payload string) chatResult {
	t.Helper()
	req, err := http.NewRequest(http.MethodPost, host.URL()+"/v1/chat/completions", strings.NewReader(payload))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	stampPolicyIdentity(req, newID(), generation, revision)
	resp, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return chatResult{status: resp.StatusCode, body: string(raw)}
}

func waitReady(t *testing.T, client *http.Client, host *Host, needle string) string {
	t.Helper()
	deadline := time.Now().Add(8 * time.Second)
	var last string
	for time.Now().Before(deadline) {
		req, err := http.NewRequest(http.MethodGet, host.URL()+"/_internal/ocg/ready", nil)
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("X-OCG-Ready-Token", "ready-secret")
		req.Header.Set("Origin", "http://127.0.0.1")
		resp, err := client.Do(req)
		if err != nil {
			time.Sleep(50 * time.Millisecond)
			continue
		}
		raw, _ := io.ReadAll(resp.Body)
		_ = resp.Body.Close()
		last = string(raw)
		if resp.StatusCode == http.StatusOK && strings.Contains(last, needle) {
			return last
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("ready missing %s: %s", needle, last)
	return last
}

func TestGenerationIngressOutsideV1(t *testing.T) {
	policy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil || req.Operation != "ready" {
			http.Error(w, "ready", http.StatusBadRequest)
			return
		}
		writeReady(w, req)
	}))
	defer policy.Close()
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Error("generation ingress reached an upstream")
		w.WriteHeader(http.StatusOK)
	}))
	defer upstream.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstream.URL, upstream.URL)
	client := &http.Client{Timeout: 10 * time.Second, Transport: &http.Transport{Proxy: nil}}
	paths := []string{
		"/v1beta/models/public-model:generateContent",
		"/v1beta/models/public-model:streamGenerateContent",
		"/v1beta/models/public-model:countTokens",
		"/v1beta/interactions",
		"/backend-api/codex/responses",
		"/backend-api/codex/responses/compact",
		"/openai/v1/videos",
	}
	for _, path := range paths {
		missing, err := http.NewRequest(http.MethodPost, host.URL()+path, strings.NewReader(`{}`))
		if err != nil {
			t.Fatal(err)
		}
		missing.Header.Set("Authorization", "Bearer hop-secret")
		missingResp, err := client.Do(missing)
		if err != nil {
			t.Fatal(err)
		}
		missingBody, _ := io.ReadAll(missingResp.Body)
		_ = missingResp.Body.Close()
		if missingResp.StatusCode != http.StatusBadRequest || !strings.Contains(string(missingBody), "missing_policy_identity") {
			t.Fatalf("%s missing gate = %d %s", path, missingResp.StatusCode, missingBody)
		}

		mismatch, err := http.NewRequest(http.MethodPost, host.URL()+path, strings.NewReader(`{}`))
		if err != nil {
			t.Fatal(err)
		}
		mismatch.Header.Set("Authorization", "Bearer hop-secret")
		mismatch.Header.Set("X-OCG-Request-Id", "00000000-0000-4000-8000-0000000000c1")
		mismatch.Header.Set("X-OCG-Process-Generation", "999")
		mismatch.Header.Set("X-OCG-Projection-Revision", "7")
		mismatchResp, err := client.Do(mismatch)
		if err != nil {
			t.Fatal(err)
		}
		mismatchBody, _ := io.ReadAll(mismatchResp.Body)
		_ = mismatchResp.Body.Close()
		if mismatchResp.StatusCode != http.StatusConflict || !strings.Contains(string(mismatchBody), "projection_mismatch") {
			t.Fatalf("%s mismatch gate = %d %s", path, mismatchResp.StatusCode, mismatchBody)
		}
	}
	catalog, err := http.NewRequest(http.MethodGet, host.URL()+"/v1beta/models", nil)
	if err != nil {
		t.Fatal(err)
	}
	catalog.Header.Set("Authorization", "Bearer hop-secret")
	catalogResp, err := client.Do(catalog)
	if err != nil {
		t.Fatal(err)
	}
	catalogBody, _ := io.ReadAll(catalogResp.Body)
	_ = catalogResp.Body.Close()
	if catalogResp.StatusCode == http.StatusBadRequest && strings.Contains(string(catalogBody), "missing_policy_identity") {
		t.Fatalf("model catalog was treated as a generation: %s", catalogBody)
	}
}

func startSample(t *testing.T, policyURL, revision, generation, upstreamA, upstreamB string) (*Host, string) {
	t.Helper()
	dir := testWorkDir(t)
	port := freeListenPort(t)
	body := sampleConfig(strconv.Itoa(port), revision, generation, policyURL)
	body = strings.ReplaceAll(body, "auth-dir: AUTHDIR", "auth-dir: "+yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.ReplaceAll(body, "http://127.0.0.1:19001/v1", upstreamA+"/v1")
	body = strings.ReplaceAll(body, "http://127.0.0.1:19002/v1", upstreamB+"/v1")
	body = strings.ReplaceAll(body, "origin: http://127.0.0.1:9", "origin: http://127.0.0.1")
	configPath, err := filepath.Abs(filepath.Join(dir, "config.yaml"))
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(configPath, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	host, err := Start(t.Context(), configPath)
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	t.Cleanup(host.Close)
	return host, body
}

func dropLimitedBody(w http.ResponseWriter) {
	hj, ok := w.(http.Hijacker)
	if !ok {
		http.Error(w, "hijack", http.StatusInternalServerError)
		return
	}
	conn, rw, err := hj.Hijack()
	if err != nil {
		return
	}
	_, _ = rw.WriteString("HTTP/1.1 429 Too Many Requests\r\nContent-Length: 80\r\nRetry-After: 3\r\nConnection: close\r\n\r\n")
	_ = rw.Flush()
	_ = conn.Close()
}

func routedConfig(port, revision, generation, policyURL, material string) string {
	body := sampleConfig(port, revision, generation, policyURL)
	body = strings.Replace(body, `      - api-key: upstream-a`, "      - api-key: upstream-a\n        proxy-url: http://127.0.0.1:1", 1)
	oldRoutes := `      routes:
        - public-model: public-model
          upstream-model: upstream-model
          protocol: chat_completions
          endpoint: http://127.0.0.1:19001/v1/chat/completions
          auth-scheme: bearer
          request-identity: none
          wire: none
    - namespace: auth-b`
	newRoutes := `      routes:
        - public-model: public-model
          upstream-model: upstream-model
          protocol: chat_completions
          endpoint: http://127.0.0.1:19001/v1/chat/completions
          auth-scheme: bearer
          request-identity: none
          wire: none
        - public-model: public-model
          upstream-model: upstream-model
          protocol: messages
          endpoint: http://127.0.0.1:19003/v1/messages
          auth-scheme: bearer
          request-identity: none
          wire: none
    - namespace: auth-b`
	body = strings.Replace(body, oldRoutes, newRoutes, 1)
	binding := `  oauth-bindings:
    - relative-path: codex-user.json
      credential-id: oauth-cred
      credential-version: "4"
      priority: "2"
      material-revision: ` + material + `
      provider-id: canonical-oauth
      models: ["public-model"]
`
	return strings.Replace(body, "  oauth-bindings: []\n", binding, 1)
}
