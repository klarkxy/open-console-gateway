package main

import (
	"context"
	"encoding/json"
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

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/executor"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v8/sdk/translator"
)

const (
	chatPayload = `{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`
	okBody      = `{"id":"ok","choices":[{"message":{"role":"assistant","content":"yes"}}]}`
	sseBody     = "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n"
)

func TestExplicit429FailoverPublishesBeforeNext(t *testing.T) {
	var sendsA, sendsB atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsA.Add(1)
		w.Header().Set("Retry-After", "1")
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"limited"}}`))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusTooManyRequests {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	got := chat(t, directClient(), host, "41", "7", chatPayload)
	if sendsA.Load() != 1 || sendsB.Load() != 1 {
		t.Fatalf("429 failover sends A=%d B=%d status=%d body=%s", sendsA.Load(), sendsB.Load(), got.status, got.body)
	}
	assertResultBeforeNextAdmit(t, results)
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 2 {
		t.Fatalf("results = %+v", results.items)
	}
	first := results.items[0]
	if first.Outcome != "explicit_rejection" || first.ErrorCode != "provider_rejected" || first.Observation == nil || first.Observation.ID != first.AttemptID {
		t.Fatalf("explicit 429 = %+v", first)
	}
	if first.ProviderID != "canonical-provider" {
		t.Fatalf("provider = %s", first.ProviderID)
	}
	if results.items[1].Outcome != "success" || results.items[1].ErrorCode != "none" || results.items[1].AttemptID == first.AttemptID {
		t.Fatalf("failover result = %+v", results.items[1])
	}
}

func TestOrdinary429DoesNotExhaustProcess(t *testing.T) {
	var sendsA, sendsB atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsA.Add(1)
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"ordinary"}}`))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	client := directClient()
	first := chat(t, client, host, "41", "7", chatPayload)
	if sendsB.Load() != 0 || sendsA.Load() == 0 {
		t.Fatalf("ordinary 429 continued or never sent: A=%d B=%d status=%d", sendsA.Load(), sendsB.Load(), first.status)
	}
	if host.unavailable.Load() {
		t.Fatal("ordinary 429 marked the process unavailable")
	}
	second := chat(t, client, host, "41", "7", chatPayload)
	if second.status == http.StatusServiceUnavailable {
		t.Fatalf("next request was refused: %s", second.body)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 2 || results.items[0].Outcome != "explicit_rejection" || results.items[0].Observation == nil {
		t.Fatalf("ordinary 429 results = %+v second=%d", results.items, second.status)
	}
	admits := 0
	for _, item := range results.seq {
		if item == "admit" {
			admits++
		}
	}
	if admits < 2 {
		t.Fatalf("next request was not admitted: %v", results.seq)
	}
}

func TestGeneric404StopsDespiteSkip(t *testing.T) {
	var sendsB atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNotFound)
		_, _ = w.Write([]byte(`{"error":{"message":"missing"}}`))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "skip" })
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	got := chat(t, directClient(), host, "41", "7", chatPayload)
	if sendsB.Load() != 0 {
		t.Fatalf("404 continued to B status=%d body=%s", got.status, got.body)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 1 || results.items[0].Outcome != "uncertain" || results.items[0].ErrorCode != "unknown" || results.items[0].Observation != nil {
		t.Fatalf("404 wire = %+v", results.items)
	}
}

func TestCompatUnauthorizedWithoutRefreshDoesNotResend(t *testing.T) {
	var sendsA, sendsB atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if sendsA.Add(1) == 1 {
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":{"message":"unauthorized"}}`))
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusUnauthorized {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	got := chat(t, directClient(), host, "41", "7", chatPayload)
	if sendsA.Load() != 1 || sendsB.Load() != 1 {
		t.Fatalf("compat 401 without a refresh token sends A=%d B=%d status=%d body=%s", sendsA.Load(), sendsB.Load(), got.status, got.body)
	}
	assertResultBeforeNextAdmit(t, results)
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) == 0 || results.items[0].Status == nil || *results.items[0].Status != http.StatusUnauthorized || results.items[0].Observation == nil {
		t.Fatalf("401 result = %+v", results.items)
	}
}

func TestCompatUnauthorizedStopDoesNotResend(t *testing.T) {
	var sendsA atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsA.Add(1)
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = w.Write([]byte(`{"error":{"message":"unauthorized"}}`))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Errorf("credential B was contacted")
		w.WriteHeader(http.StatusOK)
	}))
	defer upstreamB.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	_ = chat(t, directClient(), host, "41", "7", chatPayload)
	if sendsA.Load() != 1 {
		t.Fatalf("stop still resent: %d", sendsA.Load())
	}
}

