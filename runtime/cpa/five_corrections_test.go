package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"io"
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

	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/executor"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v8/sdk/translator"
)

func TestUnmanagedRedirectsDoNotResend(t *testing.T) {
	for _, code := range []int{http.StatusMovedPermanently, http.StatusFound, http.StatusSeeOther, http.StatusTemporaryRedirect, http.StatusPermanentRedirect} {
		t.Run(strconv.Itoa(code), func(t *testing.T) {
			assertRedirectClosed(t, code, "compat")
			assertRedirectClosed(t, code, "codex")
		})
	}
	t.Run("cancel", func(t *testing.T) {
		assertRedirectCancel(t, "compat")
		assertRedirectCancel(t, "codex")
	})
	t.Run("deadline", func(t *testing.T) {
		assertRedirectDeadline(t, "compat")
		assertRedirectDeadline(t, "codex")
	})
}

func TestOwnedManagementMutatorsDoNotWrite(t *testing.T) {
	t.Setenv("MANAGEMENT_PASSWORD", "mgmt-test-secret")
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	dir := testWorkDir(t)
	port := freeListenPort(t)
	body := baseConfig(port, policy.URL, "fill-first", false, false, "")
	body = strings.ReplaceAll(body, "AUTHDIR", yamlQuote(absoluteAuthDir(t, dir)))
	if err := os.MkdirAll(filepath.FromSlash(absoluteAuthDir(t, dir)), 0o700); err != nil {
		t.Fatal(err)
	}
	configPath := writeConfig(t, dir, body)
	before := shaFile(t, configPath)
	host := startConfig(t, configPath)
	client := directClient()

	rejected := []struct{ method, path, body string }{
		{http.MethodPut, "/v0/management/config.yaml", "port: 9\n"},
		{http.MethodPut, "/v0/management/api-keys", `["rewritten"]`},
		{http.MethodPut, "/v0/management/openai-compatibility", `[]`},
		{http.MethodPut, "/v0/management/routing/strategy", `{"value":"round-robin"}`},
		{http.MethodPost, "/v0/management/api-call", `{"url":"http://127.0.0.1/"}`},
		{http.MethodPut, "/v8/management/config.yaml", "port: 9\n"},
		{http.MethodPatch, "/v8/management/config", `{"port":9}`},
		{http.MethodPut, "/v8/management/config/routing/strategy", `{"value":"round-robin"}`},
	}
	for _, call := range rejected {
		status, text := mgmtDo(t, client, host, call.method, call.path, call.body)
		if status != http.StatusForbidden || !strings.Contains(text, "owned_projection") {
			t.Fatalf("%s %s status=%d body=%s", call.method, call.path, status, text)
		}
		if got := shaFile(t, configPath); got != before {
			t.Fatalf("%s %s changed the projection %s -> %s", call.method, call.path, before, got)
		}
	}

	status, text := mgmtDo(t, client, host, http.MethodGet, "/v0/management/auth-files", "")
	if status != http.StatusOK {
		t.Fatalf("auth file list status=%d body=%s", status, text)
	}
	status, text = mgmtDo(t, client, host, http.MethodPost, "/v0/management/auth-files?name=extra-ref.json", `{"type":"codex","access_token":"uploaded-token","refresh_token":"uploaded-refresh"}`)
	if status != http.StatusOK || !strings.Contains(text, `"status":"ok"`) {
		t.Fatalf("auth upload status=%d body=%s", status, text)
	}
	status, listed := mgmtDo(t, client, host, http.MethodGet, "/v0/management/auth-files", "")
	if status != http.StatusOK || strings.Contains(listed, "uploaded-token") || strings.Contains(listed, "uploaded-refresh") {
		t.Fatalf("auth list leaked credential material status=%d", status)
	}
	status, text = mgmtDo(t, client, host, http.MethodGet, "/v0/management/get-auth-status", "")
	if status != http.StatusOK || !strings.Contains(text, `"status":"ok"`) {
		t.Fatalf("auth status status=%d body=%s", status, text)
	}
	status, text = mgmtDo(t, client, host, http.MethodGet, "/v8/management/oauth/status", "")
	if status != http.StatusOK {
		t.Fatalf("v8 oauth status=%d body=%s", status, text)
	}
	status, text = mgmtDo(t, client, host, http.MethodPost, "/v0/management/auth-files/refresh?name=extra-ref.json", "")
	if status == http.StatusForbidden && strings.Contains(text, "owned_projection") {
		t.Fatalf("refresh was rejected by the owned gate")
	}
	status, text = mgmtDo(t, client, host, http.MethodDelete, "/v0/management/oauth-session?state=ocg-logout-test", "")
	if status != http.StatusOK || !strings.Contains(text, `"cancelled":false`) {
		t.Fatalf("logout status=%d body=%s", status, text)
	}
	status, text = mgmtDo(t, client, host, http.MethodPost, "/v8/management/oauth/import", `{}`)
	if status != http.StatusBadRequest || !strings.Contains(text, "provider is required") {
		t.Fatalf("v8 import status=%d body=%s", status, text)
	}
	status, text = mgmtDo(t, client, host, http.MethodPost, "/v0/management/vertex/import", `{}`)
	if status == http.StatusForbidden && strings.Contains(text, "owned_projection") {
		t.Fatalf("vertex import was rejected by the owned gate")
	}
	if got := shaFile(t, configPath); got != before {
		t.Fatalf("preserved management operations changed the projection")
	}

	next := strings.ReplaceAll(body, `projection-revision: "7"`, `projection-revision: "8"`)
	if err := os.WriteFile(configPath, []byte(next), 0o600); err != nil {
		t.Fatal(err)
	}
	reloadStatus, err := host.Reload(context.Background(), configPath)
	if err != nil || reloadStatus != "applied" || shaFile(t, configPath) == before {
		t.Fatalf("ocg reload status=%s err=%v digest=%s", reloadStatus, err, shaFile(t, configPath))
	}
}

