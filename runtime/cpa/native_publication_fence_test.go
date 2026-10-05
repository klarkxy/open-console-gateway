package main

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

func TestStaleRegistrationCannotRepublishCurrentScope(t *testing.T) {
	unrelatedModel := "ocg-unrelated-model"
	t.Run("replacement", func(t *testing.T) {
		runPublicationFence(t, publicationFence{
			file: "codex-a.json", credentialID: "oauth-a", version: "4",
			initial: []string{nativeModel, otherModel},
			mutate: func(binding *OAuthBinding) {
				binding.CredentialID = "oauth-a-next"
				binding.CredentialVersion = decimalString{value: "9"}
				binding.Models = []string{nativeModel}
			},
			wantID: "oauth-a-next", wantVersion: "9", wantModels: []string{nativeModel},
			absent: []string{otherModel}, unrelatedModel: unrelatedModel,
		})
	})
	t.Run("same-credential-narrowing", func(t *testing.T) {
		runPublicationFence(t, publicationFence{
			file: "codex-a.json", credentialID: "oauth-a", version: "4",
			initial: []string{nativeModel, otherModel},
			mutate: func(binding *OAuthBinding) {
				binding.Models = []string{nativeModel}
			},
			wantID: "oauth-a", wantVersion: "4", wantModels: []string{nativeModel},
			absent: []string{otherModel}, unrelatedModel: unrelatedModel,
		})
	})
	t.Run("old-bound-empty", func(t *testing.T) {
		runPublicationFence(t, publicationFence{
			file: "codex-empty.json", credentialID: "oauth-empty", version: "4",
			initial: nil,
			mutate: func(binding *OAuthBinding) {
				binding.Models = []string{nativeModel}
			},
			wantID: "oauth-empty", wantVersion: "4", wantModels: []string{nativeModel},
			unrelatedModel: unrelatedModel,
		})
	})
}

type publicationFence struct {
	file, credentialID, version string
	initial                     []string
	mutate                      func(*OAuthBinding)
	wantID, wantVersion         string
	wantModels, absent          []string
	unrelatedModel              string
}

