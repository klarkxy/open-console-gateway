package auth_test

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"testing"
	"time"

	cliproxyauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/executor"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v8/sdk/translator"
)

type streamOutcome struct {
	stream *cliproxyexecutor.StreamResult
	err    error
}

func TestOCGNativeStreamRefreshHTTP(t *testing.T) {
	fx := startNativeCriticalFixture(t, false, firstUnauthorizedHit)
	req, opts := nativeCriticalRequest(true)
	stream, err := fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
	if err != nil {
		t.Fatal(err)
	}
	body := drainNativeStream(t, stream)
	if !strings.Contains(body, "response.completed") {
		t.Fatalf("stream body=%s", body)
	}
	if fx.count.hits.Load() != 2 || fx.count.tokens.Load() == 0 {
		t.Fatalf("hits=%d tokens=%d", fx.count.hits.Load(), fx.count.tokens.Load())
	}
	requireFirstSelectedA(t, fx)
	recs := fx.count.captured()
	if len(recs) != 2 || recs[0].auth != "Bearer access-old" || recs[1].auth != "Bearer access-new" {
		t.Fatalf("authorization=%+v", recs)
	}
	for _, rec := range recs {
		if rec.path != "/responses" || rec.query != "" {
			t.Fatalf("paths=%+v", recs)
		}
		requireExactArrival(t, rec, fx.responsesURL())
	}
	requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindStream, cliproxyauth.GenerationKindStreamRefresh)
	requireDistinctAttempts(t, fx.boundary.attempts())
	requireSameCredential(t, fx.boundary.identities())
	refresh, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindStreamRefresh)
	if !ok || !refresh.result.Success || !refresh.sent || !refresh.streamed || !refresh.bodyComplete {
		t.Fatalf("stream-refresh result=%+v", refresh)
	}
	if refresh.attemptID == "" || refresh.attemptID != fx.boundary.attempts()[1] {
		t.Fatalf("stream-refresh attempt=%s attempts=%v", refresh.attemptID, fx.boundary.attempts())
	}
	requireMatchedPin(t, refresh.matched, fx.responsesURL())
	requireIdleB(t, fx)
	live := liveAuth(t, fx.manager, "oauth-a")
	if live.Metadata["access_token"] != "access-new" {
		t.Fatalf("live token=%v", live.Metadata["access_token"])
	}
}

type heldExpect struct {
	requireRefreshStop bool
	contextErr         error
}

func TestOCGNativeStreamResumeDeniedAfterAuthorityLoss(t *testing.T) {
	t.Run("heldCurrentAuthority", func(t *testing.T) {
		runHeldDenial(t, func(fx *nativeCriticalFixture, _ *sequencedContext) {
			first := fx.boundary.identities()
			if len(first) == 0 || first[0].CredentialID != "oauth-a" || first[0].RegistrationEpoch == 0 || first[0].CredentialVersion != "4" {
				t.Fatalf("first admit identity=%+v", first)
			}
			replaced := liveAuth(t, fx.manager, "oauth-a")
			replaced.Metadata["access_token"] = "access-replaced"
			replaced.Metadata["refresh_token"] = "refresh-replaced"
			updated, err := fx.manager.Update(context.Background(), replaced)
			if err != nil {
				t.Fatal(err)
			}
			live := liveAuth(t, fx.manager, "oauth-a")
			if updated.RegistrationEpoch == first[0].RegistrationEpoch || live.RegistrationEpoch != updated.RegistrationEpoch {
				t.Fatalf("live epoch=%d first admit=%d", live.RegistrationEpoch, first[0].RegistrationEpoch)
			}
			if live.Metadata["ocg_credential_version"] != "4" {
				t.Fatalf("version=%v", live.Metadata["ocg_credential_version"])
			}
			fx.boundary.refuseKind(cliproxyauth.GenerationKindStreamRefresh)
		}, heldExpect{requireRefreshStop: true})
	})
	t.Run("revoked", func(t *testing.T) {
		runHeldDenial(t, func(fx *nativeCriticalFixture, _ *sequencedContext) {
			fx.manager.Remove(context.Background(), "oauth-a")
		}, heldExpect{})
	})
	t.Run("cancel", func(t *testing.T) {
		runHeldDenial(t, func(_ *nativeCriticalFixture, ctx *sequencedContext) {
			ctx.expire(context.Canceled)
		}, heldExpect{contextErr: context.Canceled})
	})
	t.Run("deadline", func(t *testing.T) {
		runHeldDenial(t, func(_ *nativeCriticalFixture, ctx *sequencedContext) {
			ctx.expire(context.DeadlineExceeded)
		}, heldExpect{contextErr: context.DeadlineExceeded})
	})
}

