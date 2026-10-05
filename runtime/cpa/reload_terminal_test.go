package main

import (
	"context"
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

func TestReloadStampBlocksHeldFollowup(t *testing.T) {
	t.Run("next-credential", func(t *testing.T) {
		old := newHeldReject(t)
		defer old.close()
		next := newCountedUpstream(t, http.StatusOK, okBody)
		defer next.close()
		candidate := newCountedUpstream(t, http.StatusOK, okBody)
		defer candidate.close()
		moved := newCountedUpstream(t, http.StatusOK, okBody)
		defer moved.close()
		// next exists before the request so this logical request can select it.
		// Reload then points that same credential at moved.
		window := beginStampWindow(t, old, []stampAuth{
			{name: "changed", key: "same-key", base: old.url, priority: 10, routes: []stampRoute{{endpoint: old.url + "/v1/chat/completions", upstream: "upstream-model"}}},
			{name: "next", key: "next-key", base: next.url, priority: 1, routes: []stampRoute{{endpoint: next.url + "/v1/chat/completions", upstream: "next-model"}}},
		}, []stampAuth{
			{name: "changed", key: "same-key", base: candidate.url, priority: 10, routes: []stampRoute{{endpoint: candidate.url + "/v1/chat/completions", upstream: "candidate-model"}}},
			{name: "next", key: "next-key", base: moved.url, priority: 1, routes: []stampRoute{{endpoint: moved.url + "/v1/chat/completions", upstream: "next-model"}}},
		})
		window.waitInstalled(t, candidate.url, moved.url)
		if next.hits.Load() != 0 || candidate.hits.Load() != 0 || moved.hits.Load() != 0 || old.hits.Load() != 1 {
			t.Fatalf("follow-up started early old=%d next=%d candidate=%d moved=%d", old.hits.Load(), next.hits.Load(), candidate.hits.Load(), moved.hits.Load())
		}
		window.assertIngressClosed(t)
		window.release()
		heldResult := window.waitHeld(t)
		if !strings.Contains(heldResult.body, "apply_barrier") {
			t.Fatalf("next credential was not refused at the send boundary status=%d body=%s", heldResult.status, heldResult.body)
		}
		window.assertSingleOldRejection(t)
		if next.hits.Load() != 0 || candidate.hits.Load() != 0 || moved.hits.Load() != 0 || old.hits.Load() != 1 {
			t.Fatalf("follow-up escaped old=%d next=%d candidate=%d moved=%d barrier=%v", old.hits.Load(), next.hits.Load(), candidate.hits.Load(), moved.hits.Load(), window.host.applyBarrier.Load())
		}
		window.writeHoldAuth(t)
		outcome := window.waitReload(t)
		if outcome.status != "applied" || outcome.err != nil || window.host.revisionText() != "8" {
			t.Fatalf("commit status=%s err=%v revision=%s barrier=%v ready=%v unavailable=%v", outcome.status, outcome.err, window.host.revisionText(), window.host.applyBarrier.Load(), window.host.policyReady.Load(), window.host.unavailable.Load())
		}
		got := chat(t, directClient(), window.host, "41", "8", chatPayload)
		if got.status >= 500 || candidate.hits.Load()+moved.hits.Load() != 1 || next.hits.Load() != 0 || old.hits.Load() != 1 {
			t.Fatalf("post-commit status=%d old=%d next=%d candidate=%d moved=%d body=%s", got.status, old.hits.Load(), next.hits.Load(), candidate.hits.Load(), moved.hits.Load(), got.body)
		}
		contacted := moved
		wantModel := "next-model"
		if candidate.hits.Load() == 1 {
			contacted = candidate
			wantModel = "candidate-model"
		}
		if !strings.Contains(contacted.body(), wantModel) {
			t.Fatalf("post-commit model %s body=%s", wantModel, contacted.body())
		}
		window.assertCommitAdmit(t, "8")
	})

	t.Run("internal-resend", func(t *testing.T) {
		old := newHeldReject(t)
		defer old.close()
		follow := newCountedUpstream(t, http.StatusOK, okBody)
		defer follow.close()
		candidate := newCountedUpstream(t, http.StatusOK, okBody)
		defer candidate.close()
		initial := []stampAuth{{
			name: "routed", key: "route-key", base: old.url, priority: 10,
			routes: []stampRoute{
				{endpoint: old.url + "/v1/chat/completions", upstream: "upstream-model"},
				{endpoint: follow.url + "/v1/chat/completions", upstream: "upstream-model"},
			},
		}}
		reloaded := []stampAuth{{
			name: "routed", key: "route-key", base: old.url, priority: 10,
			routes: []stampRoute{
				{endpoint: old.url + "/v1/chat/completions", upstream: "candidate-model"},
				{endpoint: candidate.url + "/v1/chat/completions", upstream: "candidate-model"},
			},
		}}
		window := beginStampWindow(t, old, initial, reloaded)
		window.waitInstalled(t, candidate.url)
		if follow.hits.Load() != 0 {
			t.Fatalf("second route ran before the held rejection follow=%d", follow.hits.Load())
		}
		window.assertIngressClosed(t)
		window.release()
		heldResult := window.waitHeld(t)
		if !strings.Contains(heldResult.body, "apply_barrier") {
			t.Fatalf("internal resend was not refused at the send boundary status=%d body=%s", heldResult.status, heldResult.body)
		}
		window.assertSingleOldRejection(t)
		if follow.hits.Load() != 0 || candidate.hits.Load() != 0 || old.hits.Load() != 1 || !window.host.applyBarrier.Load() {
			t.Fatalf("internal resend escaped old=%d follow=%d candidate=%d barrier=%v", old.hits.Load(), follow.hits.Load(), candidate.hits.Load(), window.host.applyBarrier.Load())
		}
		window.writeHoldAuth(t)
		outcome := window.waitReload(t)
		if outcome.status != "applied" || outcome.err != nil {
			t.Fatalf("commit status=%s err=%v", outcome.status, outcome.err)
		}
		got := chat(t, directClient(), window.host, "41", "8", chatPayload)
		if old.hits.Load() != 2 || candidate.hits.Load() != 1 || follow.hits.Load() != 0 || !strings.Contains(got.body, "ocg attempt stop") {
			t.Fatalf("post-commit internal status=%d old=%d follow=%d candidate=%d body=%s", got.status, old.hits.Load(), follow.hits.Load(), candidate.hits.Load(), got.body)
		}
		if !strings.Contains(candidate.body(), "candidate-model") {
			t.Fatalf("candidate model body=%s", candidate.body())
		}
		window.assertCommitAdmit(t, "8")
		window.assertRevisionSuccess(t, "8", "candidate-model")
	})

	t.Run("rollback", func(t *testing.T) {
		old := newHeldReject(t)
		defer old.close()
		follow := newCountedUpstream(t, http.StatusOK, okBody)
		defer follow.close()
		candidate := newCountedUpstream(t, http.StatusOK, okBody)
		defer candidate.close()
		initial := []stampAuth{{
			name: "routed", key: "route-key", base: old.url, priority: 10,
			routes: []stampRoute{
				{endpoint: old.url + "/v1/chat/completions", upstream: "upstream-model"},
				{endpoint: follow.url + "/v1/chat/completions", upstream: "upstream-model"},
			},
		}}
		reloaded := []stampAuth{{
			name: "routed", key: "route-key", base: old.url, priority: 10,
			routes: []stampRoute{
				{endpoint: old.url + "/v1/chat/completions", upstream: "candidate-model"},
				{endpoint: candidate.url + "/v1/chat/completions", upstream: "candidate-model"},
			},
		}}
		window := beginStampWindow(t, old, initial, reloaded)
		window.waitInstalled(t, candidate.url)
		window.release()
		heldResult := window.waitHeld(t)
		if !strings.Contains(heldResult.body, "apply_barrier") {
			t.Fatalf("rollback follow-up was not refused at the send boundary status=%d body=%s", heldResult.status, heldResult.body)
		}
		window.assertSingleOldRejection(t)
		if candidate.hits.Load() != 0 || follow.hits.Load() != 0 || old.hits.Load() != 1 {
			t.Fatalf("rollback window old=%d follow=%d candidate=%d", old.hits.Load(), follow.hits.Load(), candidate.hits.Load())
		}
		outcome := window.waitReload(t)
		if outcome.status != "rollback" || window.host.revisionText() != "7" {
			t.Fatalf("rollback status=%s err=%v revision=%s barrier=%v", outcome.status, outcome.err, window.host.revisionText(), window.host.applyBarrier.Load())
		}
		// The config watcher applies the same file once more and closes ingress again.
		deadline := time.Now().Add(20 * time.Second)
		stable := false
		for time.Now().Before(deadline) {
			if candidate.hits.Load() != 0 || follow.hits.Load() != 0 || old.hits.Load() != 1 {
				t.Fatalf("rollback window escaped old=%d follow=%d candidate=%d", old.hits.Load(), follow.hits.Load(), candidate.hits.Load())
			}
			open := !window.host.applyBarrier.Load() && window.host.policyReady.Load() && !window.host.unavailable.Load() && window.host.revisionText() == "7"
			if !open {
				time.Sleep(50 * time.Millisecond)
				continue
			}
			time.Sleep(300 * time.Millisecond)
			if !window.host.applyBarrier.Load() && window.host.policyReady.Load() && !window.host.unavailable.Load() && window.host.revisionText() == "7" {
				stable = true
				break
			}
		}
		if !stable {
			t.Fatalf("rollback gate stayed closed status=%s revision=%s barrier=%v ready=%v unavailable=%v", window.host.statusText(), window.host.revisionText(), window.host.applyBarrier.Load(), window.host.policyReady.Load(), window.host.unavailable.Load())
		}
		ready := waitReady(t, directClient(), window.host, `"applyStatus":"rollback"`)
		if !strings.Contains(ready, `"appliedProjectionRevision":"7"`) || !strings.Contains(ready, window.originalDigest) {
			t.Fatalf("rollback ready %s", ready)
		}
		got := chat(t, directClient(), window.host, "41", "7", chatPayload)
		if old.hits.Load() != 2 || follow.hits.Load() != 1 || candidate.hits.Load() != 0 || !strings.Contains(got.body, "ocg attempt stop") {
			t.Fatalf("restored routes status=%d old=%d follow=%d candidate=%d body=%s", got.status, old.hits.Load(), follow.hits.Load(), candidate.hits.Load(), got.body)
		}
		if !strings.Contains(follow.body(), "upstream-model") || strings.Contains(follow.body(), "candidate-model") {
			t.Fatalf("restored follow-up model body=%s", follow.body())
		}
		window.assertRevisionSuccess(t, "7", "upstream-model")
	})
}

func TestAntigravityContentMarkerIsUncertain(t *testing.T) {
	model := "gemini-3.7-flash-high"
	payload := []byte(`{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}`)
	opts := cliproxyexecutor.Options{SourceFormat: sdktranslator.FromString("gemini"), OriginalRequest: payload}
	content := "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"response.completed\"},{\"text\":\"\\\"message_stop\\\"\"},{\"functionCall\":{\"name\":\"tool\",\"args\":{\"query\":\"message_stop response.completed\"}}}]}}],\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":9}}}\n\n" +
		"data: {\"type\":\"response.completed\"\n\n" +
		"data: raw response.completed \"message_stop\"\n\n" +
		"data: {\"choices\":[{\"delta\":{\"content\":\"data: [DONE]\"}}]}\n\n"
	item, hits, replay := runAntigravityStream(t, model, payload, opts, content, true)
	if hits != 1 || replay != 0 || item.Outcome != "uncertain" || item.ErrorCode != "stream_lost" || boolValue(item.BodyComplete) || item.ReportedUsage == nil || !strings.Contains(string(item.ReportedUsage), `"inputTokens":8`) || !strings.Contains(string(item.ReportedUsage), `"outputTokens":9`) {
		t.Fatalf("content marker hits=%d replay=%d item=%+v", hits, replay, item)
	}

	malformed := "data: {\"type\":\"response.completed\"\n\ndata: raw response.completed \"message_stop\"\n\n"
	broken, brokenHits, brokenReplay := runAntigravityStream(t, model, payload, opts, malformed, false)
	if brokenHits != 1 || brokenReplay != 0 || broken.Outcome != "uncertain" || boolValue(broken.BodyComplete) || broken.ReportedUsage != nil {
		t.Fatalf("malformed terminal hits=%d replay=%d item=%+v", brokenHits, brokenReplay, broken)
	}
}

