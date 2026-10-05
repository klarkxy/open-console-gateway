package main

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

func TestNativeOrdinaryRefreshKeepsRegistrationEpoch(t *testing.T) {
	var sends atomic.Int32
	var auths []string
	var mu sync.Mutex
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := sends.Add(1)
		mu.Lock()
		auths = append(auths, r.Header.Get("Authorization"))
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
	defer upstream.Close()
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"access-new","refresh_token":"refresh-1","expires_in":3600}`))
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
		bindings: []nativeBinding{{
			file: "codex-a.json", id: "oauth-a", version: "4", priority: "1", provider: "canonical-oauth",
			models: []string{nativeModel}, access: "access-old", refresh: "refresh-1", baseURL: upstream.URL, tokenURL: tokenSrv.URL,
			expired: time.Now().Add(48 * time.Hour).UTC().Format(time.RFC3339),
		}},
	})
	_ = waitReady(t, directClient(), host, "oauth-a")
	before := settleNativeAuth(t, host, "oauth-a")
	quiet := before.Clone()
	if quiet.Attributes == nil {
		quiet.Attributes = map[string]string{}
	}
	quiet.Attributes["runtime_only"] = "true"
	quieted, err := host.service.CoreAuthManager().Update(context.Background(), quiet)
	if err != nil {
		t.Fatal(err)
	}
	epoch := quieted.RegistrationEpoch
	material := metadataText(quieted, "ocg_material_revision")
	if epoch != before.RegistrationEpoch || material == "" || tokenOf(quieted) != "access-old" {
		t.Fatalf("before refresh epoch=%d/%d material=%q token=%q", epoch, before.RegistrationEpoch, material, tokenOf(quieted))
	}
	got := responses(t, host, "41", "7", nativeModel, "")
	if sends.Load() != 2 {
		t.Fatalf("refresh resend sends=%d status=%d body=%s", sends.Load(), got.status, got.body)
	}
	mu.Lock()
	if len(auths) != 2 || auths[0] != "Bearer access-old" || auths[1] != "Bearer access-new" {
		mu.Unlock()
		t.Fatalf("authorization = %v", auths)
	}
	mu.Unlock()
	after := nativeAuth(t, host, "oauth-a")
	if after.RegistrationEpoch != epoch || metadataText(after, "ocg_credential_id") != "oauth-a" || metadataText(after, "ocg_credential_version") != "4" || after.Attributes["priority"] != "1" {
		t.Fatalf("refresh moved registration cid=%s ver=%s rank=%s epoch=%d want=%d", metadataText(after, "ocg_credential_id"), metadataText(after, "ocg_credential_version"), after.Attributes["priority"], after.RegistrationEpoch, epoch)
	}
	if tokenOf(after) != "access-new" || metadataText(after, "ocg_material_revision") == material {
		t.Fatalf("refresh material token=%q revision=%q previous=%q", tokenOf(after), metadataText(after, "ocg_material_revision"), material)
	}
	wantEpoch := strconv.FormatUint(epoch, 10)
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 2 || results.items[0].RegistrationEpoch != wantEpoch || results.items[1].RegistrationEpoch != wantEpoch {
		t.Fatalf("refresh epochs = %+v", results.items)
	}
}

func TestNativeMaterialReplacementAdvancesEpoch(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex-a.json", "oauth-a", upstream.URL, nativeModel)},
	})
	_ = waitReady(t, directClient(), host, "oauth-a")
	seed := settleNativeAuth(t, host, "oauth-a")
	manager := coreauth.NewManager(nil, nil, nil)
	seeded, err := manager.Register(context.Background(), seed.Clone())
	if err != nil {
		t.Fatal(err)
	}
	if seeded == nil || tokenOf(seeded) != "token-oauth-a" || seeded.RegistrationEpoch <= seed.RegistrationEpoch {
		t.Fatalf("seed register epoch=%d token=%q", epochOf(seeded), tokenOf(seeded))
	}
	base := seeded.Clone()
	epoch := seeded.RegistrationEpoch
	cid := metadataText(seeded, "ocg_credential_id")
	version := metadataText(seeded, "ocg_credential_version")
	rank := seeded.Attributes["priority"]
	material := metadataText(seeded, "ocg_material_revision")
	if cid != "oauth-a" || version != "4" || rank != "1" || material == "" {
		t.Fatalf("initial cid=%s ver=%s rank=%s material=%q epoch=%d", cid, version, rank, material, epoch)
	}

	refreshed := base.Clone()
	refreshed.Metadata["access_token"] = "access-refresh-only"
	updated, err := manager.UpdateRefreshedAuth(context.Background(), base, refreshed)
	if err != nil {
		t.Fatal(err)
	}
	if updated.RegistrationEpoch != epoch || tokenOf(updated) != "access-refresh-only" {
		t.Fatalf("ordinary refresh epoch=%d want=%d token=%q", updated.RegistrationEpoch, epoch, tokenOf(updated))
	}
	stored := managerAuth(t, manager, seeded.ID)
	if stored.RegistrationEpoch != epoch || tokenOf(stored) != "access-refresh-only" || metadataText(stored, "ocg_material_revision") == material {
		t.Fatalf("ordinary refresh stored epoch=%d token=%q material=%q", stored.RegistrationEpoch, tokenOf(stored), metadataText(stored, "ocg_material_revision"))
	}
	assertProductIdentity(t, stored, cid, version, rank)

	replaced := stored.Clone()
	replaced.Metadata["access_token"] = "access-replaced"
	replaceEpoch := stored.RegistrationEpoch
	afterReplace, err := manager.Update(context.Background(), replaced)
	if err != nil {
		t.Fatal(err)
	}
	if afterReplace.RegistrationEpoch != replaceEpoch+1 || tokenOf(afterReplace) != "access-replaced" {
		t.Fatalf("replace epoch=%d want=%d token=%q", afterReplace.RegistrationEpoch, replaceEpoch+1, tokenOf(afterReplace))
	}
	stored = managerAuth(t, manager, seeded.ID)
	if stored.RegistrationEpoch != afterReplace.RegistrationEpoch || tokenOf(stored) != "access-replaced" {
		t.Fatalf("replace stored epoch=%d token=%q", stored.RegistrationEpoch, tokenOf(stored))
	}
	assertProductIdentity(t, stored, cid, version, rank)

	meta := stored.Clone()
	meta.Attributes["priority"] = "2"
	meta.Attributes["ocg_model_scope"] = nativeModel + "," + otherModel
	meta.Label = "host-stamp"
	metaEpoch := stored.RegistrationEpoch
	afterMeta, err := manager.Update(context.Background(), meta)
	if err != nil {
		t.Fatal(err)
	}
	if afterMeta.RegistrationEpoch != metaEpoch || tokenOf(afterMeta) != "access-replaced" || afterMeta.Attributes["priority"] != "2" || afterMeta.Label != "host-stamp" {
		t.Fatalf("metadata stamp epoch=%d want=%d rank=%s label=%s token=%q", afterMeta.RegistrationEpoch, metaEpoch, afterMeta.Attributes["priority"], afterMeta.Label, tokenOf(afterMeta))
	}
	stored = managerAuth(t, manager, seeded.ID)
	if stored.RegistrationEpoch != metaEpoch || tokenOf(stored) != "access-replaced" || stored.Attributes["priority"] != "2" {
		t.Fatalf("metadata stamp stored epoch=%d rank=%s token=%q", stored.RegistrationEpoch, stored.Attributes["priority"], tokenOf(stored))
	}
	if _, ok := host.stampOAuth(manager, stored, host.document.OAuthBindings, nil); !ok {
		t.Fatal("host stamp missed the binding")
	}
	stored = managerAuth(t, manager, seeded.ID)
	if stored.RegistrationEpoch != metaEpoch {
		t.Fatalf("host stamp epoch=%d want=%d", stored.RegistrationEpoch, metaEpoch)
	}
	assertProductIdentity(t, stored, cid, version, rank)

	registeredIn := stored.Clone()
	registeredIn.Metadata["access_token"] = "access-registered"
	registerEpoch := stored.RegistrationEpoch
	registered, err := manager.Register(context.Background(), registeredIn)
	if err != nil {
		t.Fatal(err)
	}
	if registered.RegistrationEpoch != registerEpoch+1 || tokenOf(registered) != "access-registered" {
		t.Fatalf("register epoch=%d want=%d token=%q", registered.RegistrationEpoch, registerEpoch+1, tokenOf(registered))
	}
	stored = managerAuth(t, manager, seeded.ID)
	if stored.RegistrationEpoch != registered.RegistrationEpoch || tokenOf(stored) != "access-registered" {
		t.Fatalf("register stored epoch=%d token=%q", stored.RegistrationEpoch, tokenOf(stored))
	}
	assertProductIdentity(t, stored, cid, version, rank)

	stale := base.Clone()
	stale.Metadata["access_token"] = "access-stale-refresh"
	if _, err := manager.UpdateRefreshedAuth(context.Background(), base, stale); err == nil || !strings.Contains(err.Error(), "stale registration epoch") {
		t.Fatalf("stale refresh = %v", err)
	}
	kept := managerAuth(t, manager, seeded.ID)
	if kept.RegistrationEpoch != stored.RegistrationEpoch || tokenOf(kept) != "access-registered" || kept.Success != stored.Success || kept.Failed != stored.Failed || kept.Generation != stored.Generation {
		t.Fatalf("stale refresh overwrote epoch=%d token=%q success=%d failed=%d generation=%d", kept.RegistrationEpoch, tokenOf(kept), kept.Success, kept.Failed, kept.Generation)
	}

	beforeResult := kept.Clone()
	manager.MarkResult(context.Background(), coreauth.Result{
		AuthID:            seeded.ID,
		Success:           true,
		Model:             nativeModel,
		Provider:          kept.Provider,
		RegistrationEpoch: epoch,
		CredentialID:      cid,
		CredentialVersion: version,
		OAuthIdentity:     true,
		IdentityBound:     true,
	})
	afterOld := managerAuth(t, manager, seeded.ID)
	if afterOld.RegistrationEpoch != beforeResult.RegistrationEpoch || tokenOf(afterOld) != tokenOf(beforeResult) || afterOld.Success != beforeResult.Success || afterOld.Failed != beforeResult.Failed || afterOld.Generation != beforeResult.Generation || metadataText(afterOld, "ocg_material_revision") != metadataText(beforeResult, "ocg_material_revision") {
		t.Fatalf("old result overwrote epoch=%d/%d success=%d/%d failed=%d/%d generation=%d/%d token=%q/%q", afterOld.RegistrationEpoch, beforeResult.RegistrationEpoch, afterOld.Success, beforeResult.Success, afterOld.Failed, beforeResult.Failed, afterOld.Generation, beforeResult.Generation, tokenOf(afterOld), tokenOf(beforeResult))
	}
	manager.MarkResult(context.Background(), coreauth.Result{
		AuthID:            seeded.ID,
		Success:           true,
		Model:             nativeModel,
		Provider:          kept.Provider,
		RegistrationEpoch: afterOld.RegistrationEpoch,
		CredentialID:      cid,
		CredentialVersion: version,
		OAuthIdentity:     true,
		IdentityBound:     true,
	})
	afterCurrent := managerAuth(t, manager, seeded.ID)
	if afterCurrent.Success != beforeResult.Success+1 || afterCurrent.RegistrationEpoch != beforeResult.RegistrationEpoch || tokenOf(afterCurrent) != tokenOf(beforeResult) {
		t.Fatalf("current result success=%d want=%d epoch=%d token=%q", afterCurrent.Success, beforeResult.Success+1, afterCurrent.RegistrationEpoch, tokenOf(afterCurrent))
	}
}

func TestNativeCapturedResultDoesNotOverwriteReplacement(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		once.Do(func() { close(started) })
		<-release
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer upstream.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{binding("codex-a.json", "oauth-a", upstream.URL, nativeModel)},
	})
	_ = waitReady(t, directClient(), host, "oauth-a")
	live := settleNativeAuth(t, host, "oauth-a")
	epoch := live.RegistrationEpoch
	done := make(chan chatResult, 1)
	go func() {
		done <- responses(t, host, "41", "7", nativeModel, "")
	}()
	select {
	case <-started:
	case <-time.After(8 * time.Second):
		t.Fatal("in-flight send did not start")
	}
	replaced := live.Clone()
	if replaced.Attributes == nil {
		replaced.Attributes = map[string]string{}
	}
	replaced.Attributes["runtime_only"] = "true"
	replaced.Metadata["access_token"] = "access-replaced"
	updated, err := host.service.CoreAuthManager().Update(context.Background(), replaced)
	if err != nil {
		t.Fatal(err)
	}
	if updated.RegistrationEpoch != epoch+1 || tokenOf(updated) != "access-replaced" {
		t.Fatalf("replacement epoch=%d token=%q", updated.RegistrationEpoch, tokenOf(updated))
	}
	snapshot := nativeAuth(t, host, "oauth-a").Clone()
	close(release)
	select {
	case first := <-done:
		if first.status == http.StatusServiceUnavailable {
			t.Fatalf("in-flight request became unavailable: %s", first.body)
		}
	case <-time.After(15 * time.Second):
		t.Fatal("in-flight request did not finish")
	}
	kept := nativeAuth(t, host, "oauth-a")
	if kept.RegistrationEpoch != snapshot.RegistrationEpoch || tokenOf(kept) != "access-replaced" || kept.Success != snapshot.Success || kept.Failed != snapshot.Failed || metadataText(kept, "ocg_credential_id") != "oauth-a" || metadataText(kept, "ocg_credential_version") != "4" || metadataText(kept, "ocg_material_revision") != metadataText(snapshot, "ocg_material_revision") {
		t.Fatalf("captured result overwrote replacement epoch=%d token=%q success=%d failed=%d material=%q", kept.RegistrationEpoch, tokenOf(kept), kept.Success, kept.Failed, metadataText(kept, "ocg_material_revision"))
	}
	results.mu.Lock()
	if len(results.items) == 0 || results.items[0].RegistrationEpoch != strconv.FormatUint(epoch, 10) {
		epochSeen := ""
		if len(results.items) > 0 {
			epochSeen = results.items[0].RegistrationEpoch
		}
		results.mu.Unlock()
		t.Fatalf("captured result epoch = %s, admitted %d", epochSeen, epoch)
	}
	results.mu.Unlock()
	second := responses(t, host, "41", "7", nativeModel, "")
	if second.status == http.StatusServiceUnavailable {
		t.Fatalf("post-replacement request was refused: %s", second.body)
	}
	results.mu.Lock()
	last := results.items[len(results.items)-1]
	results.mu.Unlock()
	if last.RegistrationEpoch != strconv.FormatUint(kept.RegistrationEpoch, 10) || last.RegistrationEpoch == strconv.FormatUint(epoch, 10) {
		t.Fatalf("next admission epoch = %s live=%d old=%d", last.RegistrationEpoch, kept.RegistrationEpoch, epoch)
	}
}

func TestNativeReplacementResendObeysEpoch(t *testing.T) {
	var sends atomic.Int32
	var auths []string
	var mu sync.Mutex
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		n := sends.Add(1)
		mu.Lock()
		auths = append(auths, r.Header.Get("Authorization"))
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
	defer upstream.Close()
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"access_token":"access-refreshed","refresh_token":"refresh-1","expires_in":3600}`))
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
		bindings: []nativeBinding{{
			file: "codex-a.json", id: "oauth-a", version: "4", priority: "1", provider: "canonical-oauth",
			models: []string{nativeModel}, access: "access-old", refresh: "refresh-1", baseURL: upstream.URL, tokenURL: tokenSrv.URL,
			expired: time.Now().Add(48 * time.Hour).UTC().Format(time.RFC3339),
		}},
	})
	_ = waitReady(t, directClient(), host, "oauth-a")
	manager := host.service.CoreAuthManager()
	live := settleNativeAuth(t, host, "oauth-a")
	base := live.Clone()
	epoch := live.RegistrationEpoch
	replaced := live.Clone()
	if replaced.Attributes == nil {
		replaced.Attributes = map[string]string{}
	}
	replaced.Attributes["runtime_only"] = "true"
	replaced.Metadata["access_token"] = "access-replaced"
	updated, err := manager.Update(context.Background(), replaced)
	if err != nil {
		t.Fatal(err)
	}
	if updated.RegistrationEpoch != epoch+1 || tokenOf(nativeAuth(t, host, "oauth-a")) != "access-replaced" {
		t.Fatalf("replacement epoch=%d token=%q", updated.RegistrationEpoch, tokenOf(nativeAuth(t, host, "oauth-a")))
	}
	stale := base.Clone()
	stale.Metadata["access_token"] = "access-stale-refresh"
	if _, err := manager.UpdateRefreshedAuth(context.Background(), base, stale); err == nil || !strings.Contains(err.Error(), "stale registration epoch") {
		t.Fatalf("stale refresh = %v", err)
	}
	current := nativeAuth(t, host, "oauth-a")
	if current.RegistrationEpoch != updated.RegistrationEpoch || tokenOf(current) != "access-replaced" || current.Attributes["priority"] != "1" {
		t.Fatalf("stale refresh changed epoch=%d token=%q rank=%s", current.RegistrationEpoch, tokenOf(current), current.Attributes["priority"])
	}
	material := metadataText(current, "ocg_material_revision")
	got := responses(t, host, "41", "7", nativeModel, "")
	if sends.Load() != 2 {
		t.Fatalf("fenced resend sends=%d status=%d body=%s", sends.Load(), got.status, got.body)
	}
	mu.Lock()
	if len(auths) != 2 || auths[0] != "Bearer access-replaced" || auths[1] != "Bearer access-refreshed" {
		mu.Unlock()
		t.Fatalf("authorization = %v", auths)
	}
	mu.Unlock()
	after := nativeAuth(t, host, "oauth-a")
	if after.RegistrationEpoch != updated.RegistrationEpoch || metadataText(after, "ocg_credential_id") != "oauth-a" || metadataText(after, "ocg_credential_version") != "4" || after.Attributes["priority"] != "1" {
		t.Fatalf("resend moved epoch=%d want=%d cid=%s ver=%s rank=%s", after.RegistrationEpoch, updated.RegistrationEpoch, metadataText(after, "ocg_credential_id"), metadataText(after, "ocg_credential_version"), after.Attributes["priority"])
	}
	if tokenOf(after) != "access-refreshed" || metadataText(after, "ocg_material_revision") == material {
		t.Fatalf("resend material token=%q revision=%q previous=%q", tokenOf(after), metadataText(after, "ocg_material_revision"), material)
	}
	wantEpoch := strconv.FormatUint(updated.RegistrationEpoch, 10)
	oldEpoch := strconv.FormatUint(epoch, 10)
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.items) < 2 || results.items[0].RegistrationEpoch != wantEpoch || results.items[1].RegistrationEpoch != wantEpoch || results.items[0].RegistrationEpoch == oldEpoch {
		t.Fatalf("resend epochs = %+v want %s", results.items, wantEpoch)
	}
}