func runHeldDenial(t *testing.T, whileHeld func(*nativeCriticalFixture, *sequencedContext), expect heldExpect) {
	t.Helper()
	hold := newHoldGate()
	fx := startNativeCriticalFixture(t, false, func(n int32, w http.ResponseWriter, r *http.Request) bool {
		if n == 1 {
			hold.park()
			writeUnauthorized(w)
			return true
		}
		t.Errorf("held refusal reached a second generation HTTP %s", r.URL.Path)
		return false
	})
	ctx := newSequencedContext(context.Background())
	req, opts := nativeCriticalRequest(true)
	done := make(chan streamOutcome, 1)
	go func() {
		stream, err := fx.manager.ExecuteStream(ctx, []string{"codex"}, req, opts)
		done <- streamOutcome{stream: stream, err: err}
	}()
	waitClosed(t, hold.seen, "first stream hit")
	if fx.count.hits.Load() != 1 {
		t.Fatalf("first hit=%d", fx.count.hits.Load())
	}
	requireFirstSelectedA(t, fx)
	firstAttempt := fx.boundary.attempts()[0]
	beforeDec := fx.boundary.decisionLen()
	whileHeld(fx, ctx)
	close(hold.release)
	got := waitStreamOutcome(t, done)
	if fx.count.hits.Load() != 1 {
		t.Fatalf("held resume hits=%d kinds=%v err=%v decisions=%v", fx.count.hits.Load(), fx.boundary.kinds(), got.err, fx.boundary.decisionSummaries())
	}
	switch {
	case expect.contextErr != nil:
		requireContextError(t, got.stream, got.err, expect.contextErr)
	case expect.requireRefreshStop:
		requireNamedRefusal(t, fx, cliproxyauth.GenerationKindStreamRefresh, beforeDec, firstAttempt, got.err)
		requireIntendedStop(t, got.stream, got.err)
	default:
		drainStreamIfPresent(got.stream)
		if got.err == nil {
			t.Fatal("revoked resume returned success with no error")
		}
	}
	if item, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindStreamRefresh); ok && item.sent {
		t.Fatalf("resumed stream-refresh sent %+v", item)
	}
	requireIdleB(t, fx)
}

func waitStreamOutcome(t *testing.T, done <-chan streamOutcome) streamOutcome {
	t.Helper()
	select {
	case got := <-done:
		return got
	case <-time.After(nativeCriticalWait):
		t.Fatal("timeout waiting for ExecuteStream")
	}
	return streamOutcome{}
}

func TestOCGNativeStreamBootstrapHTTP(t *testing.T) {
	fx := startNativeCriticalFixture(t, true, firstAuthErrorHit)
	req, opts := nativeCriticalRequest(true)
	stream, err := fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
	if err != nil {
		t.Fatal(err)
	}
	drainNativeStream(t, stream)
	if fx.count.hits.Load() != 2 {
		t.Fatalf("hits=%d kinds=%v", fx.count.hits.Load(), fx.boundary.kinds())
	}
	requireFirstSelectedA(t, fx)
	recs := fx.count.captured()
	for _, rec := range recs {
		if rec.path != "/responses" || rec.query != "" {
			t.Fatalf("paths=%+v", recs)
		}
		requireExactArrival(t, rec, fx.responsesURL())
	}
	requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindStream, cliproxyauth.GenerationKindStreamBootstrap)
	requireDistinctAttempts(t, fx.boundary.attempts())
	requireSameCredential(t, fx.boundary.identities())
	boot, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindStreamBootstrap)
	if !ok || !boot.result.Success || !boot.sent || boot.attemptID != fx.boundary.attempts()[1] {
		t.Fatalf("stream-bootstrap result=%+v", boot)
	}
	requireMatchedPin(t, boot.matched, fx.responsesURL())
	for _, kind := range fx.boundary.kinds() {
		if kind == cliproxyauth.GenerationKindStreamRefresh {
			t.Fatal("HTTP 200 pre-payload SSE auth error selected stream-refresh")
		}
	}
	requireIdleB(t, fx)
}

