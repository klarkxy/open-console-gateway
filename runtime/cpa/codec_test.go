package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

func TestPrivateConfigDigestAndRejection(t *testing.T) {
	dir := testWorkDir(t)
	body := sampleConfig("18080", "7", "41", "http://127.0.0.1:9/policy")
	path := filepath.Join(dir, "config.yaml")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	raw, doc, cfg, digest, err := loadPrivateConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(raw)
	if digest != hex.EncodeToString(sum[:]) || digest != hex.EncodeToString(sha256Sum(before)) {
		t.Fatalf("digest = %s", digest)
	}
	if doc.ProcessGeneration.String() != "41" || doc.ProjectionRevision.String() != "7" {
		t.Fatalf("generation/revision = %s %s", doc.ProcessGeneration.String(), doc.ProjectionRevision.String())
	}
	if err := validatePrivateConfig(doc, cfg); err != nil {
		t.Fatal(err)
	}
	applyInMemory(cfg, doc)
	if !cfg.Routing.SessionAffinity || cfg.Routing.SessionAffinityTTL != "1800s" {
		t.Fatalf("sticky was not applied in memory: %+v", cfg.Routing)
	}
	after, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(after) != string(before) {
		t.Fatal("reader rewrote the config")
	}
	floatBody := strings.Replace(body, "credential-version: \"3\"", "credential-version: 1.0", 1)
	floatPath := filepath.Join(dir, "float.yaml")
	if err := os.WriteFile(floatPath, []byte(floatBody), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, _, _, _, err := loadPrivateConfig(floatPath); err == nil {
		t.Fatal("float version was accepted")
	}
	loopBody := strings.Replace(body, "http://127.0.0.1:19001/v1/chat/completions", "http://127.0.0.1:18080/v1/chat/completions", 1)
	loopPath := filepath.Join(dir, "loop.yaml")
	if err := os.WriteFile(loopPath, []byte(loopBody), 0o600); err != nil {
		t.Fatal(err)
	}
	_, loopDoc, loopCfg, _, err := loadPrivateConfig(loopPath)
	if err != nil {
		t.Fatal(err)
	}
	if err := validatePrivateConfig(loopDoc, loopCfg); err == nil || !strings.Contains(err.Error(), "self_loop") {
		t.Fatal(err)
	}
}

func TestPolicyCodecAndBoundary(t *testing.T) {
	schemaRaw, err := os.ReadFile("policy-v1.schema.json")
	if err != nil {
		t.Fatal(err)
	}
	var schema struct {
		ErrorCodes []string `json:"errorCodes"`
	}
	if err := json.Unmarshal(schemaRaw, &schema); err != nil {
		t.Fatal(err)
	}
	if strings.Join(schema.ErrorCodes, ",") != strings.Join(acceptedErrorCodes, ",") {
		t.Fatalf("schema codes = %v", schema.ErrorCodes)
	}
	fixture, err := os.ReadFile(filepath.Join("testdata", "policy-v1.identity.json"))
	if err != nil {
		t.Fatal(err)
	}
	var fixtureCases map[string]json.RawMessage
	if err := json.Unmarshal(fixture, &fixtureCases); err != nil {
		t.Fatal(err)
	}
	for name, raw := range fixtureCases {
		if err := validatePolicyDocument(raw); err != nil {
			t.Fatalf("fixture %s is not wire v1: %v", name, err)
		}
	}
	var identity struct {
		Ready                policyRequest `json:"ready"`
		Admit                policyRequest `json:"admit"`
		ExplicitTrustedLimit policyRequest `json:"explicitTrustedLimit"`
		Ordinary429          policyRequest `json:"ordinary429"`
		NullStatus           policyRequest `json:"nullStatus"`
	}
	if err := json.Unmarshal(fixture, &identity); err != nil {
		t.Fatal(err)
	}
	if identity.Ready.AuthID != "" || identity.Ready.AttemptID != "" || identity.Ready.RequestID != "" {
		t.Fatal("ready fixture selected an auth")
	}
	readyOut, err := json.Marshal(identity.Ready)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(readyOut), "authId") || strings.Contains(string(readyOut), "attemptId") || strings.Contains(string(readyOut), "registrationEpoch") {
		t.Fatalf("ready encoded an attempt: %s", readyOut)
	}
	if identity.Admit.AttemptID != identity.ExplicitTrustedLimit.AttemptID || identity.Admit.ProviderID == identity.Admit.AuthID {
		t.Fatal("fixture identity drifted")
	}
	if identity.Admit.Kind != "accepted" || identity.Admit.RegistrationEpoch != "2" || !canonicalUUID(identity.Admit.RequestID) {
		t.Fatalf("admit wire = %+v", identity.Admit)
	}
	if identity.Ordinary429.Outcome != "explicit_rejection" || identity.NullStatus.Status != nil || !strings.Contains(string(fixture), `"status": null`) {
		t.Fatal("fixture missed ordinary429 or null status")
	}
	numeric := bytes.Replace(fixture, []byte(`"registrationEpoch": "2"`), []byte(`"registrationEpoch": 2`), 1)
	if err := validatePolicyDocument(extractFixture(t, numeric, "admit")); err == nil {
		t.Fatal("numeric registrationEpoch was accepted")
	}
	opts := deviceLoginOptions()
	if opts.Metadata["codex_login_mode"] != "device" {
		t.Fatal("device login mode was not selected")
	}

	var mu sync.Mutex
	var admits, results []policyRequest
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		if err != nil || validatePolicyDocument(raw) != nil {
			http.Error(w, "bad", http.StatusBadRequest)
			return
		}
		var req policyRequest
		if err := json.Unmarshal(raw, &req); err != nil {
			http.Error(w, "bad", http.StatusBadRequest)
			return
		}
		mu.Lock()
		defer mu.Unlock()
		switch req.Operation {
		case "admit":
			admits = append(admits, req)
			writeDecision(w, "allow", "eligible")
		case "result":
			if len(admits) == 0 || admits[0].AttemptID != req.AttemptID || admits[0].ProjectionRevision != req.ProjectionRevision {
				http.Error(w, "stale", http.StatusConflict)
				return
			}
			results = append(results, req)
			writeDecision(w, "stop", "explicit_rejection")
		default:
			http.Error(w, "bad", http.StatusBadRequest)
		}
	}))
	defer server.Close()
	host := &Host{
		policyURL:         server.URL,
		policyToken:       "policy-token",
		policyOrigin:      "http://127.0.0.1",
		policyClient:      newPolicyClient(),
		processGeneration: "41",
	}
	host.appliedRevision.Store("7")
	host.appliedDigest.Store(strings.Repeat("ab", 32))
	host.policyReady.Store(true)
	auth := &coreauth.Auth{
		ID: "auth-ns",
		Attributes: map[string]string{
			"ocg_provider_id": "canonical-provider",
		},
		Metadata: map[string]any{
			"ocg_credential_id":      "cred-1",
			"ocg_credential_version": "3",
			"ocg_material_revision":  "material-1",
		},
	}
	boundary := generationBoundary{host: host}
	if _, err := boundary.BeforeSend(context.Background(), auth, "execute", "upstream", "public"); err == nil {
		t.Fatal("missing request id was admitted")
	}
	if _, err := boundary.BeforeSend(withRequestID(context.Background(), "req-1"), auth, "execute", "upstream", "public"); err == nil {
		t.Fatal("non-uuid request id was admitted")
	}
	expired, cancelExpired := context.WithDeadline(withRequestID(context.Background(), "00000000-0000-4000-8000-000000000001"), time.Now().Add(-time.Second))
	defer cancelExpired()
	if _, err := boundary.BeforeSend(expired, auth, "execute", "upstream", "public"); err == nil {
		t.Fatal("deadline admission was allowed")
	}
	ctx := withTestProtocol(withRequestID(context.Background(), "00000000-0000-4000-8000-000000000001"))
	first, err := boundary.BeforeSend(ctx, auth, "execute", "upstream", "public")
	if err != nil || first.AttemptID == "" {
		t.Fatal(err)
	}
	second, err := boundary.BeforeSend(ctx, auth, "execute", "upstream", "public")
	if err != nil || second.AttemptID == first.AttemptID {
		t.Fatalf("attempt ids were not distinct: %v %v", first.AttemptID, second.AttemptID)
	}
	host.appliedRevision.Store("99")
	host.processGeneration = "100"
	resultCtx := coreauth.WithAttemptContext(ctx, first.AttemptID)
	coreauth.NoteGenerationSent(resultCtx)
	coreauth.NoteGenerationStatus(resultCtx, 429, http.Header{"Retry-After": []string{"3"}}, []byte("limited"))
	coreauth.NoteGenerationBodyComplete(resultCtx)
	result := &coreauth.Result{AuthID: auth.ID, Success: false, Error: &coreauth.Error{HTTPStatus: 429, Message: "status text is not evidence"}}
	if err := boundary.AfterResult(resultCtx, auth, "execute", "upstream", "public", result); err != nil {
		t.Fatal(err)
	}
	if !result.HaltRotation {
		t.Fatal("policy stop was cleared")
	}
	mu.Lock()
	defer mu.Unlock()
	if len(results) != 1 || results[0].AttemptID != first.AttemptID || results[0].ProjectionRevision != "7" || results[0].ProcessGeneration != "41" {
		t.Fatalf("result capture = %+v", results)
	}
	if results[0].ErrorCode != "provider_rejected" || results[0].Outcome != "explicit_rejection" || results[0].Kind != "accepted" || results[0].RegistrationEpoch != "0" || results[0].ProviderID != "canonical-provider" || results[0].Sent == nil || !*results[0].Sent {
		t.Fatalf("result facts = %+v", results[0])
	}
	if results[0].Observation == nil || results[0].Observation.ID != first.AttemptID || results[0].ResponseBody != "limited" {
		t.Fatalf("trusted rejection observation = %+v", results[0].Observation)
	}
	if len(results[0].ReportedUsage) > 0 {
		t.Fatalf("usage was invented: %s", results[0].ReportedUsage)
	}
	empty := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`{"action":""}`))
	}))
	defer empty.Close()
	host.policyURL = empty.URL
	if _, err := host.postPolicy(context.Background(), admits[0], false); err == nil {
		t.Fatal("empty action was accepted")
	}
	readyAsAction := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		writeDecision(w, "allow", "ready")
	}))
	defer readyAsAction.Close()
	host.policyURL = readyAsAction.URL
	if _, err := host.postPolicy(context.Background(), policyRequest{ProtocolVersion: 1, Operation: "ready", ProcessGeneration: "41", ProjectionRevision: "7", ProjectionDigest: strings.Repeat("ab", 32)}, false); err == nil {
		t.Fatal("ready response was decoded as an action")
	}
	if err := boundary.AfterResult(resultCtx, auth, "execute", "upstream", "public", result); err == nil {
		t.Fatal("duplicate result was accepted")
	}
}

