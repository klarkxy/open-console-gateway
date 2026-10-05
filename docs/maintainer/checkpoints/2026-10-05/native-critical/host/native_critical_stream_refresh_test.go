package main

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
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

const nativeCriticalWait = 5 * time.Second

type hostHoldGate struct {
	seen    chan struct{}
	release chan struct{}
	once    sync.Once
}

func newHostHoldGate() *hostHoldGate {
	return &hostHoldGate{seen: make(chan struct{}), release: make(chan struct{})}
}

func (g *hostHoldGate) park() {
	g.once.Do(func() { close(g.seen) })
	select {
	case <-g.release:
	case <-time.After(nativeCriticalWait):
	}
}

type hostSequencedContext struct {
	parent      context.Context
	mu          sync.Mutex
	done        chan struct{}
	err         error
	deadline    time.Time
	hasDeadline bool
}

func newHostSequencedContext(parent context.Context) *hostSequencedContext {
	if parent == nil {
		parent = context.Background()
	}
	return &hostSequencedContext{parent: parent, done: make(chan struct{})}
}

func (c *hostSequencedContext) expire(err error) {
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

func (c *hostSequencedContext) Deadline() (time.Time, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.hasDeadline {
		return c.deadline, true
	}
	return c.parent.Deadline()
}

func (c *hostSequencedContext) Done() <-chan struct{} { return c.done }

func (c *hostSequencedContext) Err() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.err != nil {
		return c.err
	}
	return c.parent.Err()
}

func (c *hostSequencedContext) Value(key any) any { return c.parent.Value(key) }

type hostArrival struct {
	auth, method, scheme, host, path, query, absolute, canonical string
	canonErr                                                     error
}

type policyAdmitDecision struct {
	kind, attemptID, action, reason         string
	authID, credentialID, credentialVersion string
	registrationEpoch, materialRevision     string
}

type admitDecisionLog struct {
	mu    sync.Mutex
	items []policyAdmitDecision
}

func (l *admitDecisionLog) append(item policyAdmitDecision) {
	l.mu.Lock()
	l.items = append(l.items, item)
	l.mu.Unlock()
}

func (l *admitDecisionLog) len() int {
	l.mu.Lock()
	defer l.mu.Unlock()
	return len(l.items)
}

func (l *admitDecisionLog) firstKind(kind string) (policyAdmitDecision, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	for _, item := range l.items {
		if item.kind == kind {
			return item, true
		}
	}
	return policyAdmitDecision{}, false
}

func (l *admitDecisionLog) namedStopAfter(kind string, after int) (policyAdmitDecision, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if after < 0 {
		after = 0
	}
	for i := after; i < len(l.items); i++ {
		item := l.items[i]
		if item.kind == kind && item.action == "stop" && item.reason == "current_authority_denied" {
			return item, true
		}
	}
	return policyAdmitDecision{}, false
}

func (l *admitDecisionLog) summaries() []string {
	l.mu.Lock()
	defer l.mu.Unlock()
	out := make([]string, 0, len(l.items))
	for _, item := range l.items {
		out = append(out, item.kind+"/"+item.action+"/"+item.reason+"/"+item.attemptID+"/"+item.credentialID)
	}
	return out
}