func TestOCGNativeInternalAdmitHTTP(t *testing.T) {
	fx := startNativeCriticalFixture(t, false, nil)
	req, _ := nativeCriticalRequest(false)
	var parentCtx context.Context
	fx.boundary.hook = func(ctx context.Context, auth *cliproxyauth.Auth, kind string) {
		if kind != cliproxyauth.GenerationKindExecute || parentCtx != nil {
			return
		}
		parentCtx = ctx
		child, err := cliproxyauth.AdmitInternalGeneration(ctx, auth, nativeCriticalModel)
		if err != nil {
			t.Errorf("child admit: %v", err)
			return
		}
		if cliproxyauth.CurrentAttemptID(child) == "" {
			t.Error("child attempt id missing")
		}
		_, childErr := fx.exec.Execute(child, auth, req, executorOpts())
		if pub := cliproxyauth.PublishInternalGeneration(child, auth, nativeCriticalModel, childErr); pub != nil {
			t.Errorf("publish child: %v", pub)
		}
		if childErr != nil {
			t.Errorf("child execute: %v", childErr)
		}
	}
	if _, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, executorOpts()); err != nil {
		t.Fatal(err)
	}
	if fx.count.hits.Load() != 2 {
		t.Fatalf("allowed parent+child hits=%d", fx.count.hits.Load())
	}
	if parentCtx == nil {
		t.Fatal("missing parent context")
	}
	sent, _, _, _, _, _, _, _, _ := cliproxyauth.GenerationFacts(parentCtx)
	if !sent || cliproxyauth.MatchedEndpointPin(parentCtx) == nil {
		t.Fatal("parent permit was not consumed independently")
	}
	internal, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindInternal)
	if !ok || !internal.result.Success || !internal.sent || internal.attemptID == cliproxyauth.CurrentAttemptID(parentCtx) {
		t.Fatalf("internal publication=%+v parent=%s", internal, cliproxyauth.CurrentAttemptID(parentCtx))
	}
	requireMatchedPin(t, internal.matched, fx.responsesURL())
	parent, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindExecute)
	if !ok || !parent.result.Success || parent.attemptID == internal.attemptID {
		t.Fatalf("parent result=%+v internal=%+v", parent, internal)
	}

	before := fx.count.hits.Load()
	fx.boundary.setExplicit(cliproxyauth.GenerationKindInternal, []cliproxyauth.EndpointPin{publicPin(t, fx.backupURL(), "lb-wrong")})
	var deniedParent context.Context
	fx.boundary.hook = func(ctx context.Context, auth *cliproxyauth.Auth, kind string) {
		if kind != cliproxyauth.GenerationKindExecute || deniedParent != nil {
			return
		}
		deniedParent = ctx
		child, err := cliproxyauth.AdmitInternalGeneration(ctx, auth, nativeCriticalModel)
		if err != nil {
			t.Errorf("denied child admit: %v", err)
			return
		}
		_, denyErr := fx.exec.Execute(child, auth, req, executorOpts())
		requireUnsentNull(t, child, denyErr)
		if pub := cliproxyauth.PublishInternalGeneration(child, auth, nativeCriticalModel, denyErr); pub != nil && !errors.Is(pub, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(pub) {
			t.Errorf("publish denied child: %v", pub)
		}
		sent, _, _, _, _, _, _, _, _ = cliproxyauth.GenerationFacts(ctx)
		if sent || cliproxyauth.MatchedEndpointPin(ctx) != nil {
			t.Error("parent permit consumed by denied child")
		}
	}
	_, parentErr := fx.manager.Execute(context.Background(), []string{"codex"}, req, executorOpts())
	if parentErr == nil || (!errors.Is(parentErr, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(parentErr)) {
		t.Fatalf("stopped parent send = %v", parentErr)
	}
	if fx.count.hits.Load() != before {
		t.Fatalf("denied child/parent sent HTTP hits=%d", fx.count.hits.Load())
	}
	if deniedParent == nil {
		t.Fatal("missing denied parent context")
	}
	sent, _, _, _, _, transport, _, _, _ := cliproxyauth.GenerationFacts(deniedParent)
	if sent || cliproxyauth.MatchedEndpointPin(deniedParent) != nil {
		t.Fatal("stopped parent consumed a pin")
	}
	if transport != "" && transport != "unsent" {
		t.Fatalf("parent transport=%q", transport)
	}
	requireIdleB(t, fx)
	kinds := fx.boundary.kinds()
	if len(kinds) < 4 || kinds[0] != cliproxyauth.GenerationKindExecute || kinds[1] != cliproxyauth.GenerationKindInternal {
		t.Fatalf("kinds=%v", kinds)
	}
}

func TestOCGNativeSevenKindsFollowupDenyHTTP(t *testing.T) {
	t.Run("execute", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, nil)
		req, opts := nativeCriticalRequest(false)
		if _, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts); err != nil {
			t.Fatal(err)
		}
		requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindExecute)
		got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindExecute)
		if !ok || !got.result.Success || !got.sent || got.attemptID == "" {
			t.Fatalf("execute result=%+v", got)
		}
		requireMatchedPin(t, got.matched, fx.responsesURL())
		denyNamedFollowup(t, fx, cliproxyauth.GenerationKindExecute, func() error {
			_, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts)
			return err
		})
	})
	t.Run("refresh-resend", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, firstUnauthorizedHit)
		req, opts := nativeCriticalRequest(false)
		if _, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts); err != nil {
			t.Fatal(err)
		}
		requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindExecute, cliproxyauth.GenerationKindRefreshResend)
		requireDistinctAttempts(t, fx.boundary.attempts())
		got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindRefreshResend)
		if !ok || !got.result.Success || !got.sent || got.attemptID != fx.boundary.attempts()[1] {
			t.Fatalf("refresh-resend result=%+v", got)
		}
		requireMatchedPin(t, got.matched, fx.responsesURL())
		fx.count.onHit = nextHitEquals(fx.count.hits.Load()+1, writeUnauthorized)
		before := fx.count.hits.Load()
		beforeDec := fx.boundary.decisionLen()
		fx.boundary.refuseKind(cliproxyauth.GenerationKindRefreshResend)
		_, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts)
		if fx.count.hits.Load() != before+1 {
			t.Fatalf("refresh-resend followup hits=%d before=%d", fx.count.hits.Load(), before)
		}
		requireNamedRefusal(t, fx, cliproxyauth.GenerationKindRefreshResend, beforeDec, got.attemptID, err)
		requireIdleB(t, fx)
	})
	t.Run("stream", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, nil)
		req, opts := nativeCriticalRequest(true)
		stream, err := fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
		if err != nil {
			t.Fatal(err)
		}
		drainNativeStream(t, stream)
		requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindStream)
		got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindStream)
		if !ok || !got.sent || got.attemptID == "" {
			t.Fatalf("stream result=%+v", got)
		}
		requireMatchedPin(t, got.matched, fx.responsesURL())
		denyNamedFollowup(t, fx, cliproxyauth.GenerationKindStream, func() error {
			_, err := fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
			return err
		})
	})
	t.Run("stream-refresh", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, firstUnauthorizedHit)
		req, opts := nativeCriticalRequest(true)
		stream, err := fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
		if err != nil {
			t.Fatal(err)
		}
		drainNativeStream(t, stream)
		requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindStream, cliproxyauth.GenerationKindStreamRefresh)
		got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindStreamRefresh)
		if !ok || !got.sent || got.attemptID != fx.boundary.attempts()[1] {
			t.Fatalf("stream-refresh result=%+v", got)
		}
		requireMatchedPin(t, got.matched, fx.responsesURL())
		fx.count.onHit = nextHitEquals(fx.count.hits.Load()+1, writeUnauthorized)
		before := fx.count.hits.Load()
		beforeDec := fx.boundary.decisionLen()
		fx.boundary.refuseKind(cliproxyauth.GenerationKindStreamRefresh)
		stream, err = fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
		if fx.count.hits.Load() != before+1 {
			t.Fatalf("stream-refresh followup hits=%d before=%d", fx.count.hits.Load(), before)
		}
		requireNamedRefusal(t, fx, cliproxyauth.GenerationKindStreamRefresh, beforeDec, got.attemptID, err)
		requireIntendedStop(t, stream, err)
		requireIdleB(t, fx)
	})
	t.Run("stream-bootstrap", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, true, firstAuthErrorHit)
		req, opts := nativeCriticalRequest(true)
		stream, err := fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
		if err != nil {
			t.Fatal(err)
		}
		drainNativeStream(t, stream)
		requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindStream, cliproxyauth.GenerationKindStreamBootstrap)
		got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindStreamBootstrap)
		if !ok || !got.sent || got.attemptID != fx.boundary.attempts()[1] {
			t.Fatalf("stream-bootstrap result=%+v", got)
		}
		requireMatchedPin(t, got.matched, fx.responsesURL())
		fx.count.onHit = nextHitEquals(fx.count.hits.Load()+1, writeAuthErrorSSE)
		before := fx.count.hits.Load()
		beforeDec := fx.boundary.decisionLen()
		fx.boundary.refuseKind(cliproxyauth.GenerationKindStreamBootstrap)
		stream, err = fx.manager.ExecuteStream(context.Background(), []string{"codex"}, req, opts)
		if fx.count.hits.Load() != before+1 {
			t.Fatalf("stream-bootstrap followup hits=%d before=%d", fx.count.hits.Load(), before)
		}
		requireNamedRefusal(t, fx, cliproxyauth.GenerationKindStreamBootstrap, beforeDec, got.attemptID, err)
		requireIntendedStop(t, stream, err)
		requireIdleB(t, fx)
	})
	t.Run("internal", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, nil)
		req, _ := nativeCriticalRequest(false)
		fx.boundary.hook = func(ctx context.Context, auth *cliproxyauth.Auth, kind string) {
			if kind != cliproxyauth.GenerationKindExecute {
				return
			}
			child, err := cliproxyauth.AdmitInternalGeneration(ctx, auth, nativeCriticalModel)
			if err != nil {
				t.Errorf("child admit: %v", err)
				return
			}
			_, childErr := fx.exec.Execute(child, auth, req, executorOpts())
			_ = cliproxyauth.PublishInternalGeneration(child, auth, nativeCriticalModel, childErr)
		}
		if _, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, executorOpts()); err != nil {
			t.Fatal(err)
		}
		if fx.boundary.kinds()[1] != cliproxyauth.GenerationKindInternal {
			t.Fatalf("kinds=%v", fx.boundary.kinds())
		}
		internal, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindInternal)
		if !ok || !internal.sent {
			t.Fatalf("internal result=%+v", internal)
		}
		beforeDec := fx.boundary.decisionLen()
		var denyErr error
		fx.boundary.hook = func(ctx context.Context, auth *cliproxyauth.Auth, kind string) {
			if kind != cliproxyauth.GenerationKindExecute {
				return
			}
			fx.boundary.refuseKind(cliproxyauth.GenerationKindInternal)
			_, denyErr = cliproxyauth.AdmitInternalGeneration(ctx, auth, nativeCriticalModel)
		}
		before := fx.count.hits.Load()
		_, parentErr := fx.manager.Execute(context.Background(), []string{"codex"}, req, executorOpts())
		if fx.count.hits.Load() != before {
			t.Fatalf("internal followup sent HTTP")
		}
		requireNamedRefusal(t, fx, cliproxyauth.GenerationKindInternal, beforeDec, internal.attemptID, denyErr)
		if parentErr != nil && !errors.Is(parentErr, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(parentErr) {
			t.Fatalf("parent after denied internal admit = %v", parentErr)
		}
		requireIdleB(t, fx)
	})
	t.Run("count-tokens", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, nil)
		before := fx.count.hits.Load()
		req, opts := nativeCriticalRequest(false)
		if _, err := fx.manager.ExecuteCount(context.Background(), []string{"codex"}, req, opts); err != nil {
			t.Fatal(err)
		}
		if fx.count.hits.Load() != before {
			t.Fatalf("local count sent HTTP hits=%d", fx.count.hits.Load())
		}
		requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindCount)
		countCtx := fx.boundary.contextFor(cliproxyauth.GenerationKindCount)
		if countCtx == nil {
			t.Fatal("missing count context")
		}
		if cliproxyauth.MatchedEndpointPin(countCtx) != nil {
			t.Fatalf("count matched pin=%+v", cliproxyauth.MatchedEndpointPin(countCtx))
		}
		sent, _, _, _, _, transport, _, _, _ := cliproxyauth.GenerationFacts(countCtx)
		if sent || (transport != "" && transport != "unsent") {
			t.Fatalf("count facts sent=%v transport=%q", sent, transport)
		}
		got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindCount)
		if !ok || got.sent || got.matched != nil {
			t.Fatalf("count result=%+v", got)
		}
		_, err := fx.exec.Execute(countCtx, liveAuth(t, fx.manager, "oauth-a"), req, executorOpts())
		requireUnsentNull(t, countCtx, err)
		if fx.count.hits.Load() != before {
			t.Fatal("count followup arrived at server")
		}
		denyNamedFollowup(t, fx, cliproxyauth.GenerationKindCount, func() error {
			_, err := fx.manager.ExecuteCount(context.Background(), []string{"codex"}, req, opts)
			return err
		})
	})
}