func runPublicationFence(t *testing.T, spec publicationFence) {
	t.Helper()
	var targetHits, otherHits atomic.Int32
	targetUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			targetHits.Add(1)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer targetUpstream.Close()
	otherUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			otherHits.Add(1)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer otherUpstream.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{
				file: spec.file, id: spec.credentialID, version: spec.version, priority: "1", provider: "canonical-oauth",
				models: append([]string(nil), spec.initial...), access: "token-target", baseURL: targetUpstream.URL, kind: "codex",
			},
			{
				file: "codex-b.json", id: "oauth-b", version: "4", priority: "2", provider: "canonical-oauth",
				models: []string{spec.unrelatedModel}, access: "token-other", baseURL: otherUpstream.URL, kind: "codex",
			},
		},
	})
	target := nativeAuth(t, host, spec.credentialID)
	other := nativeAuth(t, host, "oauth-b")
	waitModelIDs(t, target.ID, spec.initial)
	waitModelIDs(t, other.ID, []string{spec.unrelatedModel})
	otherBefore := append([]string(nil), coreauth.OCGRegisteredModelIDs(other.ID)...)

	entered := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	var releaseOnce sync.Once
	releaseHold := func() { releaseOnce.Do(func() { close(release) }) }
	var capturedScope string
	var capturedVersion string
	coreauth.OCGHoldModelPublication = func(captured *coreauth.Auth) {
		if captured == nil || captured.ID != target.ID {
			return
		}
		once.Do(func() {
			if captured.Attributes != nil {
				capturedScope = captured.Attributes["ocg_model_scope"]
			}
			capturedVersion = metadataText(captured, "ocg_credential_version")
			close(entered)
			<-release
		})
	}
	t.Cleanup(func() {
		coreauth.OCGHoldModelPublication = nil
		releaseHold()
	})
	writeAuthNote(t, host, spec.file, "publication-fence-"+spec.file)
	select {
	case <-entered:
	case <-time.After(8 * time.Second):
		t.Fatal("registration did not reach publication")
	}
	if capturedVersion != spec.version || capturedScope != joinModelScope(spec.initial) {
		t.Fatalf("held snapshot version=%s scope=%q want version %s scope %q", capturedVersion, capturedScope, spec.version, joinModelScope(spec.initial))
	}
	mutateBinding(t, host, spec.file, spec.mutate)
	err := host.stamp()
	releaseHold()
	if err != nil {
		t.Fatal(err)
	}
	waitModelIDs(t, target.ID, spec.wantModels)
	current, ok := host.service.CoreAuthManager().GetByID(target.ID)
	if !ok || current == nil {
		t.Fatal("target auth disappeared")
	}
	if metadataText(current, "ocg_credential_id") != spec.wantID || metadataText(current, "ocg_credential_version") != spec.wantVersion || current.Attributes["ocg_model_scope"] != joinModelScope(spec.wantModels) {
		t.Fatalf("current id=%s version=%s scope=%q", metadataText(current, "ocg_credential_id"), metadataText(current, "ocg_credential_version"), current.Attributes["ocg_model_scope"])
	}
	for _, id := range spec.absent {
		for _, got := range coreauth.OCGRegisteredModelIDs(target.ID) {
			if got == id {
				t.Fatalf("superseded model %s was republished: %v", id, coreauth.OCGRegisteredModelIDs(target.ID))
			}
		}
	}
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(other.ID), otherBefore) {
		t.Fatalf("unrelated registration = %v want %v", coreauth.OCGRegisteredModelIDs(other.ID), otherBefore)
	}

	allowed := postPolicy(t, host, "/v1/responses", `{"model":"`+spec.wantModels[0]+`","input":"hello"}`)
	if targetHits.Load() != 1 || otherHits.Load() != 0 || allowed.status >= 400 {
		t.Fatalf("current model hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), allowed.status, allowed.body)
	}
	for _, id := range spec.absent {
		refused := postPolicy(t, host, "/v1/responses", `{"model":"`+id+`","input":"hello"}`)
		if targetHits.Load() != 1 || otherHits.Load() != 0 || refused.status < 400 {
			t.Fatalf("superseded %s hits target=%d other=%d status=%d body=%s", id, targetHits.Load(), otherHits.Load(), refused.status, refused.body)
		}
	}
	unrelated := postPolicy(t, host, "/v1/responses", `{"model":"`+spec.unrelatedModel+`","input":"hello"}`)
	if otherHits.Load() != 1 || targetHits.Load() != 1 || unrelated.status >= 400 {
		t.Fatalf("unrelated hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), unrelated.status, unrelated.body)
	}
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(target.ID), spec.wantModels) || !sameModelIDs(coreauth.OCGRegisteredModelIDs(other.ID), otherBefore) {
		t.Fatalf("registration after admission target=%v other=%v", coreauth.OCGRegisteredModelIDs(target.ID), coreauth.OCGRegisteredModelIDs(other.ID))
	}
}

func mutateBinding(t *testing.T, host *Host, file string, mutate func(*OAuthBinding)) {
	t.Helper()
	host.mu.Lock()
	defer host.mu.Unlock()
	for i := range host.document.OAuthBindings {
		if host.document.OAuthBindings[i].RelativePath == file {
			mutate(&host.document.OAuthBindings[i])
			return
		}
	}
	t.Fatalf("binding %s missing", file)
}

func waitModelIDs(t *testing.T, clientID string, want []string) {
	t.Helper()
	deadline := time.Now().Add(8 * time.Second)
	stable := 0
	for time.Now().Before(deadline) {
		if sameModelIDs(coreauth.OCGRegisteredModelIDs(clientID), want) {
			stable++
			if stable >= 3 {
				return
			}
		} else {
			stable = 0
		}
		time.Sleep(15 * time.Millisecond)
	}
	t.Fatalf("client %s models = %v want %v", clientID, coreauth.OCGRegisteredModelIDs(clientID), want)
}