func observedHostAbsolute(r *http.Request) (absolute, scheme, host, path, query string, err error) {
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

func observeHostArrival(r *http.Request) hostArrival {
	rec := hostArrival{method: r.Method, auth: r.Header.Get("Authorization")}
	absolute, scheme, host, path, query, err := observedHostAbsolute(r)
	rec.scheme, rec.host, rec.path, rec.query, rec.absolute = scheme, host, path, query, absolute
	if err != nil {
		rec.canonErr = err
		return rec
	}
	canonical, _, _, err := coreauth.CanonicalDispatchURL(absolute)
	if err != nil {
		rec.canonErr = err
		return rec
	}
	rec.canonical = canonical
	return rec
}

func admitControlPolicy(t *testing.T, admitAction func(policyRequest) string, resultAction func(policyRequest) string) (*httptest.Server, *policyLog, *admitDecisionLog) {
	t.Helper()
	t.Cleanup(resetNativeGrants)
	log := &policyLog{admits: map[string]policyRequest{}}
	decisions := &admitDecisionLog{}
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
			action, reason := "allow", "eligible"
			if admitAction != nil {
				if next := admitAction(req); next != "" {
					action = next
				}
			}
			if action == "stop" || action == "skip" {
				reason = "current_authority_denied"
			}
			log.admits[req.AttemptID] = req
			log.seq = append(log.seq, "admit")
			decisions.append(policyAdmitDecision{
				kind: req.GenerationKind, attemptID: req.AttemptID, action: action, reason: reason,
				authID: req.AuthID, credentialID: req.CredentialID, credentialVersion: req.CredentialVersion,
				registrationEpoch: req.RegistrationEpoch, materialRevision: req.MaterialRevision,
			})
			if action == "stop" || action == "skip" {
				writeDecisionPins(w, action, reason, nil)
				return
			}
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
	return server, log, decisions
}

func TestNativeStreamRefreshResendKeepsPinnedCredential(t *testing.T) {
	var aHits, bHits atomic.Int32
	var mu sync.Mutex
	var records []hostArrival
	a := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := aHits.Add(1)
		mu.Lock()
		records = append(records, observeHostArrival(r))
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
		t.Errorf("stream-refresh reached registered competing B %s %s", r.Method, r.URL.Path)
		w.WriteHeader(http.StatusOK)
	}))
	defer b.Close()
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
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
		bindings: []nativeBinding{
			{
				file: "codex-a.json", id: "oauth-a", version: "4", priority: "10", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "access-old", refresh: "refresh-1", baseURL: a.URL, tokenURL: tokenSrv.URL,
			},
			{
				file: "codex-b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "access-b", refresh: "refresh-b", baseURL: b.URL,
			},
		},
	})
	_ = waitReady(t, directClient(), host, "oauth-a")
	_ = waitReady(t, directClient(), host, "oauth-b")
	if settleNativeAuth(t, host, "oauth-b").ID == "" {
		t.Fatal("competing B was not registered")
	}
	payload := []byte(`{"model":"` + nativeModel + `","input":"hello","stream":true}`)
	opts := cliproxyexecutor.Options{
		Stream:          true,
		SourceFormat:    sdktranslator.FromString("openai-response"),
		ResponseFormat:  sdktranslator.FormatOpenAIResponse,
		OriginalRequest: payload,
	}
	stream, err := host.service.CoreAuthManager().ExecuteStream(withRequestID(context.Background(), newID()), []string{"codex"}, cliproxyexecutor.Request{Model: nativeModel, Payload: payload}, opts)
	if err != nil {
		t.Fatal(err)
	}
	if stream == nil || stream.Chunks == nil {
		t.Fatal("missing stream")
	}
	var body strings.Builder
	for chunk := range stream.Chunks {
		if chunk.Err != nil {
			t.Fatalf("stream chunk: %v", chunk.Err)
		}
		body.Write(chunk.Payload)
	}
	if !strings.Contains(body.String(), "response.completed") {
		t.Fatalf("stream body=%s", body.String())
	}
	if aHits.Load() != 2 || bHits.Load() != 0 {
		t.Fatalf("stream-refresh hits A=%d B=%d", aHits.Load(), bHits.Load())
	}
	target := strings.TrimRight(a.URL, "/") + "/responses"
	wantCanonical, _, _, err := coreauth.CanonicalDispatchURL(target)
	if err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	if len(records) != 2 || records[0].auth != "Bearer access-old" || records[1].auth != "Bearer access-new" {
		mu.Unlock()
		t.Fatalf("authorization=%+v", records)
	}
	for _, rec := range records {
		if rec.canonErr != nil {
			mu.Unlock()
			t.Fatalf("observed absolute canonical err=%v scheme=%s host=%s path=%s query=%s absolute=%s", rec.canonErr, rec.scheme, rec.host, rec.path, rec.query, rec.absolute)
		}
		if rec.method != http.MethodPost || rec.path != "/responses" || rec.query != "" || rec.canonical == "" || rec.canonical != wantCanonical {
			mu.Unlock()
			t.Fatalf("arrival method=%s scheme=%s host=%s path=%s query=%s absolute=%s canonical=%s want POST %s", rec.method, rec.scheme, rec.host, rec.path, rec.query, rec.absolute, rec.canonical, wantCanonical)
		}
	}
	mu.Unlock()
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) != 2 {
		t.Fatalf("result count=%d items=%+v", len(results.items), results.items)
	}
	first, second := results.items[0], results.items[1]
	streamAdmit := results.admits[first.AttemptID]
	refreshAdmit := results.admits[second.AttemptID]
	if streamAdmit.GenerationKind != "stream" || refreshAdmit.GenerationKind != "stream-refresh" {
		t.Fatalf("admit kinds stream=%s refresh=%s", streamAdmit.GenerationKind, refreshAdmit.GenerationKind)
	}
	if first.AttemptID == "" || second.AttemptID == "" || first.AttemptID == second.AttemptID {
		t.Fatalf("attempts=%s/%s", first.AttemptID, second.AttemptID)
	}
	if first.AttemptID != streamAdmit.AttemptID || second.AttemptID != refreshAdmit.AttemptID {
		t.Fatalf("result/admit mismatch %s/%s vs %s/%s", first.AttemptID, second.AttemptID, streamAdmit.AttemptID, refreshAdmit.AttemptID)
	}
	if streamAdmit.CredentialID != "oauth-a" || refreshAdmit.CredentialID != "oauth-a" || streamAdmit.CredentialVersion != "4" || refreshAdmit.CredentialVersion != "4" {
		t.Fatalf("credential moved stream=%+v refresh=%+v", streamAdmit, refreshAdmit)
	}
	if streamAdmit.MaterialRevision == "" || streamAdmit.MaterialRevision == refreshAdmit.MaterialRevision {
		t.Fatalf("material was not refreshed stream=%q refresh=%q", streamAdmit.MaterialRevision, refreshAdmit.MaterialRevision)
	}
	if first.Outcome != "explicit_rejection" || second.Outcome != "success" {
		t.Fatalf("outcomes=%s/%s", first.Outcome, second.Outcome)
	}
	if first.Sent == nil || !*first.Sent || second.Sent == nil || !*second.Sent {
		t.Fatalf("sent facts first=%+v second=%+v", first, second)
	}
	if second.StreamStarted == nil || !*second.StreamStarted || second.BodyComplete == nil || !*second.BodyComplete {
		t.Fatalf("stream facts=%+v", second)
	}
	requireMatchedPin(t, first, target, "responses")
	requireMatchedPin(t, second, target, "responses")
}