func TestOCGNativeCompactAltHTTP(t *testing.T) {
	fx := startNativeCriticalFixture(t, false, func(_ int32, w http.ResponseWriter, r *http.Request) bool {
		if r.Method != http.MethodPost || r.URL.Path != "/responses/compact" || r.URL.RawQuery != "" {
			t.Errorf("compact target method=%s path=%s query=%s", r.Method, r.URL.Path, r.URL.RawQuery)
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(nativeCriticalCompactJSON))
		return true
	})
	payload := []byte(`{"model":"` + nativeCriticalModel + `","input":[{"type":"message","role":"user","content":"history"},{"type":"compaction_trigger"}]}`)
	req := cliproxyexecutor.Request{Model: nativeCriticalModel, Payload: payload}
	opts := cliproxyexecutor.Options{
		Stream:          false,
		Alt:             "responses/compact",
		SourceFormat:    sdktranslator.FromString("openai-response"),
		ResponseFormat:  sdktranslator.FormatOpenAIResponse,
		OriginalRequest: payload,
	}
	resp, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts)
	if err != nil {
		t.Fatal(err)
	}
	if fx.count.hits.Load() != 1 {
		t.Fatalf("compact hits=%d", fx.count.hits.Load())
	}
	recs := fx.count.captured()
	if len(recs) != 1 || recs[0].path != "/responses/compact" || recs[0].query != "" {
		t.Fatalf("paths=%+v", recs)
	}
	requireExactArrival(t, recs[0], fx.compactURL())
	if !strings.Contains(string(resp.Payload), "response.compaction") && !strings.Contains(string(resp.Payload), "resp_compact") {
		t.Fatalf("payload=%s", resp.Payload)
	}
	requireKinds(t, fx.boundary.kinds(), cliproxyauth.GenerationKindExecute)
	got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindExecute)
	if !ok || !got.result.Success || !got.sent || got.attemptID == "" {
		t.Fatalf("compact result=%+v", got)
	}
	requireMatchedPin(t, got.matched, fx.compactURL())
	for _, kind := range fx.boundary.kinds() {
		if kind == cliproxyauth.GenerationKindInternal {
			t.Fatal("manager compact selected internal; public compaction is a separate C leaf")
		}
	}
	requireIdleB(t, fx)
	denyNamedFollowup(t, fx, cliproxyauth.GenerationKindExecute, func() error {
		_, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts)
		return err
	})
}