func extractFixture(t *testing.T, body []byte, name string) []byte {
	t.Helper()
	var all map[string]json.RawMessage
	if err := json.Unmarshal(body, &all); err != nil {
		t.Fatal(err)
	}
	raw, ok := all[name]
	if !ok {
		t.Fatalf("fixture %s missing", name)
	}
	return raw
}

func absoluteAuthDir(t *testing.T, dir string) string {
	t.Helper()
	abs, err := filepath.Abs(filepath.Join(dir, "auth"))
	if err != nil {
		t.Fatal(err)
	}
	slash := filepath.ToSlash(abs)
	if strings.Contains(slash, "..") {
		t.Fatalf("auth dir still contains parent segments: %s", slash)
	}
	return slash
}

func yamlQuote(value string) string {
	return "'" + strings.ReplaceAll(value, "'", "''") + "'"
}

func testWorkDir(t *testing.T) string {
	t.Helper()
	base := filepath.Join("..", "test-work")
	if err := os.MkdirAll(base, 0o700); err != nil {
		t.Fatal(err)
	}
	dir, err := os.MkdirTemp(base, "case-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func sha256Sum(body []byte) []byte {
	sum := sha256.Sum256(body)
	return sum[:]
}

func sampleConfig(port, revision, generation, policyURL string) string {
	return strings.ReplaceAll(strings.ReplaceAll(strings.ReplaceAll(strings.ReplaceAll(`host: 127.0.0.1
port: PORT
auth-dir: AUTHDIR
api-keys:
  - hop-secret
remote-management:
  secret-key: ""
  allow-remote: false
  disable-control-panel: true
routing:
  strategy: fill-first
request-retry: 0
commercial-mode: true
openai-compatibility:
  - name: auth-ns
    priority: 10
    base-url: http://127.0.0.1:19001/v1
    api-key-entries:
      - api-key: upstream-a
    models:
      - name: upstream-model
        alias: public-model
  - name: auth-b
    priority: 1
    base-url: http://127.0.0.1:19002/v1
    api-key-entries:
      - api-key: upstream-b
    models:
      - name: upstream-model
        alias: public-model
ocg:
  protocol-version: 1
  process-generation: "GENERATION"
  projection-revision: "REVISION"
  ready-key: ready-secret
  policy:
    url: POLICY
    token: policy-secret
    origin: http://127.0.0.1:9
  routing:
    sticky-global: true
    conversation-sticky: true
    conversation-ttl-seconds: 1800
  proxy-list:
    direction: whitelist
    models: ["public-model"]
    proxy-url: direct
  credentials:
    - namespace: auth-ns
      auth-id: auth-ns
      credential-id: cred-1
      credential-version: "3"
      binding-id: bind-1
      material-revision: material-1
      provider-id: canonical-provider
      opaque-remote: false
      routes:
        - public-model: public-model
          upstream-model: upstream-model
          protocol: chat_completions
          endpoint: http://127.0.0.1:19001/v1/chat/completions
          auth-scheme: bearer
          request-identity: none
          wire: none
    - namespace: auth-b
      auth-id: auth-b
      credential-id: cred-2
      credential-version: "3"
      binding-id: bind-2
      material-revision: material-2
      provider-id: canonical-provider
      opaque-remote: false
      routes:
        - public-model: public-model
          upstream-model: upstream-model
          protocol: chat_completions
          endpoint: http://127.0.0.1:19002/v1/chat/completions
          auth-scheme: bearer
          request-identity: none
          wire: none
  oauth-bindings: []
`, "PORT", port), "GENERATION", generation), "REVISION", revision), "POLICY", policyURL)
}