func joinModelScope(models []string) string {
	if len(models) == 0 {
		return ""
	}
	out := models[0]
	for _, model := range models[1:] {
		out += "," + model
	}
	return out
}

func TestStaleDisabledUnregisterCannotEraseEnabledScope(t *testing.T) {
	unrelatedModel := "ocg-unrelated-disabled"
	targetUpstream, otherUpstream, targetHits, otherHits, policyURL := openFenceUpstreams(t)
	host := startNative(t, policyURL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{
				file: "codex-reenable.json", id: "oauth-reenable", version: "4", priority: "1", provider: "canonical-oauth",
				models: []string{otherModel}, access: "token-reenable", baseURL: targetUpstream.URL, kind: "codex",
			},
			{
				file: "codex-reenable-other.json", id: "oauth-reenable-other", version: "4", priority: "2", provider: "canonical-oauth",
				models: []string{unrelatedModel}, access: "token-reenable-other", baseURL: otherUpstream.URL, kind: "codex",
			},
		},
	})
	target := nativeAuth(t, host, "oauth-reenable")
	other := nativeAuth(t, host, "oauth-reenable-other")
	waitModelIDs(t, target.ID, []string{otherModel})
	waitModelIDs(t, other.ID, []string{unrelatedModel})
	otherBefore := append([]string(nil), coreauth.OCGRegisteredModelIDs(other.ID)...)
	hold := newCommitHold(t, func() { coreauth.OCGHoldDisabledUnregister = nil })
	coreauth.OCGHoldDisabledUnregister = func(captured *coreauth.Auth, commit func()) {
		if captured == nil || captured.ID != target.ID || !captured.Disabled {
			commit()
			return
		}
		hold.pause(nil, commit)
	}
	setAuthFileDisabled(t, host, "codex-reenable.json", true)
	hold.waitEntered(t)
	setAuthFileDisabled(t, host, "codex-reenable.json", false)
	held, ok := host.service.CoreAuthManager().GetByID(target.ID)
	if !ok || held == nil || !held.Disabled {
		t.Fatal("held registration did not keep the disabled auth")
	}
	enabled := held.Clone()
	enabled.Disabled = false
	enabled.Status = coreauth.StatusActive
	if _, err := host.service.CoreAuthManager().Update(context.Background(), enabled); err != nil {
		t.Fatal(err)
	}
	waitManagerEnabled(t, host, target.ID)
	mutateBinding(t, host, "codex-reenable.json", func(binding *OAuthBinding) {
		binding.Models = []string{nativeModel}
	})
	if err := host.stamp(); err != nil {
		t.Fatal(err)
	}
	hold.finish(t)
	waitModelIDs(t, target.ID, []string{nativeModel})
	current, ok := host.service.CoreAuthManager().GetByID(target.ID)
	if !ok || current == nil || current.Disabled || metadataText(current, "ocg_credential_id") != "oauth-reenable" || scopeOf(current) != nativeModel {
		t.Fatalf("re-enabled auth id=%s disabled=%v scope=%q", metadataText(current, "ocg_credential_id"), current != nil && current.Disabled, scopeOf(current))
	}
	assertFencedPublication(t, host, target.ID, other.ID, otherBefore, targetHits, otherHits, []string{nativeModel}, []string{otherModel}, unrelatedModel)
}