func TestReloadBarrierKeepsAppliedRoutes(t *testing.T) {
	oldHits := atomic.Int32{}
	stableHits := atomic.Int32{}
	candidateHits := atomic.Int32{}
	hold := make(chan struct{})
	var holdOnce sync.Once
	old := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		oldHits.Add(1)
		if r.Header.Get("X-Test-Hold") == "1" {
			holdOnce.Do(func() { close(hold) })
			<-r.Context().Done()
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer old.Close()
	stable := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		stableHits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer stable.Close()
	candidate := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		candidateHits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer candidate.Close()

	readyHeld := make(chan struct{})
	releaseReady := make(chan struct{})
	var readyOnce sync.Once
	policy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		switch req.Operation {
		case "ready":
			if req.ProjectionRevision == "8" {
				readyOnce.Do(func() { close(readyHeld) })
				<-releaseReady
			}
			if req.ProjectionRevision == "9" {
				http.Error(w, "ready rejected", http.StatusInternalServerError)
				return
			}
			writeReady(w, req)
		case "admit":
			writeDecision(w, "allow", "eligible")
		case "result":
			writeDecision(w, "stop", "recorded")
		default:
			http.Error(w, "op", http.StatusBadRequest)
		}
	}))
	defer policy.Close()

	dir := testWorkDir(t)
	if err := os.MkdirAll(filepath.FromSlash(absoluteAuthDir(t, dir)), 0o700); err != nil {
		t.Fatal(err)
	}
	port := freeListenPort(t)
	initial := twoCredentialConfig(t, dir, port, policy.URL, "7", old.URL, stable.URL, "same-key")
	configPath := writeConfig(t, dir, initial)
	originalDigest := shaFile(t, configPath)
	host := startConfig(t, configPath)
	_ = waitReady(t, directClient(), host, `"appliedProjectionRevision":"7"`)
	if got := chat(t, directClient(), host, "41", "7", chatPayload); got.status >= 500 || oldHits.Load() != 1 {
		t.Fatalf("initial chat status=%d old=%d body=%s", got.status, oldHits.Load(), got.body)
	}
	if got := chat(t, directClient(), host, "41", "7", `{"model":"stable-model","messages":[{"role":"user","content":"hi"}]}`); got.status >= 500 || stableHits.Load() != 1 {
		t.Fatalf("stable chat status=%d hits=%d body=%s", got.status, stableHits.Load(), got.body)
	}

	replaceConfig(t, configPath, twoCredentialConfig(t, dir, port, policy.URL, "8", candidate.URL, stable.URL, "same-key"))
	select {
	case <-readyHeld:
	case <-time.After(5 * time.Second):
		t.Fatal("ready was not held")
	}
	heldOld := oldHits.Load()
	heldStable := stableHits.Load()
	if got := chat(t, directClient(), host, "41", "7", chatPayload); got.status >= 500 || oldHits.Load() != heldOld+1 || candidateHits.Load() != 0 {
		t.Fatalf("held reload used a candidate route status=%d old=%d candidate=%d body=%s", got.status, oldHits.Load(), candidateHits.Load(), got.body)
	}
	if got := chat(t, directClient(), host, "41", "7", `{"model":"stable-model","messages":[{"role":"user","content":"hi"}]}`); got.status >= 500 || stableHits.Load() != heldStable+1 || candidateHits.Load() != 0 {
		t.Fatalf("held reload disturbed the unchanged credential status=%d stable=%d candidate=%d", got.status, stableHits.Load(), candidateHits.Load())
	}
	close(releaseReady)
	_ = waitReady(t, directClient(), host, `"appliedProjectionRevision":"8"`)
	if got := chat(t, directClient(), host, "41", "8", chatPayload); got.status >= 500 || candidateHits.Load() != 1 {
		t.Fatalf("committed route status=%d candidate=%d body=%s", got.status, candidateHits.Load(), got.body)
	}
	candidateBeforeStale := candidateHits.Load()
	stale := chat(t, directClient(), host, "41", "7", chatPayload)
	if stale.status != http.StatusConflict || candidateHits.Load() != candidateBeforeStale {
		t.Fatalf("stale revision status=%d candidate=%d body=%s", stale.status, candidateHits.Load(), stale.body)
	}
	if got := chat(t, directClient(), host, "41", "8", `{"model":"stable-model","messages":[{"role":"user","content":"hi"}]}`); got.status >= 500 || stableHits.Load() < heldStable+2 {
		t.Fatalf("committed stable credential status=%d hits=%d", got.status, stableHits.Load())
	}

	committedDigest := shaFile(t, configPath)
	if committedDigest == originalDigest {
		t.Fatal("revision 8 did not change the projection digest")
	}
	freshHits := atomic.Int32{}
	fresh := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		freshHits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer fresh.Close()
	recovered := twoCredentialConfig(t, dir, port, policy.URL, "10", fresh.URL, stable.URL, "same-key")
	recovered = strings.Replace(recovered, "  oauth-bindings: []\n", "  oauth-bindings:\n    - relative-path: missing.json\n      credential-id: oauth-missing\n      credential-version: \"4\"\n      priority: \"1\"\n      provider-id: canonical-oauth\n      models: [\"public-model\"]\n", 1)
	replaceConfig(t, configPath, recovered)
	sawClosed := false
	closedDeadline := time.Now().Add(6 * time.Second)
	for time.Now().Before(closedDeadline) && !sawClosed {
		if freshHits.Load() != 0 {
			t.Fatalf("registration candidate was contacted %d", freshHits.Load())
		}
		if chat(t, directClient(), host, "41", "8", chatPayload).status == http.StatusServiceUnavailable {
			sawClosed = true
			break
		}
		time.Sleep(50 * time.Millisecond)
	}
	if !sawClosed || freshHits.Load() != 0 {
		t.Fatalf("registration failure did not close ingress closed=%v fresh=%d", sawClosed, freshHits.Load())
	}
	restored := false
	var restoredChat chatResult
	waitUntil := time.Now().Add(20 * time.Second)
	for time.Now().Before(waitUntil) {
		if freshHits.Load() != 0 {
			t.Fatalf("registration failure sent to the candidate %d", freshHits.Load())
		}
		ready := waitReadyOnce(t, host)
		got := chat(t, directClient(), host, "41", "8", chatPayload)
		if strings.Contains(ready, `"applyStatus":"rollback"`) && strings.Contains(ready, `"appliedProjectionRevision":"8"`) && strings.Contains(ready, committedDigest) && got.status < 400 && candidateHits.Load() > candidateBeforeStale {
			restored = true
			restoredChat = got
			break
		}
		time.Sleep(200 * time.Millisecond)
	}
	if !restored || freshHits.Load() != 0 {
		t.Fatalf("registration rollback fresh=%d candidate=%d before=%d status=%d body=%s", freshHits.Load(), candidateHits.Load(), candidateBeforeStale, restoredChat.status, restoredChat.body)
	}

	beforeRotate := oldHits.Load() + candidateHits.Load() + stableHits.Load()
	replaceConfig(t, configPath, twoCredentialConfig(t, dir, port, policy.URL, "9", candidate.URL, stable.URL, "rotated-key"))
	failed := waitReady(t, directClient(), host, `"applyStatus":"apply_failure"`)
	if strings.Contains(failed, "same-key") || strings.Contains(failed, "rotated-key") {
		t.Fatal("ready exposed a credential")
	}
	blocked := chat(t, directClient(), host, "41", "8", chatPayload)
	if blocked.status != http.StatusServiceUnavailable || oldHits.Load()+candidateHits.Load()+stableHits.Load() != beforeRotate {
		t.Fatalf("rotation failure status=%d hits old=%d candidate=%d stable=%d", blocked.status, oldHits.Load(), candidateHits.Load(), stableHits.Load())
	}
}