func TestStructuredStreamTerminalsComplete(t *testing.T) {
	model := "gemini-3.7-flash-high"
	payload := []byte(`{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}`)
	opts := cliproxyexecutor.Options{SourceFormat: sdktranslator.FromString("gemini"), OriginalRequest: payload}
	completed := "event: response.completed\ndata: {\"usage\":{\"input_tokens\":5,\"output_tokens\":6}}\n\n"
	assertAntigravityExchange(t, model, payload, opts, false, completed, func(item policyRequest, hits int32) {
		if hits != 1 || item.Outcome != "success" || !boolValue(item.BodyComplete) || !boolValue(item.StreamStarted) || !strings.Contains(string(item.ReportedUsage), `"inputTokens":5`) || !strings.Contains(string(item.ReportedUsage), `"outputTokens":6`) {
			t.Fatalf("response.completed event hits=%d item=%+v", hits, item)
		}
	})
	stopped := "event: message_stop\ndata: {\"usage\":{\"input_tokens\":7,\"output_tokens\":8}}\n\n"
	assertAntigravityExchange(t, model, payload, opts, false, stopped, func(item policyRequest, hits int32) {
		if hits != 1 || item.Outcome != "success" || !boolValue(item.BodyComplete) || !strings.Contains(string(item.ReportedUsage), `"inputTokens":7`) || !strings.Contains(string(item.ReportedUsage), `"outputTokens":8`) {
			t.Fatalf("message_stop event hits=%d item=%+v", hits, item)
		}
	})
	typed := "data: {\"type\":\"message_stop\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n\n"
	assertAntigravityExchange(t, model, payload, opts, false, typed, func(item policyRequest, hits int32) {
		if hits != 1 || item.Outcome != "success" || !boolValue(item.BodyComplete) || !strings.Contains(string(item.ReportedUsage), `"inputTokens":1`) || !strings.Contains(string(item.ReportedUsage), `"outputTokens":2`) {
			t.Fatalf("message_stop field hits=%d item=%+v", hits, item)
		}
	})
}