func TestCurrentDisabledAuthUnregisters(t *testing.T) {
	coreauth.OCGHoldDisabledUnregister = nil
	coreauth.OCGHoldCatalogPublication = nil
	unrelatedModel := "ocg-unrelated-still-disabled"
	targetUpstream, otherUpstream, targetHits, otherHits, policyURL := openFenceUpstreams(t)
	host := startNative(t, policyURL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{
				file: "codex-disable-now.json", id: "oauth-disable-now", version: "4", priority: "1", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "token-disable-now", baseURL: targetUpstream.URL, kind: "codex",
			},
			{
				file: "codex-disable-other.json", id: "oauth-disable-other", version: "4", priority: "2", provider: "canonical-oauth",
				models: []string{unrelatedModel}, access: "token-disable-other", baseURL: otherUpstream.URL, kind: "codex",
			},
		},
	})
	target := nativeAuth(t, host, "oauth-disable-now")
	other := nativeAuth(t, host, "oauth-disable-other")
	waitModelIDs(t, target.ID, []string{nativeModel})
	waitModelIDs(t, other.ID, []string{unrelatedModel})
	otherBefore := append([]string(nil), coreauth.OCGRegisteredModelIDs(other.ID)...)
	setAuthFileDisabled(t, host, "codex-disable-now.json", true)
	waitModelIDs(t, target.ID, nil)
	current, ok := host.service.CoreAuthManager().GetByID(target.ID)
	if !ok || current == nil || !current.Disabled {
		t.Fatal("disabled auth was not retained")
	}
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(other.ID), otherBefore) {
		t.Fatalf("unrelated registration = %v want %v", coreauth.OCGRegisteredModelIDs(other.ID), otherBefore)
	}
	refused := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	if targetHits.Load() != 0 || otherHits.Load() != 0 || refused.status < 400 {
		t.Fatalf("disabled hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), refused.status, refused.body)
	}
	unrelated := postPolicy(t, host, "/v1/responses", `{"model":"`+unrelatedModel+`","input":"hello"}`)
	if otherHits.Load() != 1 || targetHits.Load() != 0 || unrelated.status >= 400 {
		t.Fatalf("unrelated hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), unrelated.status, unrelated.body)
	}
}

func TestStaleCatalogWriteCannotReplaceBoundScope(t *testing.T) {
	t.Run("stock-register", func(t *testing.T) {
		runStaleCatalogWrite(t, false)
	})
	t.Run("empty-unregister", func(t *testing.T) {
		runStaleCatalogWrite(t, true)
	})
	t.Run("unbound-discovery", func(t *testing.T) {
		runUnboundDiscovery(t)
	})
}

func runStaleCatalogWrite(t *testing.T, empty bool) {
	t.Helper()
	file := "codex-stock.json"
	otherFile := "codex-stock-other.json"
	credentialID := "oauth-stock"
	if empty {
		file = "codex-emptycat.json"
		otherFile = "codex-emptycat-other.json"
		credentialID = "oauth-emptycat"
	}
	unrelatedModel := "ocg-unrelated-" + credentialID
	targetUpstream, otherUpstream, targetHits, otherHits, policyURL := openFenceUpstreams(t)
	host := startNative(t, policyURL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{
			file: otherFile, id: credentialID + "-other", version: "4", priority: "2", provider: "canonical-oauth",
			models: []string{unrelatedModel}, access: "token-" + credentialID + "-other", baseURL: otherUpstream.URL, kind: "codex",
		}},
	})
	other := nativeAuth(t, host, credentialID+"-other")
	waitModelIDs(t, other.ID, []string{unrelatedModel})
	otherBefore := append([]string(nil), coreauth.OCGRegisteredModelIDs(other.ID)...)
	var excluded, capturedID string
	hold := newCommitHold(t, func() { coreauth.OCGHoldCatalogPublication = nil })
	coreauth.OCGHoldCatalogPublication = func(captured *coreauth.Auth, commit func()) {
		if !sameAuthFile(captured, file) {
			commit()
			return
		}
		hold.pause(func() {
			capturedID = captured.ID
			if captured.Attributes != nil {
				excluded = captured.Attributes["excluded_models"]
			}
		}, commit)
	}
	var exclusions []string
	if empty {
		exclusions = []string{"*"}
	}
	writeLooseCodex(t, host, file, targetUpstream.URL, exclusions)
	hold.waitEntered(t)
	if empty && !strings.Contains(excluded, "*") {
		t.Fatalf("held exclusions = %q", excluded)
	}
	if !empty && strings.Contains(excluded, "*") {
		t.Fatalf("stock exclusions = %q", excluded)
	}
	if capturedID == "" {
		t.Fatal("held auth missing")
	}
	if got := coreauth.OCGRegisteredModelIDs(capturedID); len(got) != 0 {
		t.Fatalf("registry changed before the held commit: %v", got)
	}
	host.mu.Lock()
	host.document.OAuthBindings = append(host.document.OAuthBindings, OAuthBinding{
		RelativePath:      file,
		CredentialID:      credentialID,
		CredentialVersion: decimalString{value: "4"},
		ProviderID:        "canonical-oauth",
		Models:            []string{nativeModel},
		Priority:          decimalString{value: "1"},
	})
	host.mu.Unlock()
	if err := host.stamp(); err != nil {
		t.Fatal(err)
	}
	hold.finish(t)
	waitModelIDs(t, capturedID, []string{nativeModel})
	current, ok := host.service.CoreAuthManager().GetByID(capturedID)
	if !ok || current == nil || metadataText(current, "ocg_credential_id") != credentialID || scopeOf(current) != nativeModel {
		t.Fatalf("bound auth id=%s scope=%q", metadataText(current, "ocg_credential_id"), scopeOf(current))
	}
	assertFencedPublication(t, host, capturedID, other.ID, otherBefore, targetHits, otherHits, []string{nativeModel}, []string{otherModel}, unrelatedModel)
}