func TestModelPoolPublishesBetweenModels(t *testing.T) {
	var models []string
	var mu sync.Mutex
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		mu.Lock()
		models = append(models, string(raw))
		call := len(models)
		mu.Unlock()
		if call == 1 {
			w.WriteHeader(http.StatusTooManyRequests)
			_, _ = w.Write([]byte(`{"error":{"message":"pool"}}`))
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamA.Close()
	var sendsB atomic.Int32
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusTooManyRequests {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host := startMutated(t, policy.URL, upstreamA.URL, upstreamB.URL, func(body string) string {
		old := "      - name: upstream-model\n        alias: public-model\n  - name: auth-b"
		pool := "      - name: model-one\n        alias: public-model\n      - name: model-two\n        alias: public-model\n  - name: auth-b"
		return strings.Replace(body, old, pool, 1)
	})
	got := chat(t, directClient(), host, "41", "7", chatPayload)
	mu.Lock()
	seen := append([]string(nil), models...)
	mu.Unlock()
	if len(seen) < 2 || sendsB.Load() != 0 {
		t.Fatalf("pool calls=%d B=%d status=%d body=%s", len(seen), sendsB.Load(), got.status, got.body)
	}
	assertResultBeforeNextAdmit(t, results)
	results.mu.Lock()
	defer results.mu.Unlock()
	upstreams := map[string]bool{}
	for _, admit := range results.admits {
		upstreams[admit.UpstreamModel] = true
	}
	if !upstreams["model-one"] || !upstreams["model-two"] {
		t.Fatalf("pool admissions = %+v", results.admits)
	}
	if len(results.items) < 2 || results.items[0].AttemptID == results.items[1].AttemptID || results.items[0].AuthID != "auth-ns" {
		t.Fatalf("pool results = %+v", results.items)
	}
}

func TestStreamEmptyBootstrapSuccessAndCancel(t *testing.T) {
	var phase atomic.Int32
	started := make(chan struct{})
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch phase.Add(1) {
		case 1:
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
		case 2:
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":{"message":"bootstrap"}}`))
		case 3:
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte(sseBody))
		default:
			w.Header().Set("Content-Type", "text/event-stream")
			w.WriteHeader(http.StatusOK)
			flusher, _ := w.(http.Flusher)
			_, _ = w.Write([]byte("data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n"))
			if flusher != nil {
				flusher.Flush()
			}
			close(started)
			<-r.Context().Done()
		}
	}))
	defer upstreamA.Close()
	var sendsB atomic.Int32
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	client := directClient()
	empty := streamChat(t, client, host, chatPayload)
	if empty.status == 0 {
		t.Fatal("empty stream produced no response")
	}
	bootstrap := streamChat(t, client, host, chatPayload)
	if bootstrap.status == 0 {
		t.Fatal("bootstrap 401 produced no response")
	}
	done := streamChat(t, client, host, chatPayload)
	if !strings.Contains(done.body, "ok") && done.status >= 500 {
		t.Fatalf("completed stream status=%d body=%s", done.status, done.body)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		<-started
		time.Sleep(20 * time.Millisecond)
		cancel()
	}()
	cancelled := streamChatContext(t, client, host, ctx, chatPayload)
	_ = cancelled
	waitUntil := time.Now().Add(2 * time.Second)
	for time.Now().Before(waitUntil) {
		results.mu.Lock()
		published := len(results.items) >= 4
		results.mu.Unlock()
		if published {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if sendsB.Load() != 0 {
		t.Fatal("stream failure continued to credential B")
	}
	if host.unavailable.Load() {
		t.Fatal("stream cancellation marked the process unavailable")
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 4 {
		t.Fatalf("stream results = %+v", results.items)
	}
	emptyResult, bootResult, doneResult, cancelResult := results.items[0], results.items[1], results.items[2], results.items[3]
	if emptyResult.Outcome == "success" || emptyResult.ErrorCode == "none" {
		t.Fatalf("empty stream was success: %+v", emptyResult)
	}
	if bootResult.Outcome != "explicit_rejection" || bootResult.ErrorCode != "provider_rejected" || bootResult.Observation == nil || (bootResult.StreamStarted != nil && *bootResult.StreamStarted) {
		t.Fatalf("bootstrap 401 = %+v", bootResult)
	}
	if doneResult.Outcome != "success" || doneResult.ErrorCode != "none" || doneResult.StreamStarted == nil || !*doneResult.StreamStarted || doneResult.BodyComplete == nil || !*doneResult.BodyComplete {
		t.Fatalf("completed stream = %+v", doneResult)
	}
	if cancelResult.Outcome != "cancelled" || cancelResult.ErrorCode != "cancelled" {
		t.Fatalf("cancelled stream = %+v", cancelResult)
	}
	if strings.Contains(waitReady(t, client, host, `"policyReady":true`), "hop-secret") {
		t.Fatal("ready exposed the hop secret after cancellation")
	}
}

func TestDeadlineDuringSendDoesNotContinue(t *testing.T) {
	var sendsA, sendsB atomic.Int32
	release := make(chan struct{})
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		sendsA.Add(1)
		select {
		case <-r.Context().Done():
		case <-release:
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamA.Close()
	defer close(release)
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sendsB.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "skip" })
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	auth := compatAuth(t, host, "auth-ns")
	ctx, cancel := context.WithTimeout(withRequestID(context.Background(), newID()), time.Second)
	defer cancel()
	payload := []byte(chatPayload)
	_, err := host.service.CoreAuthManager().Execute(ctx, []string{auth.Provider}, cliproxyexecutor.Request{
		Model:   "public-model",
		Payload: payload,
	}, cliproxyexecutor.Options{SourceFormat: sdktranslator.FromString("openai"), OriginalRequest: payload})
	if err == nil {
		t.Fatal("deadline send returned success")
	}
	if sendsA.Load() != 1 || sendsB.Load() != 0 {
		t.Fatalf("deadline sends A=%d B=%d err=%v", sendsA.Load(), sendsB.Load(), err)
	}
	if host.unavailable.Load() {
		t.Fatal("caller deadline marked the process unavailable")
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 1 || results.items[0].Outcome != "deadline" || results.items[0].ErrorCode != "deadline" {
		t.Fatalf("deadline wire = %+v seq=%v admits=%d err=%v", results.items, results.seq, len(results.admits), err)
	}
}

func TestCountTokensIsAdmittedAndLocal(t *testing.T) {
	var upstreamHits atomic.Int32
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		upstreamHits.Add(1)
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		upstreamHits.Add(1)
		w.WriteHeader(http.StatusOK)
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	client := directClient()
	req, err := http.NewRequest(http.MethodPost, host.URL()+"/v1/messages/count_tokens", strings.NewReader(`{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	stampPolicyIdentity(req, newID(), "41", "7")
	resp, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	results.mu.Lock()
	httpResults := len(results.items)
	results.mu.Unlock()
	if httpResults == 0 {
		auth := compatAuth(t, host, "auth-ns")
		payload := []byte(`{"model":"public-model","messages":[{"role":"user","content":"hi"}]}`)
		_, err = host.service.CoreAuthManager().ExecuteCount(withRequestID(context.Background(), newID()), []string{auth.Provider}, cliproxyexecutor.Request{
			Model:   "public-model",
			Payload: payload,
		}, cliproxyexecutor.Options{SourceFormat: sdktranslator.FromString("openai"), OriginalRequest: payload})
		if err != nil {
			t.Fatalf("count tokens HTTP status=%d body=%s execute=%v", resp.StatusCode, raw, err)
		}
	}
	if upstreamHits.Load() != 0 {
		t.Fatalf("local count dialed upstream %d times; http=%d body=%s", upstreamHits.Load(), resp.StatusCode, raw)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) == 0 || results.items[0].Sent == nil || *results.items[0].Sent || results.items[0].Outcome != "success" || results.items[0].ErrorCode != "none" || results.items[0].Kind != "accepted" {
		t.Fatalf("count wire = %+v http=%d body=%s", results.items, resp.StatusCode, raw)
	}
}

func TestRegisterPreservesStampAndCapturedEpoch(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	upstreamA := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		once.Do(func() { close(started) })
		<-release
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstreamA.Close()
	upstreamB := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Errorf("credential B was contacted")
		w.WriteHeader(http.StatusOK)
	}))
	defer upstreamB.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host, _ := startSample(t, policy.URL, "7", "41", upstreamA.URL, upstreamB.URL)
	client := directClient()
	done := make(chan chatResult, 1)
	go func() {
		done <- chat(t, client, host, "41", "7", chatPayload)
	}()
	select {
	case <-started:
	case <-time.After(8 * time.Second):
		t.Fatal("in-flight send did not start")
	}
	auth := compatAuth(t, host, "auth-ns")
	before := auth.RegistrationEpoch
	clone := auth.Clone()
	delete(clone.Metadata, "ocg_credential_id")
	delete(clone.Attributes, "ocg_provider_id")
	delete(clone.Attributes, "websockets")
	if _, err := host.service.CoreAuthManager().Register(context.Background(), clone); err != nil {
		t.Fatal(err)
	}
	kept := compatAuth(t, host, "auth-ns")
	if kept.Metadata["ocg_credential_id"] != "cred-1" || kept.Attributes["ocg_provider_id"] != "canonical-provider" || kept.Attributes["websockets"] != "false" {
		t.Fatalf("resynthesis dropped the stamp: meta=%v attr=%v", kept.Metadata, kept.Attributes)
	}
	if kept.RegistrationEpoch <= before {
		t.Fatalf("registration epoch did not advance: %d -> %d", before, kept.RegistrationEpoch)
	}
	close(release)
	select {
	case first := <-done:
		if first.status == http.StatusServiceUnavailable {
			t.Fatalf("in-flight request became unavailable: %s", first.body)
		}
	case <-time.After(15 * time.Second):
		t.Fatal("in-flight request did not finish")
	}
	results.mu.Lock()
	if len(results.items) == 0 || results.items[0].RegistrationEpoch != strconv.FormatUint(before, 10) {
		epoch := ""
		if len(results.items) > 0 {
			epoch = results.items[0].RegistrationEpoch
		}
		results.mu.Unlock()
		t.Fatalf("in-flight result epoch = %s, captured %d", epoch, before)
	}
	results.mu.Unlock()
	second := chat(t, client, host, "41", "7", chatPayload)
	if second.status == http.StatusServiceUnavailable {
		t.Fatalf("post-register request was refused: %s", second.body)
	}
	results.mu.Lock()
	last := results.items[len(results.items)-1]
	results.mu.Unlock()
	if last.RegistrationEpoch != strconv.FormatUint(kept.RegistrationEpoch, 10) || last.RegistrationEpoch == strconv.FormatUint(before, 10) {
		t.Fatalf("next admission epoch = %s live=%d old=%d", last.RegistrationEpoch, kept.RegistrationEpoch, before)
	}
	readyBody := waitReady(t, client, host, "cred-1")
	authDir := host.AuthDir()
	if strings.Contains(readyBody, authDir) || strings.Contains(readyBody, "hop-secret") || strings.Contains(readyBody, "oauth-refresh") {
		t.Fatal("ready exposed a private path or secret after resynthesis")
	}
	var ready readyDocument
	if err := json.Unmarshal([]byte(readyBody), &ready); err != nil {
		t.Fatal(err)
	}
	for _, capability := range ready.Artifact.Capabilities {
		if strings.Contains(capability, "websocket") {
			t.Fatalf("websocket is claimed: %s", capability)
		}
	}
}

func assertResultBeforeNextAdmit(t *testing.T, log *policyLog) {
	t.Helper()
	log.mu.Lock()
	defer log.mu.Unlock()
	firstResult := -1
	secondAdmit := -1
	admits := 0
	for i, item := range log.seq {
		if item == "result" && firstResult < 0 {
			firstResult = i
		}
		if item == "admit" {
			admits++
			if admits == 2 {
				secondAdmit = i
			}
		}
	}
	if firstResult < 0 || secondAdmit < 0 || secondAdmit < firstResult {
		t.Fatalf("publication order = %v", log.seq)
	}
}

func compatAuth(t *testing.T, host *Host, name string) *coreauth.Auth {
	t.Helper()
	for _, auth := range host.service.CoreAuthManager().List() {
		if auth != nil && auth.Attributes != nil && auth.Attributes["compat_name"] == name {
			return auth
		}
	}
	t.Fatalf("compat auth %s was not registered", name)
	return nil
}

func startMutated(t *testing.T, policyURL, upstreamA, upstreamB string, mutate func(string) string) *Host {
	t.Helper()
	dir := testWorkDir(t)
	port := freeListenPort(t)
	body := sampleConfig(strconv.Itoa(port), "7", "41", policyURL)
	body = strings.ReplaceAll(body, "auth-dir: AUTHDIR", "auth-dir: "+yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.ReplaceAll(body, "http://127.0.0.1:19001/v1", upstreamA+"/v1")
	body = strings.ReplaceAll(body, "http://127.0.0.1:19002/v1", upstreamB+"/v1")
	body = strings.ReplaceAll(body, "origin: http://127.0.0.1:9", "origin: http://127.0.0.1")
	if mutate != nil {
		body = mutate(body)
	}
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
	return host
}

func streamChat(t *testing.T, client *http.Client, host *Host, payload string) chatResult {
	t.Helper()
	return streamChatContext(t, client, host, context.Background(), payload)
}

func streamChatContext(t *testing.T, client *http.Client, host *Host, ctx context.Context, payload string) chatResult {
	t.Helper()
	streamPayload := strings.TrimSuffix(payload, "}") + `,"stream":true}`
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, host.URL()+"/v1/chat/completions", strings.NewReader(streamPayload))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	stampPolicyIdentity(req, newID(), "41", "7")
	resp, err := client.Do(req)
	if err != nil {
		return chatResult{status: 0, body: err.Error()}
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return chatResult{status: resp.StatusCode, body: string(raw)}
}