func TestNativeReadyRefsExposeDisabledStatusAndEmptyScope(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Errorf("ready observation contacted upstream")
		w.WriteHeader(http.StatusOK)
	}))
	defer upstream.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	disabled := binding("codex-off.json", "oauth-off", upstream.URL, nativeModel)
	disabled.disabled = true
	empty := binding("codex-empty.json", "oauth-empty", upstream.URL, "")
	empty.models = []string{}
	empty.priority = "2"
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{disabled, empty},
	})
	body := waitReady(t, directClient(), host, "oauth-empty")
	if strings.Contains(body, "token-oauth-off") || strings.Contains(body, "token-oauth-empty") || strings.Contains(body, "refresh-oauth-off") || strings.Contains(body, "refresh-oauth-empty") || strings.Contains(body, "access_token") || strings.Contains(body, "refresh_token") {
		t.Fatal("ready exposed token material or raw metadata")
	}
	var ready readyDocument
	if err := json.Unmarshal([]byte(body), &ready); err != nil {
		t.Fatal(err)
	}
	if len(ready.Artifact.Capabilities) != 16 || !containsString(ready.Artifact.Capabilities, "native-refresh-registration-fence-v1") || !containsString(ready.Artifact.Capabilities, "native-final-endpoint-pin-v1") {
		t.Fatalf("capabilities = %v", ready.Artifact.Capabilities)
	}
	for _, capability := range ready.Artifact.Capabilities {
		if strings.Contains(capability, "websocket") {
			t.Fatalf("websocket is claimed: %s", capability)
		}
	}
	var sawDisabled, sawEmpty bool
	for _, ref := range ready.AuthRefs {
		switch ref.CredentialID {
		case "oauth-off":
			sawDisabled = true
			auth := nativeAuth(t, host, "oauth-off")
			raw, err := json.Marshal(ref)
			if err != nil {
				t.Fatal(err)
			}
			if !ref.Disabled || ref.Status != string(coreauth.StatusDisabled) || ref.Status != string(auth.Status) || len(ref.Models) != 1 || ref.Models[0] != nativeModel || !strings.Contains(string(raw), `"disabled":true`) || !strings.Contains(string(raw), `"status":"disabled"`) {
				t.Fatalf("disabled ref = %s authDisabled=%v authStatus=%q", raw, auth.Disabled, auth.Status)
			}
			if ref.RawProviderLabel != "codex" || ref.EffectiveSubtype != "codex" || ref.EffectiveMode != "" || ref.EffectiveAuthKind != "oauth" || ref.EffectiveGenerationBase != codexObservedBase(auth) || ref.Provider == ref.RawProviderLabel && ref.Provider == "cpa" {
				t.Fatalf("disabled native facts = %s", raw)
			}
		case "oauth-empty":
			sawEmpty = true
			auth := nativeAuth(t, host, "oauth-empty")
			raw, err := json.Marshal(ref)
			if err != nil {
				t.Fatal(err)
			}
			if ref.Disabled || auth.Disabled || len(ref.Models) != 0 || ref.Status != string(auth.Status) || !strings.Contains(string(raw), `"disabled":false`) || !strings.Contains(string(raw), `"models":[]`) || !strings.Contains(string(raw), `"status":`) {
				t.Fatalf("empty scope ref = %s authDisabled=%v authStatus=%q", raw, auth.Disabled, auth.Status)
			}
			if ref.RawProviderLabel != "codex" || ref.EffectiveSubtype != "codex" || ref.EffectiveMode != "" || ref.EffectiveAuthKind != "oauth" || ref.EffectiveGenerationBase != codexObservedBase(auth) || strings.Contains(string(raw), "token-oauth") {
				t.Fatalf("empty native facts = %s", raw)
			}
		}
	}
	if !sawDisabled || !sawEmpty {
		t.Fatalf("ready refs = %+v", ready.AuthRefs)
	}
}