func TestOCGNativePinDenialBeforeCountingTransport(t *testing.T) {
	t.Run("missing", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, nil)
		fx.boundary.setEmpty(cliproxyauth.GenerationKindExecute)
		beforeHits := fx.count.hits.Load()
		beforeInvoked := fx.transport.invoked.Load()
		req, opts := nativeCriticalRequest(false)
		_, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts)
		ctx := fx.boundary.contextFor(cliproxyauth.GenerationKindExecute)
		if ctx == nil {
			t.Fatal("missing execute context")
		}
		requireUnsentNull(t, ctx, err)
		if fx.count.hits.Load() != beforeHits || fx.transport.passed.Load() != 0 {
			t.Fatalf("missing pin arrived hits=%d passed=%d", fx.count.hits.Load(), fx.transport.passed.Load())
		}
		httpReq := postRequest(t, ctx, fx.responsesURL())
		if _, tripErr := fx.transport.RoundTrip(httpReq); tripErr == nil || (!errors.Is(tripErr, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(tripErr)) {
			t.Fatalf("counting transport missing pin = %v", tripErr)
		}
		if fx.transport.invoked.Load() != beforeInvoked+1 || fx.transport.passed.Load() != 0 || fx.count.hits.Load() != beforeHits {
			t.Fatalf("missing pin transport invoked=%d passed=%d hits=%d", fx.transport.invoked.Load(), fx.transport.passed.Load(), fx.count.hits.Load())
		}
		got, ok := fx.boundary.lastResult(cliproxyauth.GenerationKindExecute)
		if ok && (got.sent || got.matched != nil) {
			t.Fatalf("missing pin result=%+v", got)
		}
		requireIdleB(t, fx)
	})
	t.Run("wrong", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, nil)
		fx.boundary.setExplicit(cliproxyauth.GenerationKindExecute, []cliproxyauth.EndpointPin{publicPin(t, fx.backupURL(), "lb-b")})
		beforeHits := fx.count.hits.Load()
		beforeInvoked := fx.transport.invoked.Load()
		req, opts := nativeCriticalRequest(false)
		_, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts)
		ctx := fx.boundary.contextFor(cliproxyauth.GenerationKindExecute)
		requireUnsentNull(t, ctx, err)
		if fx.count.hits.Load() != beforeHits {
			t.Fatalf("wrong pin arrived at A hits=%d", fx.count.hits.Load())
		}
		httpReq := postRequest(t, ctx, fx.responsesURL())
		if _, tripErr := fx.transport.RoundTrip(httpReq); tripErr == nil {
			t.Fatal("wrong pin counting transport passed")
		}
		if fx.transport.invoked.Load() != beforeInvoked+1 || fx.transport.passed.Load() != 0 || fx.count.backup.Load() != 0 {
			t.Fatalf("wrong pin transport invoked=%d passed=%d B=%d", fx.transport.invoked.Load(), fx.transport.passed.Load(), fx.count.backup.Load())
		}
		requireIdleB(t, fx)
	})
	t.Run("host", func(t *testing.T) {
		fx := startNativeCriticalFixture(t, false, nil)
		beforeHits := fx.count.hits.Load()
		beforeInvoked := fx.transport.invoked.Load()
		var tripErr error
		fx.observer.beforeExecute = func(ctx context.Context, _ *cliproxyauth.Auth) bool {
			httpReq := postRequest(t, ctx, fx.responsesURL())
			httpReq.Host = "evil.example"
			_, tripErr = fx.transport.RoundTrip(httpReq)
			return true
		}
		req, opts := nativeCriticalRequest(false)
		_, err := fx.manager.Execute(context.Background(), []string{"codex"}, req, opts)
		if err == nil {
			t.Fatal("host mutation left Execute successful")
		}
		if tripErr == nil || (!errors.Is(tripErr, cliproxyauth.ErrAttemptStop) && !cliproxyauth.RequestStopped(tripErr)) {
			t.Fatalf("host mutation = %v", tripErr)
		}
		if fx.transport.invoked.Load() != beforeInvoked+1 || fx.transport.passed.Load() != 0 || fx.count.hits.Load() != beforeHits {
			t.Fatalf("host mutation invoked=%d passed=%d hits=%d", fx.transport.invoked.Load(), fx.transport.passed.Load(), fx.count.hits.Load())
		}
		ctx := fx.boundary.contextFor(cliproxyauth.GenerationKindExecute)
		if cliproxyauth.MatchedEndpointPin(ctx) != nil {
			t.Fatalf("host mutation consumed pin=%+v", cliproxyauth.MatchedEndpointPin(ctx))
		}
		sent, _, _, _, _, _, _, _, _ := cliproxyauth.GenerationFacts(ctx)
		if sent {
			t.Fatal("host mutation marked sent")
		}
		requireIdleB(t, fx)
	})
}

func postRequest(t *testing.T, ctx context.Context, raw string) *http.Request {
	t.Helper()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, raw, nil)
	if err != nil {
		t.Fatal(err)
	}
	return req
}

func denyNamedFollowup(t *testing.T, fx *nativeCriticalFixture, kind string, trigger func() error) {
	t.Helper()
	beforeHits := fx.count.hits.Load()
	beforeDec := fx.boundary.decisionLen()
	previous := ""
	if attempts := fx.boundary.attempts(); len(attempts) > 0 {
		previous = attempts[len(attempts)-1]
	}
	fx.boundary.refuseKind(kind)
	err := trigger()
	if fx.count.hits.Load() != beforeHits {
		t.Fatalf("%s followup arrived at server before=%d after=%d", kind, beforeHits, fx.count.hits.Load())
	}
	requireNamedRefusal(t, fx, kind, beforeDec, previous, err)
	requireIdleB(t, fx)
}