type hostHeldExpect struct {
	requireRefreshStop bool
	contextErr         error
}

func TestNativeStreamResumeDeniedAfterAuthorityLoss(t *testing.T) {
	t.Run("heldCurrentAuthority", func(t *testing.T) {
		runHostHeldDenial(t, func(host *Host, first policyAdmitDecision, refuse *atomic.Bool, _ *hostSequencedContext) {
			if first.credentialID != "oauth-a" || first.credentialVersion != "4" || first.registrationEpoch == "" {
				t.Fatalf("first policy admit=%+v", first)
			}
			replaced := settleNativeAuth(t, host, "oauth-a").Clone()
			replaced.Metadata["access_token"] = "access-replaced"
			replaced.Metadata["refresh_token"] = "refresh-replaced"
			updated, err := host.service.CoreAuthManager().Update(context.Background(), replaced)
			if err != nil {
				t.Fatal(err)
			}
			after := settleNativeAuth(t, host, "oauth-a")
			if strconv.FormatUint(updated.RegistrationEpoch, 10) == first.registrationEpoch || strconv.FormatUint(after.RegistrationEpoch, 10) != strconv.FormatUint(updated.RegistrationEpoch, 10) {
				t.Fatalf("live epoch=%d first admit epoch=%s", after.RegistrationEpoch, first.registrationEpoch)
			}
			if metadataText(after, "ocg_credential_version") != "4" {
				t.Fatalf("version=%q", metadataText(after, "ocg_credential_version"))
			}
			refuse.Store(true)
		}, hostHeldExpect{requireRefreshStop: true})
	})
	t.Run("revoked", func(t *testing.T) {
		runHostHeldDenial(t, func(host *Host, _ policyAdmitDecision, _ *atomic.Bool, _ *hostSequencedContext) {
			host.service.CoreAuthManager().Remove(context.Background(), settleNativeAuth(t, host, "oauth-a").ID)
		}, hostHeldExpect{})
	})
	t.Run("cancel", func(t *testing.T) {
		runHostHeldDenial(t, func(_ *Host, _ policyAdmitDecision, _ *atomic.Bool, ctx *hostSequencedContext) {
			ctx.expire(context.Canceled)
		}, hostHeldExpect{contextErr: context.Canceled})
	})
	t.Run("deadline", func(t *testing.T) {
		runHostHeldDenial(t, func(_ *Host, _ policyAdmitDecision, _ *atomic.Bool, ctx *hostSequencedContext) {
			ctx.expire(context.DeadlineExceeded)
		}, hostHeldExpect{contextErr: context.DeadlineExceeded})
	})
}

