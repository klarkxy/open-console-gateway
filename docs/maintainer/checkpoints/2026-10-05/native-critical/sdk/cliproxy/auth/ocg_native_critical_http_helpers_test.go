package auth_test

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/router-for-me/CLIProxyAPI/v8/internal/config"
	"github.com/router-for-me/CLIProxyAPI/v8/internal/registry"
	runtimeexecutor "github.com/router-for-me/CLIProxyAPI/v8/internal/runtime/executor"
	_ "github.com/router-for-me/CLIProxyAPI/v8/internal/translator"
	cliproxyauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/executor"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v8/sdk/translator"
)

const (
	nativeCriticalModel = "gpt-5.5"
	nativeCriticalSSE   = "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"yes\"}]}}\n\n" +
		"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_native\",\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
	nativeCriticalAuthErrorSSE = "data: {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"code\":\"unauthorized\",\"message\":\"invalid token\"}}\n\n"
	nativeCriticalCompactJSON  = `{"id":"resp_compact","object":"response.compaction","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}`
	nativeCriticalWait         = 5 * time.Second
)

type recordedHTTP struct {
	method    string
	scheme    string
	host      string
	path      string
	query     string
	absolute  string
	canonical string
	auth      string
	canonErr  error
}

type recordedDecision struct {
	kind      string
	attemptID string
	action    string
	reason    string
	identity  cliproxyauth.GenerationIdentity
	ctx       context.Context
}

type recordedResult struct {
	kind                         string
	attemptID                    string
	identity                     cliproxyauth.GenerationIdentity
	result                       cliproxyauth.Result
	matched                      *cliproxyauth.EndpointPin
	sent, bodyComplete, streamed bool
	status                       int
	hasStatus                    bool
	transport, body              string
	ctx                          context.Context
}

type recordingBoundary struct {
	mu        sync.Mutex
	httpPins  []cliproxyauth.EndpointPin
	empty     map[string]bool
	explicit  map[string][]cliproxyauth.EndpointPin
	refuse    map[string]int
	n         atomic.Int32
	kind      []string
	id        []string
	auth      []cliproxyauth.GenerationIdentity
	captured  map[string]context.Context
	results   []recordedResult
	decisions []recordedDecision
	hook      func(ctx context.Context, auth *cliproxyauth.Auth, kind string)
}

func (d *recordingBoundary) pinsFor(kind string) []cliproxyauth.EndpointPin {
	d.mu.Lock()
	defer d.mu.Unlock()
	if pins, ok := d.explicit[kind]; ok {
		return append([]cliproxyauth.EndpointPin(nil), pins...)
	}
	if d.empty[kind] || kind == cliproxyauth.GenerationKindCount {
		return []cliproxyauth.EndpointPin{}
	}
	return append([]cliproxyauth.EndpointPin(nil), d.httpPins...)
}

func (d *recordingBoundary) setEmpty(kind string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.empty == nil {
		d.empty = map[string]bool{}
	}
	d.empty[kind] = true
}

func (d *recordingBoundary) setExplicit(kind string, pins []cliproxyauth.EndpointPin) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.explicit == nil {
		d.explicit = map[string][]cliproxyauth.EndpointPin{}
	}
	d.explicit[kind] = append([]cliproxyauth.EndpointPin(nil), pins...)
}

func (d *recordingBoundary) refuseKind(kind string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.refuse == nil {
		d.refuse = map[string]int{}
	}
	d.refuse[kind]++
}