func TestAdmittedResultSurvivesReloadAndGrace(t *testing.T) {
	previousNow := nowFn
	t.Cleanup(func() { nowFn = previousNow })
	var results policyLog
	results.admits = map[string]policyRequest{}
	releaseUpstream := make(chan struct{})
	var once sync.Once
	started := make(chan struct{})
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		once.Do(func() { close(started) })
		select {
		case <-releaseUpstream:
		case <-r.Context().Done():
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstream.Close()
	other := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Error("second upstream was contacted")
		w.WriteHeader(http.StatusOK)
	}))
	defer other.Close()
	readyHeld := make(chan struct{})
	releaseReady := make(chan struct{})
	var readyOnce sync.Once
	policy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		results.mu.Lock()
		defer results.mu.Unlock()
		switch req.Operation {
		case "ready":
			if req.ProjectionRevision == "8" {
				readyOnce.Do(func() { close(readyHeld) })
				results.mu.Unlock()
				<-releaseReady
				results.mu.Lock()
			}
			writeReady(w, req)
		case "admit":
			results.admits[req.AttemptID] = req
			results.seq = append(results.seq, "admit")
			writeDecision(w, "allow", "eligible")
		case "result":
			results.items = append(results.items, req)
			results.seq = append(results.seq, "result")
			writeDecision(w, "stop", "recorded")
		default:
			http.Error(w, "op", http.StatusBadRequest)
		}
	}))
	defer policy.Close()
	dir := testWorkDir(t)
	port := freeListenPort(t)
	body := sampleConfig(strconv.Itoa(port), "7", "41", policy.URL)
	body = strings.ReplaceAll(body, "auth-dir: AUTHDIR", "auth-dir: "+yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.ReplaceAll(body, "http://127.0.0.1:19001/v1", upstream.URL+"/v1")
	body = strings.ReplaceAll(body, "http://127.0.0.1:19002/v1", other.URL+"/v1")
	body = strings.ReplaceAll(body, "origin: http://127.0.0.1:9", "origin: http://127.0.0.1")
	configPath := writeConfig(t, dir, body)
	originalDigest := shaFile(t, configPath)
	host := startConfig(t, configPath)
	_ = waitReady(t, directClient(), host, `"policyReady":true`)

	done := make(chan chatResult, 1)
	go func() {
		done <- postChat(t, context.Background(), host, "41", "7", chatPayload, time.Now().UTC().Add(30*time.Second).Format(time.RFC3339Nano))
	}()
	select {
	case <-started:
	case <-time.After(8 * time.Second):
		t.Fatal("admitted request did not reach the applied upstream")
	}
	next := strings.Replace(body, `projection-revision: "7"`, `projection-revision: "8"`, 1)
	next = strings.Replace(next, upstream.URL+"/v1", other.URL+"/moved", 1)
	replaceConfig(t, configPath, next)
	select {
	case <-readyHeld:
	case <-time.After(5 * time.Second):
		t.Fatal("reload ready was not held")
	}
	close(releaseUpstream)
	got := <-done
	close(releaseReady)
	_ = waitReady(t, directClient(), host, `"appliedProjectionRevision":"8"`)
	results.mu.Lock()
	if got.status >= 500 || len(results.items) != 1 || results.items[0].ProjectionRevision != "7" || results.items[0].ProjectionDigest != originalDigest || results.items[0].Outcome != "success" {
		t.Fatalf("admitted result status=%d body=%s results=%+v", got.status, got.body, results.items)
	}
	results.mu.Unlock()

	graceUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		finishHeldUpstream(w, r, 5*time.Second)
	}))
	defer graceUpstream.Close()
	gracePolicy, graceResults := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer gracePolicy.Close()
	graceDir := testWorkDir(t)
	gracePort := freeListenPort(t)
	graceBody := sampleConfig(strconv.Itoa(gracePort), "7", "41", gracePolicy.URL)
	graceBody = strings.ReplaceAll(graceBody, "auth-dir: AUTHDIR", "auth-dir: "+yamlQuote(absoluteAuthDir(t, graceDir)))
	graceBody = strings.ReplaceAll(graceBody, "http://127.0.0.1:19001/v1", graceUpstream.URL+"/v1")
	graceBody = strings.ReplaceAll(graceBody, "http://127.0.0.1:19002/v1", other.URL+"/v1")
	graceBody = strings.ReplaceAll(graceBody, "origin: http://127.0.0.1:9", "origin: http://127.0.0.1")
	graceHost := startConfig(t, writeConfig(t, graceDir, graceBody))
	_ = waitReady(t, directClient(), graceHost, `"policyReady":true`)
	deadline := time.Now().UTC().Add(2 * time.Second)
	graceDone := make(chan chatResult, 1)
	go func() {
		graceDone <- postChat(t, context.Background(), graceHost, "41", "7", chatPayload, deadline.Format(time.RFC3339Nano))
	}()
	time.Sleep(300 * time.Millisecond)
	nowFn = func() time.Time { return deadline.Add(30 * time.Second) }
	sweep := postChat(t, context.Background(), graceHost, "41", "7", chatPayload, time.Now().UTC().Add(2*time.Second).Format(time.RFC3339Nano))
	if sweep.status < 400 {
		t.Fatalf("grace sweep was admitted status=%d body=%s", sweep.status, sweep.body)
	}
	select {
	case graceGot := <-graceDone:
		graceResults.mu.Lock()
		defer graceResults.mu.Unlock()
		if len(graceResults.items) != 1 || graceResults.items[0].ProjectionRevision != "7" || graceResults.items[0].Outcome != "deadline" {
			t.Fatalf("grace result status=%d body=%s results=%+v", graceGot.status, graceGot.body, graceResults.items)
		}
	case <-time.After(12 * time.Second):
		t.Fatal("grace request did not finish")
	}

	nowFn = previousNow
	lateUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		finishHeldUpstream(w, r, 5*time.Second)
	}))
	defer lateUpstream.Close()
	latePolicy, lateResults := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer latePolicy.Close()
	lateDir := testWorkDir(t)
	latePort := freeListenPort(t)
	lateBody := sampleConfig(strconv.Itoa(latePort), "7", "41", latePolicy.URL)
	lateBody = strings.ReplaceAll(lateBody, "auth-dir: AUTHDIR", "auth-dir: "+yamlQuote(absoluteAuthDir(t, lateDir)))
	lateBody = strings.ReplaceAll(lateBody, "http://127.0.0.1:19001/v1", lateUpstream.URL+"/v1")
	lateBody = strings.ReplaceAll(lateBody, "http://127.0.0.1:19002/v1", other.URL+"/v1")
	lateBody = strings.ReplaceAll(lateBody, "origin: http://127.0.0.1:9", "origin: http://127.0.0.1")
	lateHost := startConfig(t, writeConfig(t, lateDir, lateBody))
	_ = waitReady(t, directClient(), lateHost, `"policyReady":true`)
	lateDeadline := time.Now().UTC().Add(2 * time.Second)
	lateDone := make(chan chatResult, 1)
	go func() {
		lateDone <- postChat(t, context.Background(), lateHost, "41", "7", chatPayload, lateDeadline.Format(time.RFC3339Nano))
	}()
	time.Sleep(300 * time.Millisecond)
	nowFn = func() time.Time { return lateDeadline.Add(publicationGrace + time.Second) }
	_ = postChat(t, context.Background(), lateHost, "41", "7", chatPayload, time.Now().UTC().Add(2*time.Second).Format(time.RFC3339Nano))
	select {
	case <-lateDone:
	case <-time.After(12 * time.Second):
		t.Fatal("expired capture request did not finish")
	}
	lateResults.mu.Lock()
	defer lateResults.mu.Unlock()
	if len(lateResults.items) != 0 {
		t.Fatalf("expired capture still published %+v", lateResults.items)
	}
}

