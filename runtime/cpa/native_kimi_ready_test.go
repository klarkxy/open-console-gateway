package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestReadyKimiFactsUseExplicitDomainNotFilename(t *testing.T) {
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"models":{}}`))
	}))
	defer upstream.Close()
	policy, _ := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{
				file: "kimi-ai.json", id: "oauth-file", version: "4", priority: "1", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "token-file-name", baseURL: upstream.URL, kind: "kimi",
			},
			{
				file: "plain.json", id: "oauth-plain", version: "4", priority: "2", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "token-plain-domain", baseURL: upstream.URL, kind: "kimi", domain: "kimi.ai",
			},
		},
	})
	body := waitReady(t, directClient(), host, "oauth-plain")
	if strings.Contains(body, "token-file-name") || strings.Contains(body, "token-plain-domain") || strings.Contains(body, "access_token") {
		t.Fatal("ready exposed token material")
	}
	var ready readyDocument
	if err := json.Unmarshal([]byte(body), &ready); err != nil {
		t.Fatal(err)
	}
	var sawFile, sawPlain bool
	for _, ref := range ready.AuthRefs {
		switch ref.CredentialID {
		case "oauth-file":
			sawFile = true
			if ref.RawProviderLabel != "kimi" || ref.EffectiveSubtype != "kimi.com" || ref.EffectiveGenerationBase != upstream.URL || ref.EffectiveAuthKind != "oauth" {
				t.Fatalf("filename ref = %+v", ref)
			}
		case "oauth-plain":
			sawPlain = true
			if ref.RawProviderLabel != "kimi" || ref.EffectiveSubtype != "kimi.ai" || ref.EffectiveGenerationBase != upstream.URL || ref.EffectiveAuthKind != "oauth" {
				t.Fatalf("explicit domain ref = %+v", ref)
			}
		}
	}
	if !sawFile || !sawPlain {
		t.Fatalf("ready refs = %+v", ready.AuthRefs)
	}
}