func (d *recordingBoundary) BeforeSend(ctx context.Context, auth *cliproxyauth.Auth, kind, _, _ string) (cliproxyauth.GenerationDecision, error) {
	n := d.n.Add(1)
	id := "attempt-" + itoa(int(n))
	d.mu.Lock()
	refuse := d.refuse[kind] > 0
	if refuse {
		d.refuse[kind]--
	}
	d.kind = append(d.kind, kind)
	d.id = append(d.id, id)
	d.auth = append(d.auth, cliproxyauth.CaptureGenerationIdentity(auth))
	if d.captured == nil {
		d.captured = map[string]context.Context{}
	}
	d.captured[kind] = ctx
	hook := d.hook
	d.mu.Unlock()
	if hook != nil {
		hook(ctx, auth, kind)
	}
	action, reason := "allow", "eligible"
	if refuse {
		action, reason = "stop", "current_authority_denied"
	}
	d.mu.Lock()
	d.decisions = append(d.decisions, recordedDecision{
		kind:      kind,
		attemptID: id,
		action:    action,
		reason:    reason,
		identity:  cliproxyauth.CaptureGenerationIdentity(auth),
		ctx:       ctx,
	})
	d.mu.Unlock()
	if refuse {
		return cliproxyauth.GenerationDecision{Action: action, Reason: reason, AttemptID: id}, nil
	}
	return cliproxyauth.GenerationDecision{
		Action:       action,
		Reason:       reason,
		AttemptID:    id,
		EndpointPins: d.pinsFor(kind),
	}, nil
}

func (d *recordingBoundary) AfterResult(ctx context.Context, auth *cliproxyauth.Auth, kind, _, _ string, result *cliproxyauth.Result) error {
	item := recordedResult{kind: kind, attemptID: cliproxyauth.CurrentAttemptID(ctx), ctx: ctx}
	item.identity = cliproxyauth.CaptureGenerationIdentity(auth)
	if result != nil {
		item.result = *result
	}
	item.matched = cliproxyauth.MatchedEndpointPin(ctx)
	item.sent, item.bodyComplete, item.streamed, item.status, item.hasStatus, item.transport, item.body, _, _ = cliproxyauth.GenerationFacts(ctx)
	d.mu.Lock()
	d.results = append(d.results, item)
	d.mu.Unlock()
	return nil
}

func (d *recordingBoundary) kinds() []string {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]string(nil), d.kind...)
}

func (d *recordingBoundary) attempts() []string {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]string(nil), d.id...)
}

func (d *recordingBoundary) identities() []cliproxyauth.GenerationIdentity {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]cliproxyauth.GenerationIdentity(nil), d.auth...)
}

func (d *recordingBoundary) contextFor(kind string) context.Context {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.captured[kind]
}

func (d *recordingBoundary) resultsCopy() []recordedResult {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]recordedResult(nil), d.results...)
}

func (d *recordingBoundary) lastResult(kind string) (recordedResult, bool) {
	d.mu.Lock()
	defer d.mu.Unlock()
	for i := len(d.results) - 1; i >= 0; i-- {
		if d.results[i].kind == kind {
			return d.results[i], true
		}
	}
	return recordedResult{}, false
}

func (d *recordingBoundary) decisionLen() int {
	d.mu.Lock()
	defer d.mu.Unlock()
	return len(d.decisions)
}

func (d *recordingBoundary) namedStopAfter(kind string, after int) (recordedDecision, bool) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if after < 0 {
		after = 0
	}
	for i := after; i < len(d.decisions); i++ {
		item := d.decisions[i]
		if item.kind == kind && item.action == "stop" && item.reason == "current_authority_denied" {
			return item, true
		}
	}
	return recordedDecision{}, false
}

func (d *recordingBoundary) decisionSummaries() []string {
	d.mu.Lock()
	defer d.mu.Unlock()
	out := make([]string, 0, len(d.decisions))
	for _, item := range d.decisions {
		out = append(out, item.kind+"/"+item.action+"/"+item.reason+"/"+item.attemptID+"/"+item.identity.CredentialID)
	}
	return out
}

type countingTransport struct {
	invoked, passed atomic.Int32
	next            http.RoundTripper
}

func (c *countingTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	c.invoked.Add(1)
	if req == nil {
		return nil, errors.New("missing request")
	}
	if err := cliproxyauth.BeforeOCGHTTPDispatch(req.Context(), req); err != nil {
		return nil, err
	}
	c.passed.Add(1)
	next := c.next
	if next == nil {
		next = http.DefaultTransport
	}
	return next.RoundTrip(req)
}

