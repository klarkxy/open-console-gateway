package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

func TestNativeScopedModelsSurviveAuthFileEvent(t *testing.T) {
	t.Run("codex", func(t *testing.T) {
		assertScopeSurvivesFileEvent(t, scopeEvent{
			kind: "codex", ingress: "/v1/responses", protocol: "responses", path: "/responses",
			model: "claude-sonnet", payload: `{"model":"claude-sonnet","input":"hello"}`,
			body: codexSSE, contentType: "text/event-stream",
			outside: "gpt-6-luna", excluded: "gpt-6-luna",
		})
	})
	t.Run("claude", func(t *testing.T) {
		body := `{"id":"msg","type":"message","role":"assistant","model":"claude-sonnet","stop_reason":"end_turn","content":[{"type":"text","text":"yes"}],"usage":{"input_tokens":1,"output_tokens":1}}`
		assertScopeSurvivesFileEvent(t, scopeEvent{
			kind: "claude", ingress: "/v1/messages", protocol: "messages", path: "/v1/messages", query: "beta=true",
			model: "claude-sonnet", payload: `{"model":"claude-sonnet","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}`,
			body: body, contentType: "application/json",
			outside: "claude-sonnet-4-5-20250929", excluded: "claude-sonnet-4-5-20250929",
		})
	})
	t.Run("kimi", func(t *testing.T) {
		assertScopeSurvivesFileEvent(t, scopeEvent{
			kind: "kimi", ingress: "/v1/chat/completions", protocol: "chat_completions", path: "/v1/chat/completions",
			model: nativeModel, payload: `{"model":"` + nativeModel + `","messages":[{"role":"user","content":"hi"}]}`,
			body: okBody, contentType: "application/json",
			outside: "kimi-k2", excluded: "kimi-k2",
		})
	})
	t.Run("xai", func(t *testing.T) {
		assertScopeSurvivesFileEvent(t, scopeEvent{
			kind: "xai", usingAPI: "false", ingress: "/v1/responses", protocol: "responses", path: "/responses",
			model: nativeModel, payload: `{"model":"` + nativeModel + `","input":"hello"}`,
			body: codexSSE, contentType: "text/event-stream",
			outside: "grok-4.7", excluded: "grok-4.7",
		})
	})
	t.Run("narrowed", func(t *testing.T) {
		assertScopeSurvivesFileEvent(t, scopeEvent{
			kind: "xai", usingAPI: "false", models: []string{nativeModel},
			ingress: "/v1/responses", protocol: "responses", path: "/responses",
			model: nativeModel, payload: `{"model":"` + nativeModel + `","input":"hello"}`,
			body: codexSSE, contentType: "text/event-stream",
			outside: "claude-sonnet", excluded: "grok-4.7",
		})
	})
	t.Run("disabled", func(t *testing.T) {
		assertDisabledScopeStaysUnregistered(t)
	})
}

type scopeEvent struct {
	kind, usingAPI, ingress, protocol, path, query, model, payload, body, contentType, outside, excluded string
	models                                                                                               []string
}