func runUnboundDiscovery(t *testing.T) {
	t.Helper()
	coreauth.OCGHoldCatalogPublication = nil
	coreauth.OCGHoldDisabledUnregister = nil
	unrelatedModel := "ocg-unrelated-discovery"
	targetUpstream, otherUpstream, targetHits, otherHits, policyURL := openFenceUpstreams(t)
	host := startNative(t, policyURL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{{
			file: "codex-discovery-other.json", id: "oauth-discovery-other", version: "4", priority: "2", provider: "canonical-oauth",
			models: []string{unrelatedModel}, access: "token-discovery-other", baseURL: otherUpstream.URL, kind: "codex",
		}},
	})
	other := nativeAuth(t, host, "oauth-discovery-other")
	waitModelIDs(t, other.ID, []string{unrelatedModel})
	otherBefore := append([]string(nil), coreauth.OCGRegisteredModelIDs(other.ID)...)
	writeLooseCodex(t, host, "codex-discovery.json", targetUpstream.URL, nil)
	loose := waitAuthByFile(t, host, "codex-discovery.json")
	got := waitCatalogIDs(t, loose.ID, []string{nativeModel, otherModel})
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(other.ID), otherBefore) {
		t.Fatalf("unrelated registration = %v want %v", coreauth.OCGRegisteredModelIDs(other.ID), otherBefore)
	}
	refused := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"hello"}`)
	if targetHits.Load() != 0 || otherHits.Load() != 0 || refused.status < 400 {
		t.Fatalf("unmapped catalog hits target=%d other=%d status=%d body=%s models=%v", targetHits.Load(), otherHits.Load(), refused.status, refused.body, got)
	}
	catalogModel := postPolicy(t, host, "/v1/responses", `{"model":"`+otherModel+`","input":"hello"}`)
	if targetHits.Load() != 0 || otherHits.Load() != 0 || catalogModel.status < 400 {
		t.Fatalf("unmapped second catalog model hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), catalogModel.status, catalogModel.body)
	}
	unrelated := postPolicy(t, host, "/v1/responses", `{"model":"`+unrelatedModel+`","input":"hello"}`)
	if otherHits.Load() != 1 || targetHits.Load() != 0 || unrelated.status >= 400 {
		t.Fatalf("unrelated hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), unrelated.status, unrelated.body)
	}
	if !modelIDsContain(coreauth.OCGRegisteredModelIDs(loose.ID), []string{nativeModel, otherModel}) || !sameModelIDs(coreauth.OCGRegisteredModelIDs(other.ID), otherBefore) {
		t.Fatalf("registration after admission loose=%v other=%v", coreauth.OCGRegisteredModelIDs(loose.ID), coreauth.OCGRegisteredModelIDs(other.ID))
	}
}

func assertFencedPublication(t *testing.T, host *Host, targetID, otherID string, otherBefore []string, targetHits, otherHits *atomic.Int32, want, absent []string, unrelatedModel string) {
	t.Helper()
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(targetID), want) {
		t.Fatalf("target registration = %v want %v", coreauth.OCGRegisteredModelIDs(targetID), want)
	}
	for _, id := range absent {
		for _, got := range coreauth.OCGRegisteredModelIDs(targetID) {
			if got == id {
				t.Fatalf("superseded model %s was republished: %v", id, coreauth.OCGRegisteredModelIDs(targetID))
			}
		}
	}
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(otherID), otherBefore) {
		t.Fatalf("unrelated registration = %v want %v", coreauth.OCGRegisteredModelIDs(otherID), otherBefore)
	}
	allowed := postPolicy(t, host, "/v1/responses", `{"model":"`+want[0]+`","input":"hello"}`)
	if targetHits.Load() != 1 || otherHits.Load() != 0 || allowed.status >= 400 {
		t.Fatalf("current model hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), allowed.status, allowed.body)
	}
	for _, id := range absent {
		refused := postPolicy(t, host, "/v1/responses", `{"model":"`+id+`","input":"hello"}`)
		if targetHits.Load() != 1 || otherHits.Load() != 0 || refused.status < 400 {
			t.Fatalf("superseded %s hits target=%d other=%d status=%d body=%s", id, targetHits.Load(), otherHits.Load(), refused.status, refused.body)
		}
	}
	unrelated := postPolicy(t, host, "/v1/responses", `{"model":"`+unrelatedModel+`","input":"hello"}`)
	if otherHits.Load() != 1 || targetHits.Load() != 1 || unrelated.status >= 400 {
		t.Fatalf("unrelated hits target=%d other=%d status=%d body=%s", targetHits.Load(), otherHits.Load(), unrelated.status, unrelated.body)
	}
	if !sameModelIDs(coreauth.OCGRegisteredModelIDs(targetID), want) || !sameModelIDs(coreauth.OCGRegisteredModelIDs(otherID), otherBefore) {
		t.Fatalf("registration after admission target=%v other=%v", coreauth.OCGRegisteredModelIDs(targetID), coreauth.OCGRegisteredModelIDs(otherID))
	}
}

func openFenceUpstreams(t *testing.T) (*httptest.Server, *httptest.Server, *atomic.Int32, *atomic.Int32, string) {
	t.Helper()
	var targetHits, otherHits atomic.Int32
	targetUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			targetHits.Add(1)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	t.Cleanup(targetUpstream.Close)
	otherUpstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			otherHits.Add(1)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	t.Cleanup(otherUpstream.Close)
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	t.Cleanup(policy.Close)
	return targetUpstream, otherUpstream, &targetHits, &otherHits, policy.URL
}

type commitHold struct {
	entered     chan struct{}
	finished    chan struct{}
	release     chan struct{}
	once        sync.Once
	releaseOnce sync.Once
}

func newCommitHold(t *testing.T, clear func()) *commitHold {
	t.Helper()
	hold := &commitHold{
		entered:  make(chan struct{}),
		finished: make(chan struct{}),
		release:  make(chan struct{}),
	}
	t.Cleanup(func() {
		clear()
		hold.releaseOnce.Do(func() { close(hold.release) })
	})
	return hold
}

func (h *commitHold) pause(before func(), commit func()) {
	var ran bool
	h.once.Do(func() {
		ran = true
		if before != nil {
			before()
		}
		close(h.entered)
		<-h.release
		commit()
		close(h.finished)
	})
	if !ran {
		commit()
	}
}

func (h *commitHold) waitEntered(t *testing.T) {
	t.Helper()
	select {
	case <-h.entered:
	case <-time.After(8 * time.Second):
		t.Fatal("registration did not reach the fenced mutation")
	}
}

func (h *commitHold) finish(t *testing.T) {
	t.Helper()
	h.releaseOnce.Do(func() { close(h.release) })
	select {
	case <-h.finished:
	case <-time.After(8 * time.Second):
		t.Fatal("held registration did not finish")
	}
}

func setAuthFileDisabled(t *testing.T, host *Host, file string, disabled bool) {
	t.Helper()
	rewriteAuthFile(t, host, file, func(doc map[string]any) {
		if disabled {
			doc["disabled"] = true
			return
		}
		delete(doc, "disabled")
	})
}

func writeLooseCodex(t *testing.T, host *Host, file, baseURL string, excluded []string) {
	t.Helper()
	payload := map[string]any{
		"type":         "codex",
		"access_token": "token-" + file,
		"plan_type":    "pro",
		"base_url":     baseURL,
		"expired":      time.Now().Add(48 * time.Hour).UTC().Format(time.RFC3339),
	}
	if excluded != nil {
		payload["excluded_models"] = excluded
	}
	raw, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(host.AuthDir(), file), raw, 0o600); err != nil {
		t.Fatal(err)
	}
	noteNativeGrant("codex", baseURL)
}

func rewriteAuthFile(t *testing.T, host *Host, file string, mutate func(map[string]any)) {
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
	mutate(doc)
	encoded, err := json.Marshal(doc)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, encoded, 0o600); err != nil {
		t.Fatal(err)
	}
}

func waitManagerEnabled(t *testing.T, host *Host, id string) {
	t.Helper()
	deadline := time.Now().Add(8 * time.Second)
	for time.Now().Before(deadline) {
		auth, ok := host.service.CoreAuthManager().GetByID(id)
		if ok && auth != nil && !auth.Disabled && auth.Status != coreauth.StatusDisabled {
			return
		}
		time.Sleep(15 * time.Millisecond)
	}
	t.Fatalf("auth %s did not become enabled", id)
}

func waitAuthByFile(t *testing.T, host *Host, file string) *coreauth.Auth {
	t.Helper()
	deadline := time.Now().Add(8 * time.Second)
	for time.Now().Before(deadline) {
		for _, auth := range host.service.CoreAuthManager().List() {
			if sameAuthFile(auth, file) {
				return auth
			}
		}
		time.Sleep(15 * time.Millisecond)
	}
	t.Fatalf("auth file %s was not registered", file)
	return nil
}

func waitCatalogIDs(t *testing.T, clientID string, required []string) []string {
	t.Helper()
	deadline := time.Now().Add(8 * time.Second)
	stable := 0
	var got []string
	for time.Now().Before(deadline) {
		got = coreauth.OCGRegisteredModelIDs(clientID)
		if len(got) > 1 && modelIDsContain(got, required) {
			stable++
			if stable >= 3 {
				return append([]string(nil), got...)
			}
		} else {
			stable = 0
		}
		time.Sleep(15 * time.Millisecond)
	}
	t.Fatalf("client %s models = %v want %v in a larger catalog", clientID, got, required)
	return nil
}

func sameAuthFile(auth *coreauth.Auth, file string) bool {
	if auth == nil {
		return false
	}
	return strings.EqualFold(auth.FileName, file) || strings.EqualFold(strings.TrimSpace(auth.ID), file)
}

func modelIDsContain(got, required []string) bool {
	seen := make(map[string]struct{}, len(got))
	for _, id := range got {
		seen[id] = struct{}{}
	}
	for _, id := range required {
		if _, ok := seen[id]; !ok {
			return false
		}
	}
	return true
}

func scopeOf(auth *coreauth.Auth) string {
	if auth == nil || auth.Attributes == nil {
		return ""
	}
	return auth.Attributes["ocg_model_scope"]
}