func TestAntigravityCountBodyAndNativeUsage(t *testing.T) {
	model := "gemini-3.7-flash-high"
	payload := []byte(`{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}`)
	opts := cliproxyexecutor.Options{SourceFormat: sdktranslator.FromString("gemini"), OriginalRequest: payload}
	runCount := func(t *testing.T, status int, body string, lose bool) policyRequest {
		t.Helper()
		var hits atomic.Int32
		upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if !antigravityGeneration(r) {
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`{"models":{}}`))
				return
			}
			hits.Add(1)
			if lose {
				w.Header().Set("Content-Length", "64")
				w.WriteHeader(status)
				_, _ = w.Write([]byte("partial"))
				hj, ok := w.(http.Hijacker)
				if !ok {
					return
				}
				conn, _, err := hj.Hijack()
				if err == nil {
					_ = conn.Close()
				}
				return
			}
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(status)
			_, _ = w.Write([]byte(body))
		}))
		defer upstream.Close()
		policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
		defer policy.Close()
		host := startAntigravity(t, policy.URL, upstream.URL, model)
		_, err := host.service.CoreAuthManager().ExecuteCount(withRequestID(context.Background(), newID()), []string{"antigravity"}, cliproxyexecutor.Request{Model: model, Payload: payload}, opts)
		if status >= 200 && status < 300 && err != nil {
			t.Fatalf("count error=%v hits=%d", err, hits.Load())
		}
		results.mu.Lock()
		defer results.mu.Unlock()
		if hits.Load() != 1 || len(results.items) != 1 || len(results.admits) != 1 {
			t.Fatalf("count hits=%d admits=%d results=%d err=%v items=%+v", hits.Load(), len(results.admits), len(results.items), err, results.items)
		}
		return results.items[0]
	}

	success := runCount(t, http.StatusOK, `{"totalTokens":9}`, false)
	if success.Outcome != "success" || success.ErrorCode != "none" || !boolValue(success.Sent) || !boolValue(success.BodyComplete) || success.Status == nil || *success.Status != http.StatusOK {
		t.Fatalf("count success %+v", success)
	}
	rejected := runCount(t, http.StatusUnauthorized, `{"error":"unauthorized-count"}`, false)
	if rejected.Outcome != "explicit_rejection" || !boolValue(rejected.Sent) || !boolValue(rejected.BodyComplete) || rejected.Status == nil || *rejected.Status != http.StatusUnauthorized || !strings.Contains(rejected.ResponseBody, "unauthorized-count") || rejected.Observation == nil {
		t.Fatalf("count 401 %+v", rejected)
	}
	limited := runCount(t, http.StatusTooManyRequests, `{"error":"limited-count"}`, false)
	if limited.Outcome != "explicit_rejection" || limited.Status == nil || *limited.Status != http.StatusTooManyRequests || !strings.Contains(limited.ResponseBody, "limited-count") {
		t.Fatalf("count 429 %+v", limited)
	}
	lost := runCount(t, http.StatusBadGateway, "", true)
	if lost.Outcome != "uncertain" || lost.ErrorCode != "body_lost" || !boolValue(lost.Sent) || boolValue(lost.BodyComplete) || lost.ResponseBody != "" {
		t.Fatalf("count body loss %+v", lost)
	}

	window := `{"error":{"message":"` + strings.Repeat("a", 12*1024) + `","type":"rate_limit"},"window":"quota-window-unique"}`
	mid := runCount(t, http.StatusTooManyRequests, window, false)
	if mid.Outcome != "explicit_rejection" || !boolValue(mid.BodyComplete) || !strings.Contains(mid.ResponseBody, "quota-window-unique") || len(mid.ResponseBody) <= 4096 {
		t.Fatalf("mid-size rejection len=%d outcome=%s bodyComplete=%v", len(mid.ResponseBody), mid.Outcome, boolValue(mid.BodyComplete))
	}
	over := `{"error":{"message":"` + strings.Repeat("b", 70*1024) + `","marker":"oversize-marker-unique"}}`
	huge := runCount(t, http.StatusTooManyRequests, over, false)
	if huge.Outcome == "explicit_rejection" || boolValue(huge.BodyComplete) || strings.Contains(huge.ResponseBody, "oversize-marker-unique") || huge.ErrorCode != "body_lost" {
		t.Fatalf("oversize rejection %+v", huge)
	}

	terminal := "data: {\"response\":{\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"yes\"}]}}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":4}}}\n\n"
	assertAntigravityExchange(t, model, payload, opts, false, terminal, func(item policyRequest, hits int32) {
		if hits != 1 || item.Outcome != "success" || !boolValue(item.BodyComplete) || !boolValue(item.StreamStarted) || !strings.Contains(string(item.ReportedUsage), `"inputTokens":3`) || !strings.Contains(string(item.ReportedUsage), `"outputTokens":4`) {
			t.Fatalf("native stream hits=%d item=%+v", hits, item)
		}
	})
	nonstream := `{"response":{"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"text":"yes"}]}}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":4}}}`
	assertAntigravityExchange(t, model, payload, opts, true, nonstream, func(item policyRequest, hits int32) {
		if hits != 1 || item.Outcome != "success" || !boolValue(item.BodyComplete) || boolValue(item.StreamStarted) || !strings.Contains(string(item.ReportedUsage), `"inputTokens":3`) || !strings.Contains(string(item.ReportedUsage), `"outputTokens":4`) {
			t.Fatalf("native nonstream hits=%d item=%+v", hits, item)
		}
	})
	truncated := "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"partial\"}]}}]}}\n\n"
	assertAntigravityExchange(t, model, payload, opts, false, truncated, func(item policyRequest, hits int32) {
		if hits != 1 || item.Outcome != "uncertain" || boolValue(item.BodyComplete) || item.ReportedUsage != nil {
			t.Fatalf("truncated stream hits=%d item=%+v", hits, item)
		}
	})

	var hits atomic.Int32
	var second atomic.Int32
	started := make(chan struct{})
	var startOnce sync.Once
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !antigravityGeneration(r) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"models":{}}`))
			return
		}
		hits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"partial\"}]}}]}}\n\n"))
		if flusher, ok := w.(http.Flusher); ok {
			flusher.Flush()
		}
		startOnce.Do(func() { close(started) })
		timer := time.NewTimer(3 * time.Second)
		defer timer.Stop()
		select {
		case <-r.Context().Done():
		case <-timer.C:
		}
	}))
	defer upstream.Close()
	shadow := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		second.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer shadow.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startAntigravity(t, policy.URL, upstream.URL, model)
	ctx, cancel := context.WithCancel(withRequestID(context.Background(), newID()))
	defer cancel()
	go func() {
		<-started
		cancel()
	}()
	stream, err := host.service.CoreAuthManager().ExecuteStream(ctx, []string{"antigravity"}, cliproxyexecutor.Request{Model: model, Payload: payload}, opts)
	if stream != nil && stream.Chunks != nil {
		for range stream.Chunks {
		}
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if hits.Load() != 1 || second.Load() != 0 || len(results.items) != 1 || results.items[0].Outcome != "cancelled" || len(results.admits) != 1 {
		t.Fatalf("cancelled stream hits=%d second=%d err=%v items=%+v", hits.Load(), second.Load(), err, results.items)
	}
}

func assertAntigravityExchange(t *testing.T, model string, payload []byte, opts cliproxyexecutor.Options, nonstream bool, response string, check func(policyRequest, int32)) {
	t.Helper()
	var hits atomic.Int32
	var second atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !antigravityGeneration(r) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"models":{}}`))
			return
		}
		hits.Add(1)
		if nonstream {
			w.Header().Set("Content-Type", "application/json")
		} else {
			w.Header().Set("Content-Type", "text/event-stream")
		}
		_, _ = w.Write([]byte(response))
	}))
	defer upstream.Close()
	shadow := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		second.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer shadow.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startAntigravity(t, policy.URL, upstream.URL, model)
	manager := host.service.CoreAuthManager()
	ctx := withRequestID(context.Background(), newID())
	var err error
	if nonstream {
		_, err = manager.Execute(ctx, []string{"antigravity"}, cliproxyexecutor.Request{Model: model, Payload: payload}, opts)
	} else {
		var stream *cliproxyexecutor.StreamResult
		stream, err = manager.ExecuteStream(ctx, []string{"antigravity"}, cliproxyexecutor.Request{Model: model, Payload: payload}, opts)
		if stream != nil && stream.Chunks != nil {
			for chunk := range stream.Chunks {
				if chunk.Err != nil && err == nil {
					err = chunk.Err
				}
			}
		}
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if second.Load() != 0 || len(results.items) != 1 || len(results.admits) != 1 {
		t.Fatalf("exchange hits=%d second=%d err=%v admits=%d items=%+v", hits.Load(), second.Load(), err, len(results.admits), results.items)
	}
	check(results.items[0], hits.Load())
}