type countingUpstream struct {
	hits, backup, tokens atomic.Int32
	records              []recordedHTTP
	mu                   sync.Mutex
	onHit                func(n int32, w http.ResponseWriter, r *http.Request) bool
	success              func(w http.ResponseWriter)
}

func (c *countingUpstream) note(r *http.Request) int32 {
	n := c.hits.Add(1)
	c.mu.Lock()
	c.records = append(c.records, observeArrival(r))
	c.mu.Unlock()
	return n
}

func observeArrival(r *http.Request) recordedHTTP {
	rec := recordedHTTP{method: r.Method, auth: r.Header.Get("Authorization")}
	absolute, scheme, host, path, query, err := observedRequestAbsolute(r)
	rec.scheme, rec.host, rec.path, rec.query, rec.absolute = scheme, host, path, query, absolute
	if err != nil {
		rec.canonErr = err
		return rec
	}
	canonical, _, _, err := cliproxyauth.CanonicalDispatchURL(absolute)
	if err != nil {
		rec.canonErr = err
		return rec
	}
	rec.canonical = canonical
	return rec
}

func observedRequestAbsolute(r *http.Request) (absolute, scheme, host, path, query string, err error) {
	if r == nil || r.URL == nil {
		return "", "", "", "", "", errors.New("missing request url")
	}
	scheme = strings.ToLower(strings.TrimSpace(r.URL.Scheme))
	if scheme == "" {
		if r.TLS != nil {
			scheme = "https"
		} else {
			scheme = "http"
		}
	}
	host = strings.TrimSpace(r.Host)
	if host == "" {
		host = strings.TrimSpace(r.URL.Host)
	}
	if host == "" {
		return "", scheme, "", r.URL.EscapedPath(), r.URL.RawQuery, errors.New("missing request host")
	}
	path = r.URL.EscapedPath()
	if path == "" {
		path = "/"
	}
	query = r.URL.RawQuery
	absolute = scheme + "://" + host + path
	if r.URL.ForceQuery || query != "" {
		absolute += "?" + query
	}
	return absolute, scheme, host, path, query, nil
}

func (c *countingUpstream) captured() []recordedHTTP {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]recordedHTTP(nil), c.records...)
}

type holdGate struct {
	seen    chan struct{}
	release chan struct{}
	once    sync.Once
}

func newHoldGate() *holdGate {
	return &holdGate{seen: make(chan struct{}), release: make(chan struct{})}
}

func (g *holdGate) park() {
	g.once.Do(func() { close(g.seen) })
	select {
	case <-g.release:
	case <-time.After(nativeCriticalWait):
	}
}

func waitClosed(t *testing.T, ch <-chan struct{}, name string) {
	t.Helper()
	select {
	case <-ch:
	case <-time.After(nativeCriticalWait):
		t.Fatalf("timeout waiting for %s", name)
	}
}

type sequencedContext struct {
	parent      context.Context
	mu          sync.Mutex
	done        chan struct{}
	err         error
	deadline    time.Time
	hasDeadline bool
}

func newSequencedContext(parent context.Context) *sequencedContext {
	if parent == nil {
		parent = context.Background()
	}
	return &sequencedContext{parent: parent, done: make(chan struct{})}
}

func (c *sequencedContext) expire(err error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.err != nil {
		return
	}
	if err == nil {
		err = context.Canceled
	}
	c.err = err
	if errors.Is(err, context.DeadlineExceeded) {
		c.deadline = time.Now()
		c.hasDeadline = true
	}
	close(c.done)
}

func (c *sequencedContext) Deadline() (time.Time, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.hasDeadline {
		return c.deadline, true
	}
	return c.parent.Deadline()
}

func (c *sequencedContext) Done() <-chan struct{} { return c.done }

func (c *sequencedContext) Err() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.err != nil {
		return c.err
	}
	return c.parent.Err()
}

func (c *sequencedContext) Value(key any) any { return c.parent.Value(key) }