type stampRoute struct {
	endpoint string
	upstream string
}

type stampAuth struct {
	name     string
	key      string
	base     string
	priority int
	routes   []stampRoute
}

type countedUpstream struct {
	url    string
	server *httptest.Server
	hits   atomic.Int32
	mu     sync.Mutex
	seen   strings.Builder
}

func newCountedUpstream(t *testing.T, status int, body string) *countedUpstream {
	t.Helper()
	up := &countedUpstream{}
	up.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		up.mu.Lock()
		up.seen.Write(raw)
		up.mu.Unlock()
		up.hits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = w.Write([]byte(body))
	}))
	up.url = up.server.URL
	return up
}

func (c *countedUpstream) close() {
	if c != nil && c.server != nil {
		c.server.Close()
	}
}

func (c *countedUpstream) body() string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.seen.String()
}

type heldReject struct {
	countedUpstream
	releaseOnce sync.Once
	releaseCh   chan struct{}
	started     chan struct{}
}

func newHeldReject(t *testing.T) *heldReject {
	t.Helper()
	held := &heldReject{releaseCh: make(chan struct{}), started: make(chan struct{})}
	held.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		held.mu.Lock()
		held.seen.Write(raw)
		held.mu.Unlock()
		n := held.hits.Add(1)
		if n == 1 {
			close(held.started)
			select {
			case <-held.releaseCh:
			case <-r.Context().Done():
				return
			}
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = w.Write([]byte(`{"error":{"message":"unauthorized-reload"}}`))
	}))
	held.url = held.server.URL
	return held
}