func antigravityGeneration(r *http.Request) bool {
	if r == nil || r.URL == nil {
		return false
	}
	switch r.URL.Path {
	case "/v1internal:countTokens", "/v1internal:generateContent", "/v1internal:streamGenerateContent":
		return true
	default:
		return false
	}
}

func startAntigravity(t *testing.T, policyURL, baseURL, model string) *Host {
	t.Helper()
	return startNative(t, policyURL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{
			file: "ag.json", id: "oauth-ag", version: "4", priority: "1", provider: "canonical-oauth",
			models: []string{model}, access: "ag-token", baseURL: baseURL, kind: "antigravity", project: "project-1",
		}},
	})
}

func assertRedirectClosed(t *testing.T, code int, kind string) {
	t.Helper()
	targetHits := atomic.Int32{}
	firstHits := atomic.Int32{}
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		targetHits.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer target.Close()
	first := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		firstHits.Add(1)
		if r.Method != http.MethodPost {
			t.Errorf("redirect source method %s", r.Method)
		}
		w.Header().Set("Location", target.URL+"/v1/chat/completions")
		w.WriteHeader(code)
		_, _ = w.Write([]byte(`{"error":"redirected"}`))
	}))
	defer first.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	driveRedirect(t, policy.URL, first.URL, kind, context.Background(), "")
	results.mu.Lock()
	defer results.mu.Unlock()
	if targetHits.Load() != 0 || firstHits.Load() != 1 || len(results.admits) != 1 || len(results.items) != 1 {
		t.Fatalf("%s %d target=%d first=%d admits=%d results=%d", kind, code, targetHits.Load(), firstHits.Load(), len(results.admits), len(results.items))
	}
}