func runHostHeldDenial(t *testing.T, whileHeld func(*Host, policyAdmitDecision, *atomic.Bool, *hostSequencedContext), expect hostHeldExpect) {
	t.Helper()
	hold := newHostHoldGate()
	var aHits, bHits atomic.Int32
	var refuse atomic.Bool
	a := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := aHits.Add(1)
		if n == 1 {
			hold.park()
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":{"message":"unauthorized"}}`))
			return
		}
		t.Errorf("held refusal reached a second generation HTTP %s", r.URL.Path)
		w.WriteHeader(http.StatusOK)
	}))
	defer a.Close()
	b := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		bHits.Add(1)
		t.Errorf("held refusal reached registered competing B %s", r.URL.Path)
		w.WriteHeader(http.StatusOK)
	}))
	defer b.Close()
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"access-new","refresh_token":"refresh-1","id_token":"not-a-jwt","expires_in":3600}`))
	}))
	defer tokenSrv.Close()
	policy, results, decisions := admitControlPolicy(t, func(req policyRequest) string {
		if refuse.Load() && req.GenerationKind == "stream-refresh" {
			return "stop"
		}
		return "allow"
	}, func(req policyRequest) string {
		if req.Status != nil && *req.Status == http.StatusUnauthorized {
			return "skip"
		}
		return "stop"
	})
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{
				file: "codex-a.json", id: "oauth-a", version: "4", priority: "10", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "access-old", refresh: "refresh-1", baseURL: a.URL, tokenURL: tokenSrv.URL,
			},
			{
				file: "codex-b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "access-b", refresh: "refresh-b", baseURL: b.URL,
			},
		},
	})
	_ = waitReady(t, directClient(), host, "oauth-a")
	_ = waitReady(t, directClient(), host, "oauth-b")
	ctx := newHostSequencedContext(withRequestID(context.Background(), newID()))
	payload := []byte(`{"model":"` + nativeModel + `","input":"hello","stream":true}`)
	opts := cliproxyexecutor.Options{Stream: true, SourceFormat: sdktranslator.FromString("openai-response"), OriginalRequest: payload}
	done := make(chan struct {
		stream *cliproxyexecutor.StreamResult
		err    error
	}, 1)
	go func() {
		stream, err := host.service.CoreAuthManager().ExecuteStream(ctx, []string{"codex"}, cliproxyexecutor.Request{Model: nativeModel, Payload: payload}, opts)
		done <- struct {
			stream *cliproxyexecutor.StreamResult
			err    error
		}{stream, err}
	}()
	select {
	case <-hold.seen:
	case <-time.After(nativeCriticalWait):
		t.Fatal("timeout waiting for first generation HTTP")
	}
	if aHits.Load() != 1 {
		t.Fatalf("first hit=%d", aHits.Load())
	}
	first, ok := decisions.firstKind("stream")
	if !ok || first.credentialID != "oauth-a" || first.credentialVersion != "4" || first.attemptID == "" {
		t.Fatalf("first stream admit=%+v decisions=%v", first, decisions.summaries())
	}
	beforeDec := decisions.len()
	whileHeld(host, first, &refuse, ctx)
	close(hold.release)
	var got struct {
		stream *cliproxyexecutor.StreamResult
		err    error
	}
	select {
	case got = <-done:
	case <-time.After(nativeCriticalWait):
		t.Fatal("timeout waiting for ExecuteStream")
	}
	if got.stream != nil && got.stream.Chunks != nil {
		for range got.stream.Chunks {
		}
	}
	if aHits.Load() != 1 || bHits.Load() != 0 {
		t.Fatalf("held resume A=%d B=%d err=%v decisions=%v", aHits.Load(), bHits.Load(), got.err, decisions.summaries())
	}
	switch {
	case expect.contextErr != nil:
		if got.err == nil || !errors.Is(got.err, expect.contextErr) {
			t.Fatalf("context err=%v want %v", got.err, expect.contextErr)
		}
	case expect.requireRefreshStop:
		dec, ok := decisions.namedStopAfter("stream-refresh", beforeDec)
		if !ok || dec.attemptID == "" || dec.attemptID == first.attemptID {
			t.Fatalf("missing stream-refresh stop/current_authority_denied after=%d first=%s decisions=%v", beforeDec, first.attemptID, decisions.summaries())
		}
		if got.err == nil || (!errors.Is(got.err, coreauth.ErrAttemptStop) && !coreauth.RequestStopped(got.err)) {
			t.Fatalf("intended refusal err=%v decision=%+v", got.err, dec)
		}
		results.mu.Lock()
		for _, item := range results.items {
			if item.AttemptID == dec.attemptID && item.Outcome == "success" && item.Sent != nil && *item.Sent {
				results.mu.Unlock()
				t.Fatalf("denied stream-refresh published a sent success result=%+v", item)
			}
			if item.AttemptID == dec.attemptID && item.Sent != nil && *item.Sent {
				results.mu.Unlock()
				t.Fatalf("denied stream-refresh result was sent=%+v", item)
			}
			if item.AttemptID == dec.attemptID && item.EndpointPin != nil {
				results.mu.Unlock()
				t.Fatalf("denied stream-refresh matched pin=%+v", item.EndpointPin)
			}
		}
		results.mu.Unlock()
	default:
		if got.err == nil {
			t.Fatal("revoked resume returned success with no error")
		}
	}
	_ = results
}