func (h *heldReject) release() {
	h.releaseOnce.Do(func() { close(h.releaseCh) })
}

type reloadOutcome struct {
	status string
	err    error
}

type stampWindow struct {
	t              *testing.T
	host           *Host
	results        *policyLog
	dir            string
	configPath     string
	originalDigest string
	reloadDone     chan reloadOutcome
	finished       chan struct{}
	heldDone       chan chatResult
	release        func()
}

func beginStampWindow(t *testing.T, held *heldReject, initial, reloaded []stampAuth) *stampWindow {
	t.Helper()
	policy, results := strictPolicy(t, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusUnauthorized {
			return "skip"
		}
		return "stop"
	})
	t.Cleanup(policy.Close)
	dir := testWorkDir(t)
	if err := os.MkdirAll(filepath.FromSlash(absoluteAuthDir(t, dir)), 0o700); err != nil {
		t.Fatal(err)
	}
	port := freeListenPort(t)
	configPath := writeConfig(t, dir, compatConfig(t, dir, port, policy.URL, "7", initial, false))
	originalDigest := shaFile(t, configPath)
	host := startConfig(t, configPath)
	_ = waitReady(t, directClient(), host, `"appliedProjectionRevision":"7"`)
	window := &stampWindow{
		t:              t,
		host:           host,
		results:        results,
		dir:            dir,
		configPath:     configPath,
		originalDigest: originalDigest,
		reloadDone:     make(chan reloadOutcome, 1),
		finished:       make(chan struct{}),
		heldDone:       make(chan chatResult, 1),
		release:        held.release,
	}
	go func() {
		window.heldDone <- chat(t, directClient(), host, "41", "7", chatPayload)
	}()
	select {
	case <-held.started:
	case <-time.After(8 * time.Second):
		t.Fatal("held provider request did not start")
	}
	if held.hits.Load() != 1 {
		t.Fatalf("held provider hits=%d before reload", held.hits.Load())
	}
	replaceConfig(t, configPath, compatConfig(t, dir, port, policy.URL, "8", reloaded, true))
	go func() {
		defer close(window.finished)
		status, err := host.Reload(context.Background(), configPath)
		window.reloadDone <- reloadOutcome{status: status, err: err}
	}()
	t.Cleanup(func() {
		held.release()
		select {
		case <-window.finished:
		case <-time.After(20 * time.Second):
		}
	})
	return window
}