type observeCodex struct {
	inner         *runtimeexecutor.CodexExecutor
	beforeExecute func(ctx context.Context, auth *cliproxyauth.Auth) bool
}

func (e *observeCodex) Identifier() string { return e.inner.Identifier() }

func (e *observeCodex) Execute(ctx context.Context, auth *cliproxyauth.Auth, req cliproxyexecutor.Request, opts cliproxyexecutor.Options) (cliproxyexecutor.Response, error) {
	if e.beforeExecute != nil && e.beforeExecute(ctx, auth) {
		return cliproxyexecutor.Response{}, cliproxyauth.ErrAttemptStop
	}
	return e.inner.Execute(ctx, auth, req, opts)
}

func (e *observeCodex) ExecuteStream(ctx context.Context, auth *cliproxyauth.Auth, req cliproxyexecutor.Request, opts cliproxyexecutor.Options) (*cliproxyexecutor.StreamResult, error) {
	return e.inner.ExecuteStream(ctx, auth, req, opts)
}

func (e *observeCodex) Refresh(ctx context.Context, auth *cliproxyauth.Auth) (*cliproxyauth.Auth, error) {
	return e.inner.Refresh(ctx, auth)
}

func (e *observeCodex) CountTokens(ctx context.Context, auth *cliproxyauth.Auth, req cliproxyexecutor.Request, opts cliproxyexecutor.Options) (cliproxyexecutor.Response, error) {
	return e.inner.CountTokens(ctx, auth, req, opts)
}

func (e *observeCodex) HttpRequest(ctx context.Context, auth *cliproxyauth.Auth, req *http.Request) (*http.Response, error) {
	return e.inner.HttpRequest(ctx, auth, req)
}

type nativeCriticalFixture struct {
	manager   *cliproxyauth.Manager
	boundary  *recordingBoundary
	transport *countingTransport
	observer  *observeCodex
	exec      *runtimeexecutor.CodexExecutor
	authA     *cliproxyauth.Auth
	authB     *cliproxyauth.Auth
	upstream  *httptest.Server
	token     *httptest.Server
	backup    *httptest.Server
	count     *countingUpstream
}

func (fx *nativeCriticalFixture) responsesURL() string {
	return strings.TrimRight(fx.upstream.URL, "/") + "/responses"
}

func (fx *nativeCriticalFixture) compactURL() string {
	return strings.TrimRight(fx.upstream.URL, "/") + "/responses/compact"
}

func (fx *nativeCriticalFixture) backupURL() string {
	return strings.TrimRight(fx.backup.URL, "/") + "/responses"
}