func assertRedirectCancel(t *testing.T, kind string) {
	t.Helper()
	targetHits := atomic.Int32{}
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		targetHits.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer target.Close()
	first := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("redirect source method %s", r.Method)
		}
		w.Header().Set("Location", target.URL+"/followed")
		w.WriteHeader(http.StatusTemporaryRedirect)
		_, _ = w.Write([]byte(`{"error":"redirected"}`))
	}))
	defer first.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		time.Sleep(500 * time.Millisecond)
		cancel()
	}()
	driveRedirect(t, policy.URL, first.URL, kind, ctx, "")
	time.Sleep(700 * time.Millisecond)
	if targetHits.Load() != 0 {
		t.Fatalf("%s cancel followed the redirect %d", kind, targetHits.Load())
	}
}

func assertRedirectDeadline(t *testing.T, kind string) {
	t.Helper()
	targetHits := atomic.Int32{}
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		targetHits.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer target.Close()
	first := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Errorf("redirect source method %s", r.Method)
		}
		w.Header().Set("Location", target.URL+"/followed")
		w.WriteHeader(http.StatusTemporaryRedirect)
		_, _ = w.Write([]byte(`{"error":"redirected"}`))
	}))
	defer first.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	deadline := time.Now().UTC().Add(2 * time.Second).Format(time.RFC3339Nano)
	driveRedirect(t, policy.URL, first.URL, kind, context.Background(), deadline)
	time.Sleep(300 * time.Millisecond)
	if targetHits.Load() != 0 {
		t.Fatalf("%s deadline followed the redirect %d", kind, targetHits.Load())
	}
}

