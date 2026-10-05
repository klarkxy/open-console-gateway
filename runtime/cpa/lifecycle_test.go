package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestLifecycleStopAfter429(t *testing.T) {
	dir := testWorkDir(t)
	var sendsA, sendsB atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sendsA.Add(1)
		w.Header().Set("Retry-After", "1")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"limited"},"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}`))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"id":"b","choices":[{"message":{"role":"assistant","content":"no"}}]}`))
	}))
	defer upstreamB.Close()

	var mu sync.Mutex
	admits := map[string]policyRequest{}
	var results []policyRequest
	policy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil {
			http.Error(w, "bad", http.StatusBadRequest)
			return
		}
		mu.Lock()
		defer mu.Unlock()
		switch req.Operation {
		case "ready":
			if req.AuthID != "" || req.AttemptID != "" || req.ProcessGeneration == "" || req.ProjectionDigest == "" {
				http.Error(w, "ready", http.StatusBadRequest)
				return
			}
			writeReady(w, req)
		case "admit":
			if !canonicalUUID(req.AttemptID) || !canonicalUUID(req.RequestID) || req.ProviderID == "" || req.ProviderID == req.AuthID || req.Kind != "accepted" || req.RegistrationEpoch == "" {
				http.Error(w, "admit", http.StatusBadRequest)
				return
			}
			admits[req.AttemptID] = req
			writeDecision(w, "allow", "eligible")
		case "result":
			prior, ok := admits[req.AttemptID]
			if !ok || prior.ProjectionRevision != req.ProjectionRevision || prior.ProjectionDigest != req.ProjectionDigest || prior.ProcessGeneration != req.ProcessGeneration || prior.Kind != req.Kind {
				http.Error(w, "stale", http.StatusConflict)
				return
			}
			results = append(results, req)
			action := "stop"
			reason := "completed"
			if req.Status != nil && *req.Status == http.StatusTooManyRequests {
				reason = "explicit_rejection"
			}
			writeDecision(w, action, reason)
		default:
			http.Error(w, "op", http.StatusBadRequest)
		}
	}))
	defer policy.Close()

	port := freeListenPort(t)
	body := sampleConfig(strconv.Itoa(port), "7", "41", policy.URL)
	body = strings.ReplaceAll(body, "auth-dir: AUTHDIR", "auth-dir: "+yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.ReplaceAll(body, "http://127.0.0.1:19001/v1", upstreamA.URL+"/v1")
	body = strings.ReplaceAll(body, "http://127.0.0.1:19002/v1", upstreamB.URL+"/v1")
	body = strings.ReplaceAll(body, "origin: http://127.0.0.1:9", "origin: http://127.0.0.1")
	configPath := filepath.Join(dir, "config.yaml")
	if err := os.WriteFile(configPath, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256([]byte(body))
	digest := hex.EncodeToString(sum[:])
	ctx := t.Context()
	host, err := Start(ctx, configPath)
	if err != nil {
		t.Fatalf("start: %v\n%s", err, body)
	}
	t.Cleanup(host.Close)
	same, err := os.ReadFile(configPath)
	if err != nil || string(same) != body {
		t.Fatal("host rewrote the private config")
	}

	readyReq, err := http.NewRequest(http.MethodGet, host.URL()+"/_internal/ocg/ready", nil)
	if err != nil {
		t.Fatal(err)
	}
	readyReq.Header.Set("X-OCG-Ready-Token", "ready-secret")
	readyReq.Header.Set("Origin", "http://127.0.0.1")
	readyResp, err := (&http.Client{Timeout: 10 * time.Second, Transport: &http.Transport{Proxy: nil}}).Do(readyReq)
	if err != nil {
		t.Fatal(err)
	}
	readyBody, _ := io.ReadAll(readyResp.Body)
	_ = readyResp.Body.Close()
	if readyResp.StatusCode != http.StatusOK {
		t.Fatalf("ready status %d", readyResp.StatusCode)
	}
	for _, secret := range []string{"hop-secret", "policy-secret", "ready-secret", "upstream-a", "upstream-b"} {
		if strings.Contains(string(readyBody), secret) {
			t.Fatal("ready exposed a secret")
		}
	}
	if !strings.Contains(string(readyBody), digest) || !strings.Contains(string(readyBody), `"processGeneration":"41"`) {
		t.Fatalf("ready identity missing")
	}

	client := &http.Client{Timeout: 20 * time.Second, Transport: &http.Transport{Proxy: nil}}
	missing, err := http.NewRequest(http.MethodPost, host.URL()+"/v1/chat/completions", bytes.NewBufferString(`{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`))
	if err != nil {
		t.Fatal(err)
	}
	missing.Header.Set("Authorization", "Bearer hop-secret")
	missingResp, err := client.Do(missing)
	if err != nil {
		t.Fatal(err)
	}
	_ = missingResp.Body.Close()
	if missingResp.StatusCode != http.StatusBadRequest {
		t.Fatalf("missing correlation status %d", missingResp.StatusCode)
	}

	chat, err := http.NewRequest(http.MethodPost, host.URL()+"/v1/chat/completions", bytes.NewBufferString(`{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`))
	if err != nil {
		t.Fatal(err)
	}
	chat.Header.Set("Authorization", "Bearer hop-secret")
	stampPolicyIdentity(chat, "00000000-0000-4000-8000-0000000000aa", "41", "7")
	chatResp, err := client.Do(chat)
	if err != nil {
		t.Fatal(err)
	}
	chatBody, _ := io.ReadAll(chatResp.Body)
	_ = chatResp.Body.Close()
	if sendsA.Load() == 0 || sendsB.Load() != 0 {
		mu.Lock()
		admitN, resultN := len(admits), len(results)
		mu.Unlock()
		t.Fatalf("sends A=%d B=%d status=%d admits=%d results=%d body=%s", sendsA.Load(), sendsB.Load(), chatResp.StatusCode, admitN, resultN, chatBody)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(results) == 0 || results[0].AttemptID == "" || admits[results[0].AttemptID].AttemptID != results[0].AttemptID {
		t.Fatal("result was not tied to its admission")
	}
	if results[0].Status == nil || *results[0].Status != http.StatusTooManyRequests || results[0].Sent == nil || !*results[0].Sent || results[0].BodyComplete == nil || !*results[0].BodyComplete {
		t.Fatalf("429 facts were not captured: %+v", results[0])
	}
	if results[0].ProviderID != "canonical-provider" || results[0].AuthID != "auth-ns" {
		t.Fatalf("identity = %s %s", results[0].AuthID, results[0].ProviderID)
	}
	if !bytes.Contains(results[0].ReportedUsage, []byte(`"inputTokens":3`)) || !bytes.Contains(results[0].ReportedUsage, []byte(`"outputTokens":1`)) || bytes.Contains(results[0].ReportedUsage, []byte("total_tokens")) {
		t.Fatalf("canonical usage = %s", results[0].ReportedUsage)
	}
	if results[0].Observation == nil || results[0].Outcome != "explicit_rejection" || results[0].ErrorCode != "provider_rejected" {
		t.Fatalf("429 wire = %+v", results[0])
	}
}

func freeListenPort(t *testing.T) int {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	return ln.Addr().(*net.TCPAddr).Port
}