func assertScopeSurvivesFileEvent(t *testing.T, spec scopeEvent) {
	t.Helper()
	models := spec.models
	if len(models) == 0 {
		models = []string{nativeModel, "claude-sonnet"}
	}
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
		w.Header().Set("Content-Type", spec.contentType)
		_, _ = w.Write([]byte(spec.body))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	credentialID := "oauth-" + spec.kind + "-scope"
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{
			file: spec.kind + "-scope.json", id: credentialID, version: "4", priority: "1", provider: "canonical-oauth",
			models: models, access: "token-" + spec.kind, baseURL: upstream.URL, kind: spec.kind, usingAPI: spec.usingAPI, project: "project-1",
		}},
	})
	writeAuthNote(t, host, spec.kind+"-scope.json", "scope-event-"+spec.kind)
	auth := waitAuthFileScope(t, host, credentialID, "scope-event-"+spec.kind, models)
	if metadataText(auth, "ocg_credential_id") != credentialID || metadataText(auth, "ocg_credential_version") != "4" || auth.Attributes["ocg_provider_id"] != "canonical-oauth" {
		t.Fatalf("credential after file event id=%s version=%s provider=%s", metadataText(auth, "ocg_credential_id"), metadataText(auth, "ocg_credential_version"), auth.Attributes["ocg_provider_id"])
	}
	if auth.Attributes["ocg_model_scope"] != strings.Join(models, ",") || !excludedHas(auth.Attributes["excluded_models"], spec.excluded) || excludedHas(auth.Attributes["excluded_models"], spec.model) {
		t.Fatalf("scope=%q excluded=%q want excluded %s and not %s", auth.Attributes["ocg_model_scope"], auth.Attributes["excluded_models"], spec.excluded, spec.model)
	}
	got := postPolicy(t, host, spec.ingress, spec.payload)
	if hits.Load() != 1 || gotPath != spec.path || gotQuery != spec.query || got.status >= 500 {
		t.Fatalf("%s hits=%d path=%s query=%s status=%d body=%s", spec.kind, hits.Load(), gotPath, gotQuery, got.status, got.body)
	}
	target := upstream.URL + spec.path
	if spec.query != "" {
		target += "?" + spec.query
	}
	item := soleResult(t, results)
	epoch := strconv.FormatUint(auth.RegistrationEpoch, 10)
	if item.Outcome != "success" || item.Sent == nil || !*item.Sent || item.CredentialID != credentialID || item.CredentialVersion != "4" || item.RegistrationEpoch != epoch {
		t.Fatalf("%s result %+v want credential %s version 4 epoch %s", spec.kind, item, credentialID, epoch)
	}
	requireMatchedPin(t, item, target, spec.protocol)
	refused := postPolicy(t, host, spec.ingress, strings.Replace(spec.payload, spec.model, spec.outside, 1))
	if hits.Load() != 1 || refused.status < 400 {
		t.Fatalf("%s outside %s hits=%d status=%d body=%s", spec.kind, spec.outside, hits.Load(), refused.status, refused.body)
	}
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(auth.ID), models) {
		t.Fatalf("%s registration changed after refusal: %v", spec.kind, coreauth.OCGRegisteredModelIDs(auth.ID))
	}
}

func assertDisabledScopeStaysUnregistered(t *testing.T) {
	t.Helper()
	var hits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			hits.Add(1)
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer upstream.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{
			file: "claude-disabled.json", id: "oauth-disabled", version: "4", priority: "1", provider: "canonical-oauth",
			models: []string{nativeModel, "claude-sonnet"}, access: "token-disabled", baseURL: upstream.URL, kind: "claude",
			project: "project-1", disabled: true,
		}},
	})
	writeAuthNote(t, host, "claude-disabled.json", "scope-event-disabled")
	auth := waitAuthFileScope(t, host, "oauth-disabled", "scope-event-disabled", nil)
	if len(coreauth.OCGRegisteredModelIDs(auth.ID)) != 0 {
		t.Fatalf("disabled registration = %v", coreauth.OCGRegisteredModelIDs(auth.ID))
	}
	refused := postPolicy(t, host, "/v1/messages", `{"model":"claude-sonnet","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}`)
	if hits.Load() != 0 || refused.status < 400 {
		t.Fatalf("disabled hits=%d status=%d body=%s", hits.Load(), refused.status, refused.body)
	}
}

func writeAuthNote(t *testing.T, host *Host, file, note string) {
	t.Helper()
	path := filepath.Join(host.AuthDir(), file)
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var doc map[string]any
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatal(err)
	}
	doc["note"] = note
	encoded, err := json.Marshal(doc)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, encoded, 0o600); err != nil {
		t.Fatal(err)
	}
}

func waitAuthFileScope(t *testing.T, host *Host, credentialID, note string, want []string) *coreauth.Auth {
	t.Helper()
	deadline := time.Now().Add(8 * time.Second)
	var generation uint64
	seen := false
	stable := 0
	for time.Now().Before(deadline) {
		auth := nativeAuth(t, host, credentialID)
		if auth.Attributes["note"] != note {
			time.Sleep(10 * time.Millisecond)
			continue
		}
		got := coreauth.OCGRegisteredModelIDs(auth.ID)
		if !sameModelIDs(got, want) {
			if len(want) == 0 {
				time.Sleep(10 * time.Millisecond)
				continue
			}
			t.Fatalf("%s registered %v after auth file event, want %v", credentialID, got, want)
		}
		if seen && auth.Generation == generation {
			stable++
			if stable >= 2 {
				return auth
			}
		} else {
			generation = auth.Generation
			seen = true
			stable = 0
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("%s auth file event did not settle on %v", credentialID, want)
	return nil
}

func sameModelIDs(got, want []string) bool {
	if len(got) != len(want) {
		return false
	}
	seen := make(map[string]int, len(got))
	for _, id := range got {
		seen[id]++
	}
	for _, id := range want {
		if seen[id] == 0 {
			return false
		}
		seen[id]--
	}
	return true
}

func excludedHas(excluded, id string) bool {
	for _, item := range strings.Split(excluded, ",") {
		if strings.TrimSpace(item) == id {
			return true
		}
	}
	return false
}