func driveRedirect(t *testing.T, policyURL, upstream, kind string, ctx context.Context, deadline string) {
	t.Helper()
	switch kind {
	case "compat":
		other := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			t.Error("compat failover was contacted")
			w.WriteHeader(http.StatusOK)
		}))
		defer other.Close()
		host, _ := startSample(t, policyURL, "7", "41", upstream, other.URL)
		_ = postChat(t, ctx, host, "41", "7", chatPayload, deadline)
	case "codex":
		host := startNative(t, policyURL, nativeSpec{strategy: "fill-first", bindings: []nativeBinding{binding("codex-a.json", "oauth-a", upstream, nativeModel)}})
		payload := `{"model":"` + nativeModel + `","input":"hello"}`
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, host.URL()+"/v1/responses", strings.NewReader(payload))
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("Authorization", "Bearer hop-secret")
		stampPolicyIdentity(req, newID(), "41", "7")
		if deadline != "" {
			req.Header.Set("X-OCG-Request-Deadline", deadline)
		}
		resp, err := directClient().Do(req)
		if err == nil {
			_, _ = io.ReadAll(resp.Body)
			_ = resp.Body.Close()
		}
	default:
		t.Fatalf("unknown redirect kind %s", kind)
	}
}

func postChat(t *testing.T, ctx context.Context, host *Host, generation, revision, payload, deadline string) chatResult {
	t.Helper()
	if ctx == nil {
		ctx = context.Background()
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, host.URL()+"/v1/chat/completions", strings.NewReader(payload))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	stampPolicyIdentity(req, newID(), generation, revision)
	if deadline != "" {
		req.Header.Set("X-OCG-Request-Deadline", deadline)
	}
	resp, err := directClient().Do(req)
	if err != nil {
		return chatResult{status: 0, body: err.Error()}
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return chatResult{status: resp.StatusCode, body: string(raw)}
}

func mgmtDo(t *testing.T, client *http.Client, host *Host, method, path, body string) (int, string) {
	t.Helper()
	req, err := http.NewRequest(method, host.URL()+path, strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer mgmt-test-secret")
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return resp.StatusCode, string(raw)
}

func twoCredentialConfig(t *testing.T, dir string, port int, policyURL, revision, changedBase, stableBase, changedKey string) string {
	t.Helper()
	body := baseConfig(port, policyURL, "fill-first", false, false, "")
	body = strings.ReplaceAll(body, "AUTHDIR", yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.ReplaceAll(body, `projection-revision: "7"`, `projection-revision: "`+revision+`"`)
	body = strings.Replace(body, "openai-compatibility: []\n", "openai-compatibility:\n  - name: changed-ns\n    priority: 10\n    base-url: "+changedBase+"/v1\n    api-key-entries:\n      - api-key: "+changedKey+"\n    models:\n      - name: upstream-model\n        alias: public-model\n  - name: stable-ns\n    priority: 1\n    base-url: "+stableBase+"/v1\n    api-key-entries:\n      - api-key: stable-key\n    models:\n      - name: stable-model\n        alias: stable-model\n", 1)
	body = strings.Replace(body, "  credentials: []\n", "  credentials:\n    - namespace: changed-ns\n      auth-id: changed-ns\n      credential-id: cred-changed\n      credential-version: \"3\"\n      binding-id: bind-changed\n      material-revision: material-changed\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: chat_completions\n          endpoint: "+changedBase+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n    - namespace: stable-ns\n      auth-id: stable-ns\n      credential-id: cred-stable\n      credential-version: \"3\"\n      binding-id: bind-stable\n      material-revision: material-stable\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: stable-model\n          upstream-model: stable-model\n          protocol: chat_completions\n          endpoint: "+stableBase+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n", 1)
	return body
}

func finishHeldUpstream(w http.ResponseWriter, r *http.Request, limit time.Duration) {
	timer := time.NewTimer(limit)
	defer timer.Stop()
	select {
	case <-r.Context().Done():
	case <-timer.C:
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}
}

func shaFile(t *testing.T, path string) string {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:])
}

func waitReadyOnce(t *testing.T, host *Host) string {
	t.Helper()
	req, err := http.NewRequest(http.MethodGet, host.URL()+"/_internal/ocg/ready", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("X-OCG-Ready-Token", "ready-secret")
	req.Header.Set("Origin", "http://127.0.0.1")
	resp, err := directClient().Do(req)
	if err != nil {
		return err.Error()
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return string(raw)
}