func codexObservedBase(auth *coreauth.Auth) string {
	base := ""
	if auth != nil && auth.Attributes != nil {
		base = strings.TrimSpace(auth.Attributes["base_url"])
	}
	if base == "" {
		return codexDefaultBase
	}
	return strings.TrimRight(base, "/")
}

func settleNativeAuth(t *testing.T, host *Host, credentialID string) *coreauth.Auth {
	t.Helper()
	deadline := time.Now().Add(3 * time.Second)
	var last *coreauth.Auth
	stable := 0
	for time.Now().Before(deadline) {
		current := nativeAuth(t, host, credentialID).Clone()
		if last != nil && current.RegistrationEpoch == last.RegistrationEpoch && current.Generation == last.Generation && tokenOf(current) == tokenOf(last) {
			stable++
			if stable >= 8 {
				return current
			}
		} else {
			stable = 0
		}
		last = current
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatalf("auth %s was still changing after the initial load", credentialID)
	return nil
}

func managerAuth(t *testing.T, manager *coreauth.Manager, id string) *coreauth.Auth {
	t.Helper()
	auth, ok := manager.GetByID(id)
	if !ok || auth == nil {
		t.Fatalf("manager auth %s missing", id)
	}
	return auth
}

func epochOf(auth *coreauth.Auth) uint64 {
	if auth == nil {
		return 0
	}
	return auth.RegistrationEpoch
}

func nativeAuth(t *testing.T, host *Host, credentialID string) *coreauth.Auth {
	t.Helper()
	for _, auth := range host.service.CoreAuthManager().List() {
		if auth != nil && metadataText(auth, "ocg_credential_id") == credentialID {
			return auth
		}
	}
	t.Fatalf("native auth %s was not registered", credentialID)
	return nil
}

func tokenOf(auth *coreauth.Auth) string {
	if auth == nil || auth.Metadata == nil {
		return ""
	}
	value, _ := auth.Metadata["access_token"].(string)
	return value
}

func assertProductIdentity(t *testing.T, auth *coreauth.Auth, cid, version, rank string) {
	t.Helper()
	if metadataText(auth, "ocg_credential_id") != cid || metadataText(auth, "ocg_credential_version") != version || auth.Attributes["priority"] != rank {
		t.Fatalf("product identity cid=%s ver=%s rank=%s", metadataText(auth, "ocg_credential_id"), metadataText(auth, "ocg_credential_version"), auth.Attributes["priority"])
	}
}

func containsString(values []string, want string) bool {
	for _, value := range values {
		if value == want {
			return true
		}
	}
	return false
}