func (w *stampWindow) waitInstalled(t *testing.T, needles ...string) {
	t.Helper()
	deadline := time.Now().Add(8 * time.Second)
	for time.Now().Before(deadline) {
		snapshot := managerSnapshot(w.host)
		staged := stagedRevision(w.host)
		if w.host.applyBarrier.Load() && w.host.revisionText() == "7" && staged == "8" && containsAll(snapshot, needles) {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("stamp hold missing barrier=%v applied=%s staged=%s snapshot=%s", w.host.applyBarrier.Load(), w.host.revisionText(), stagedRevision(w.host), managerSnapshot(w.host))
}

func (w *stampWindow) assertIngressClosed(t *testing.T) {
	t.Helper()
	blocked := chat(t, directClient(), w.host, "41", "7", chatPayload)
	if blocked.status != http.StatusServiceUnavailable || !w.host.applyBarrier.Load() {
		t.Fatalf("ingress during stamp status=%d body=%s barrier=%v", blocked.status, blocked.body, w.host.applyBarrier.Load())
	}
}

func (w *stampWindow) waitHeld(t *testing.T) chatResult {
	t.Helper()
	select {
	case got := <-w.heldDone:
		if got.status == 0 && got.body == "" {
			t.Fatal("held request produced an empty result")
		}
		return got
	case <-time.After(15 * time.Second):
		t.Fatal("held request did not finish")
		return chatResult{}
	}
}

func (w *stampWindow) assertSingleOldRejection(t *testing.T) {
	t.Helper()
	if !w.host.applyBarrier.Load() || w.host.revisionText() != "7" {
		t.Fatalf("barrier dropped before the assertion applied=%s barrier=%v", w.host.revisionText(), w.host.applyBarrier.Load())
	}
	w.results.mu.Lock()
	defer w.results.mu.Unlock()
	if strings.Join(w.results.seq, ",") != "admit,result" || len(w.results.items) != 1 || len(w.results.admits) != 1 {
		t.Fatalf("barrier policy seq=%v admits=%d items=%+v", w.results.seq, len(w.results.admits), w.results.items)
	}
	item := w.results.items[0]
	var admit policyRequest
	for _, value := range w.results.admits {
		admit = value
	}
	if item.Outcome != "explicit_rejection" || item.ErrorCode != "provider_rejected" || !boolValue(item.Sent) || !boolValue(item.BodyComplete) || boolValue(item.StreamStarted) || item.Status == nil || *item.Status != http.StatusUnauthorized || !strings.Contains(item.ResponseBody, "unauthorized-reload") || item.Observation == nil {
		t.Fatalf("held rejection %+v", item)
	}
	if item.ProjectionRevision != "7" || item.ProjectionDigest != w.originalDigest || item.ProcessGeneration != "41" || admit.ProjectionRevision != "7" || admit.ProjectionDigest != w.originalDigest {
		t.Fatalf("old admission was not retained item=%+v admit=%+v", item, admit)
	}
	if admit.UpstreamModel == "candidate-model" || admit.UpstreamModel == "next-model" || strings.Contains(admit.CredentialID, "next") {
		t.Fatalf("old revision admitted candidate state %+v", admit)
	}
}

func (w *stampWindow) assertRevisionSuccess(t *testing.T, revision, upstream string) {
	t.Helper()
	w.results.mu.Lock()
	defer w.results.mu.Unlock()
	for _, item := range w.results.items {
		if item.ProjectionRevision == revision && item.Outcome == "success" && item.UpstreamModel == upstream && boolValue(item.Sent) && boolValue(item.BodyComplete) {
			return
		}
	}
	t.Fatalf("missing %s success on revision %s %+v", upstream, revision, w.results.items)
}

func (w *stampWindow) assertCommitAdmit(t *testing.T, revision string) {
	t.Helper()
	w.results.mu.Lock()
	defer w.results.mu.Unlock()
	saw := false
	for _, item := range w.results.admits {
		if item.ProjectionRevision == revision {
			saw = true
		}
		if item.ProjectionRevision == "7" && (item.UpstreamModel == "candidate-model" || item.UpstreamModel == "next-model") {
			t.Fatalf("old revision authorized candidate model %+v", item)
		}
	}
	if !saw {
		t.Fatalf("missing commit admit %s %+v", revision, w.results.admits)
	}
}

func (w *stampWindow) writeHoldAuth(t *testing.T) {
	t.Helper()
	raw := []byte(`{"type":"codex","access_token":"hold-token","expired":"` + time.Now().Add(48*time.Hour).UTC().Format(time.RFC3339) + `","base_url":"http://127.0.0.1:9"}`)
	path := filepath.Join(filepath.FromSlash(absoluteAuthDir(t, w.dir)), "hold-stamp.json")
	if err := os.WriteFile(path, raw, 0o600); err != nil {
		t.Fatal(err)
	}
}

func (w *stampWindow) waitReload(t *testing.T) reloadOutcome {
	t.Helper()
	select {
	case outcome := <-w.reloadDone:
		return outcome
	case <-time.After(25 * time.Second):
		t.Fatal("reload did not finish")
		return reloadOutcome{}
	}
}

func compatConfig(t *testing.T, dir string, port int, policyURL, revision string, auths []stampAuth, holdStamp bool) string {
	t.Helper()
	body := baseConfig(port, policyURL, "fill-first", false, false, "")
	body = strings.Replace(body, "request-retry: 0\n", "request-retry: 0\ndisable-cooling: true\n", 1)
	body = strings.ReplaceAll(body, "AUTHDIR", yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.ReplaceAll(body, `projection-revision: "7"`, `projection-revision: "`+revision+`"`)
	var compat, creds strings.Builder
	compat.WriteString("openai-compatibility:\n")
	creds.WriteString("  credentials:\n")
	for _, auth := range auths {
		compat.WriteString("  - name: " + auth.name + "\n")
		compat.WriteString("    priority: " + strconv.Itoa(auth.priority) + "\n")
		compat.WriteString("    base-url: " + auth.base + "/v1\n")
		compat.WriteString("    api-key-entries:\n      - api-key: " + auth.key + "\n")
		compat.WriteString("    models:\n")
		seen := map[string]bool{}
		for _, route := range auth.routes {
			if seen[route.upstream] {
				continue
			}
			seen[route.upstream] = true
			compat.WriteString("      - name: " + route.upstream + "\n        alias: public-model\n")
		}
		creds.WriteString("    - namespace: " + auth.name + "\n")
		creds.WriteString("      auth-id: " + auth.name + "\n")
		creds.WriteString("      credential-id: cred-" + auth.name + "\n")
		creds.WriteString("      credential-version: \"3\"\n")
		creds.WriteString("      binding-id: bind-" + auth.name + "\n")
		creds.WriteString("      material-revision: material-" + auth.name + "\n")
		creds.WriteString("      provider-id: canonical-provider\n")
		creds.WriteString("      opaque-remote: false\n")
		creds.WriteString("      routes:\n")
		for _, route := range auth.routes {
			creds.WriteString("        - public-model: public-model\n")
			creds.WriteString("          upstream-model: " + route.upstream + "\n")
			creds.WriteString("          protocol: chat_completions\n")
			creds.WriteString("          endpoint: " + route.endpoint + "\n")
			creds.WriteString("          auth-scheme: bearer\n")
			creds.WriteString("          request-identity: none\n")
			creds.WriteString("          wire: none\n")
		}
	}
	body = strings.Replace(body, "openai-compatibility: []\n", compat.String(), 1)
	body = strings.Replace(body, "  credentials: []\n", creds.String(), 1)
	if holdStamp {
		body = strings.Replace(body, "  oauth-bindings: []\n", "  oauth-bindings:\n    - relative-path: hold-stamp.json\n      credential-id: oauth-hold\n      credential-version: \"4\"\n      priority: \"1\"\n      provider-id: canonical-oauth\n      models: [\"hold-model\"]\n", 1)
	}
	return body
}

func stagedRevision(host *Host) string {
	host.mu.RLock()
	defer host.mu.RUnlock()
	return host.document.ProjectionRevision.String()
}

func managerSnapshot(host *Host) string {
	manager := host.service.CoreAuthManager()
	if manager == nil {
		return ""
	}
	var b strings.Builder
	for _, auth := range manager.List() {
		if auth == nil || auth.Attributes == nil {
			continue
		}
		b.WriteString(auth.ID)
		b.WriteByte('\n')
		b.WriteString(auth.Attributes["base_url"])
		b.WriteByte('\n')
		b.WriteString(auth.Attributes["ocg_protocol_routes"])
		b.WriteByte('\n')
	}
	return b.String()
}

func containsAll(text string, needles []string) bool {
	for _, needle := range needles {
		if !strings.Contains(text, needle) {
			return false
		}
	}
	return true
}

func runAntigravityStream(t *testing.T, model string, payload []byte, opts cliproxyexecutor.Options, response string, replay bool) (policyRequest, int32, int32) {
	t.Helper()
	var hits atomic.Int32
	var replayHits atomic.Int32
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !antigravityGeneration(r) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"models":{}}`))
			return
		}
		hits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(response))
	}))
	defer upstream.Close()
	replayURL := upstream.URL
	if replay {
		shadow := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if !antigravityGeneration(r) {
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`{"models":{}}`))
				return
			}
			replayHits.Add(1)
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write([]byte(`{"response":{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"replayed"}]}}]}}`))
		}))
		defer shadow.Close()
		replayURL = shadow.URL
	}
	policy, results := strictPolicy(t, func(policyRequest) string { return "skip" })
	defer policy.Close()
	bindings := []nativeBinding{{
		file: "ag.json", id: "oauth-ag", version: "4", priority: "10", provider: "canonical-oauth",
		models: []string{model}, access: "ag-token", baseURL: upstream.URL, kind: "antigravity", project: "project-1",
	}}
	if replay {
		bindings = append(bindings, nativeBinding{
			file: "ag-replay.json", id: "oauth-replay", version: "4", priority: "1", provider: "canonical-oauth",
			models: []string{model}, access: "replay-token", baseURL: replayURL, kind: "antigravity", project: "project-1",
		})
	}
	host := startNative(t, policy.URL, nativeSpec{strategy: "fill-first", bindings: bindings})
	_ = waitReady(t, directClient(), host, "oauth-ag")
	stream, err := host.service.CoreAuthManager().ExecuteStream(withRequestID(context.Background(), newID()), []string{"antigravity"}, cliproxyexecutor.Request{Model: model, Payload: payload}, opts)
	if stream != nil && stream.Chunks != nil {
		for chunk := range stream.Chunks {
			if chunk.Err != nil && err == nil {
				err = chunk.Err
			}
		}
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 1 || len(results.admits) != 1 {
		t.Fatalf("stream hits=%d replay=%d err=%v admits=%d items=%+v", hits.Load(), replayHits.Load(), err, len(results.admits), results.items)
	}
	return results.items[0], hits.Load(), replayHits.Load()
}