func startNativeCriticalFixture(t *testing.T, buffering bool, onHit func(n int32, w http.ResponseWriter, r *http.Request) bool) *nativeCriticalFixture {
	t.Helper()
	count := &countingUpstream{onHit: onHit}
	count.success = func(w http.ResponseWriter) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(nativeCriticalSSE))
	}
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := count.note(r)
		if count.onHit != nil && count.onHit(n, w, r) {
			return
		}
		count.success(w)
	}))
	t.Cleanup(upstream.Close)
	token := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		count.tokens.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"access-new","refresh_token":"refresh-1","id_token":"not-a-jwt","expires_in":3600}`))
	}))
	t.Cleanup(token.Close)
	backup := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		count.backup.Add(1)
		t.Errorf("registered competing B was contacted %s %s", r.Method, r.URL.Path)
		w.WriteHeader(http.StatusOK)
	}))
	t.Cleanup(backup.Close)

	pin := publicPin(t, strings.TrimRight(upstream.URL, "/")+"/responses", "lb-a-responses")
	compact := publicPin(t, strings.TrimRight(upstream.URL, "/")+"/responses/compact", "lb-a-compact")
	boundary := &recordingBoundary{httpPins: []cliproxyauth.EndpointPin{pin, compact}}
	cfg := &config.Config{}
	cfg.Codex.StreamBootstrapBuffering = buffering
	manager := cliproxyauth.NewManager(nil, &cliproxyauth.FillFirstSelector{}, nil)
	manager.SetRetryConfig(0, 0, 0)
	manager.SetGenerationBoundary(boundary)
	exec := runtimeexecutor.NewCodexExecutor(cfg)
	observer := &observeCodex{inner: exec}
	manager.RegisterExecutor(observer)

	const authA, authB = "oauth-a", "oauth-b"
	registry.GetGlobalRegistry().RegisterClient(authA, "codex", []*registry.ModelInfo{{ID: nativeCriticalModel}})
	registry.GetGlobalRegistry().RegisterClient(authB, "codex", []*registry.ModelInfo{{ID: nativeCriticalModel}})
	t.Cleanup(func() {
		registry.GetGlobalRegistry().UnregisterClient(authA)
		registry.GetGlobalRegistry().UnregisterClient(authB)
	})
	registeredA, err := manager.Register(context.Background(), nativeCriticalOAuth(authA, upstream.URL, token.URL, "access-old", "refresh-1", "10"))
	if err != nil {
		t.Fatal(err)
	}
	registeredB, err := manager.Register(context.Background(), nativeCriticalOAuth(authB, backup.URL, token.URL, "access-b", "refresh-b", "1"))
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := manager.GetByID(authB); !ok {
		t.Fatal("competing B was not registered")
	}
	return &nativeCriticalFixture{
		manager:   manager,
		boundary:  boundary,
		transport: &countingTransport{next: http.DefaultTransport},
		observer:  observer,
		exec:      exec,
		authA:     registeredA,
		authB:     registeredB,
		upstream:  upstream,
		token:     token,
		backup:    backup,
		count:     count,
	}
}

func nativeCriticalOAuth(id, baseURL, tokenURL, access, refresh, priority string) *cliproxyauth.Auth {
	return &cliproxyauth.Auth{
		ID:       id,
		Provider: "codex",
		Status:   cliproxyauth.StatusActive,
		Attributes: map[string]string{
			"base_url": baseURL,
			"priority": priority,
		},
		Metadata: map[string]any{
			"type":                   "codex",
			"ocg_oauth":              true,
			"ocg_credential_id":      id,
			"ocg_credential_version": "4",
			"access_token":           access,
			"refresh_token":          refresh,
			"token_url":              tokenURL,
			"base_url":               baseURL,
			"plan_type":              "pro",
			"disable_cooling":        true,
		},
	}
}

func nativeCriticalRequest(stream bool) (cliproxyexecutor.Request, cliproxyexecutor.Options) {
	payload := []byte(`{"model":"` + nativeCriticalModel + `","input":"hello","stream":` + boolJSON(stream) + `}`)
	req := cliproxyexecutor.Request{Model: nativeCriticalModel, Payload: payload}
	opts := cliproxyexecutor.Options{
		Stream:          stream,
		SourceFormat:    sdktranslator.FromString("openai-response"),
		ResponseFormat:  sdktranslator.FormatOpenAIResponse,
		OriginalRequest: payload,
	}
	return req, opts
}

func executorOpts() cliproxyexecutor.Options {
	return cliproxyexecutor.Options{
		SourceFormat:   sdktranslator.FromString("openai-response"),
		ResponseFormat: sdktranslator.FormatOpenAIResponse,
	}
}

func publicPin(t *testing.T, raw, id string) cliproxyauth.EndpointPin {
	t.Helper()
	_, origin, fingerprint, err := cliproxyauth.CanonicalDispatchURL(raw)
	if err != nil {
		t.Fatal(err)
	}
	return cliproxyauth.EndpointPin{
		Protocol:            "responses",
		EndpointID:          id,
		Origin:              origin,
		EndpointFingerprint: fingerprint,
		HTTPMethod:          "POST",
	}
}

func drainNativeStream(t *testing.T, result *cliproxyexecutor.StreamResult) string {
	t.Helper()
	if result == nil || result.Chunks == nil {
		t.Fatal("missing stream")
	}
	var body strings.Builder
	for chunk := range result.Chunks {
		if chunk.Err != nil {
			t.Fatalf("stream chunk: %v", chunk.Err)
		}
		body.Write(chunk.Payload)
	}
	got := body.String()
	if !strings.Contains(got, "response.completed") {
		t.Fatalf("stream body=%s", got)
	}
	return got
}

func drainStreamIfPresent(result *cliproxyexecutor.StreamResult) {
	if result != nil && result.Chunks != nil {
		for range result.Chunks {
		}
	}
}

func requireIntendedStop(t *testing.T, result *cliproxyexecutor.StreamResult, execErr error) {
	t.Helper()
	drainStreamIfPresent(result)
	if execErr == nil || (!errors.Is(execErr, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(execErr)) {
		t.Fatalf("intended refusal err=%v", execErr)
	}
}

func requireContextError(t *testing.T, result *cliproxyexecutor.StreamResult, execErr, want error) {
	t.Helper()
	drainStreamIfPresent(result)
	if execErr == nil || !errors.Is(execErr, want) {
		t.Fatalf("context err=%v want %v", execErr, want)
	}
}

func requireNamedRefusal(t *testing.T, fx *nativeCriticalFixture, kind string, after int, previousAttempt string, execErr error) {
	t.Helper()
	dec, ok := fx.boundary.namedStopAfter(kind, after)
	if !ok {
		t.Fatalf("missing %s stop/current_authority_denied after=%d decisions=%v", kind, after, fx.boundary.decisionSummaries())
	}
	if dec.attemptID == "" || dec.attemptID == previousAttempt {
		t.Fatalf("%s refusal attempt=%s previous=%s", kind, dec.attemptID, previousAttempt)
	}
	if cliproxyauth.MatchedEndpointPin(dec.ctx) != nil {
		t.Fatalf("%s refusal matched pin=%+v", kind, cliproxyauth.MatchedEndpointPin(dec.ctx))
	}
	sent, _, _, _, _, transport, _, _, _ := cliproxyauth.GenerationFacts(dec.ctx)
	if sent || (transport != "" && transport != "unsent") {
		t.Fatalf("%s refusal sent=%v transport=%q", kind, sent, transport)
	}
	if execErr == nil || (!errors.Is(execErr, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(execErr)) {
		t.Fatalf("%s intended refusal err=%v action=%s reason=%s", kind, execErr, dec.action, dec.reason)
	}
}

func requireFirstSelectedA(t *testing.T, fx *nativeCriticalFixture) {
	t.Helper()
	ids := fx.boundary.identities()
	if len(ids) == 0 || ids[0].CredentialID != "oauth-a" || ids[0].CredentialVersion != "4" {
		t.Fatalf("first selected identity=%+v kinds=%v", ids, fx.boundary.kinds())
	}
	recs := fx.count.captured()
	if len(recs) == 0 || recs[0].auth != "Bearer access-old" {
		t.Fatalf("first arrival=%+v", recs)
	}
}

func requireKinds(t *testing.T, got []string, want ...string) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("kinds=%v want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("kinds=%v want %v", got, want)
		}
	}
}

func requireDistinctAttempts(t *testing.T, attempts []string) {
	t.Helper()
	if len(attempts) < 2 || attempts[0] == "" || attempts[1] == "" || attempts[0] == attempts[1] {
		t.Fatalf("attempts=%v", attempts)
	}
}

func requireSameCredential(t *testing.T, ids []cliproxyauth.GenerationIdentity) {
	t.Helper()
	if len(ids) < 2 {
		t.Fatalf("identities=%v", ids)
	}
	if ids[0].CredentialID != "oauth-a" || ids[1].CredentialID != "oauth-a" {
		t.Fatalf("credential id moved: %+v", ids)
	}
	if ids[0].CredentialVersion != "4" || ids[1].CredentialVersion != "4" {
		t.Fatalf("credential version moved: %+v", ids)
	}
	if ids[0].MaterialRevision == "" || ids[0].MaterialRevision == ids[1].MaterialRevision {
		t.Fatalf("material was not refreshed: %+v", ids)
	}
}

func requireIdleB(t *testing.T, fx *nativeCriticalFixture) {
	t.Helper()
	if _, ok := fx.manager.GetByID("oauth-b"); !ok {
		t.Fatal("competing B missing")
	}
	if fx.count.backup.Load() != 0 {
		t.Fatalf("competing B hits=%d", fx.count.backup.Load())
	}
}

func requireExactArrival(t *testing.T, rec recordedHTTP, raw string) {
	t.Helper()
	if rec.canonErr != nil {
		t.Fatalf("observed absolute canonical err=%v scheme=%s host=%s path=%s query=%s absolute=%s", rec.canonErr, rec.scheme, rec.host, rec.path, rec.query, rec.absolute)
	}
	canonical, _, _, err := cliproxyauth.CanonicalDispatchURL(raw)
	if err != nil {
		t.Fatal(err)
	}
	if rec.method != http.MethodPost || rec.canonical == "" || rec.canonical != canonical {
		t.Fatalf("arrival method=%s scheme=%s host=%s path=%s query=%s absolute=%s canonical=%s want POST %s", rec.method, rec.scheme, rec.host, rec.path, rec.query, rec.absolute, rec.canonical, canonical)
	}
}

func requireMatchedPin(t *testing.T, pin *cliproxyauth.EndpointPin, raw string) {
	t.Helper()
	want := publicPin(t, raw, "unused")
	if pin == nil || pin.Protocol != "responses" || pin.HTTPMethod != "POST" || pin.EndpointFingerprint != want.EndpointFingerprint || pin.Origin != want.Origin {
		t.Fatalf("matched pin %+v want %s %s", pin, want.Origin, want.EndpointFingerprint)
	}
}

func requireUnsentNull(t *testing.T, ctx context.Context, err error) {
	t.Helper()
	if err == nil || (!errors.Is(err, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(err)) {
		t.Fatalf("denied send = %v", err)
	}
	if cliproxyauth.MatchedEndpointPin(ctx) != nil {
		t.Fatalf("matched pin=%+v", cliproxyauth.MatchedEndpointPin(ctx))
	}
	sent, _, _, _, _, transport, _, _, _ := cliproxyauth.GenerationFacts(ctx)
	if sent {
		t.Fatal("denied send marked sent")
	}
	if transport != "" && transport != "unsent" {
		t.Fatalf("transport=%q", transport)
	}
}

func liveAuth(t *testing.T, manager *cliproxyauth.Manager, id string) *cliproxyauth.Auth {
	t.Helper()
	auth, ok := manager.GetByID(id)
	if !ok || auth == nil {
		t.Fatalf("missing auth %s", id)
	}
	return auth
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var buf [12]byte
	i := len(buf)
	for n > 0 {
		i--
		buf[i] = byte('0' + n%10)
		n /= 10
	}
	return string(buf[i:])
}

func boolJSON(v bool) string {
	if v {
		return "true"
	}
	return "false"
}

func writeUnauthorized(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusUnauthorized)
	_, _ = w.Write([]byte(`{"error":{"message":"unauthorized"}}`))
}

func writeAuthErrorSSE(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.WriteHeader(http.StatusOK)
	_, _ = io.WriteString(w, nativeCriticalAuthErrorSSE)
}

func firstUnauthorizedHit(n int32, w http.ResponseWriter, _ *http.Request) bool {
	if n == 1 {
		writeUnauthorized(w)
		return true
	}
	return false
}

func firstAuthErrorHit(n int32, w http.ResponseWriter, _ *http.Request) bool {
	if n == 1 {
		writeAuthErrorSSE(w)
		return true
	}
	return false
}

func nextHitEquals(target int32, write func(http.ResponseWriter)) func(int32, http.ResponseWriter, *http.Request) bool {
	return func(n int32, w http.ResponseWriter, _ *http.Request) bool {
		if n == target {
			write(w)
			return true
		}
		return false
	}
}
