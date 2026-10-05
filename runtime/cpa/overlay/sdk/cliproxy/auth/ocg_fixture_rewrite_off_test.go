//go:build !ocg_native_loopback_fixture

package auth

import (
	"os"
	"strings"
	"testing"
)

func TestOCGNativeDispatchGuardFixtureIgnored(t *testing.T) {
	source, err := os.ReadFile("ocg_fixture_rewrite_off.go")
	if err != nil {
		t.Fatal(err)
	}
	text := string(source)
	if strings.Contains(text, "Getenv") || strings.Contains(text, "LookupEnv") || strings.Contains(text, "OCG_CPA_TEST_ENDPOINTS") {
		t.Fatal("untagged fixture hook reads the environment")
	}
	t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://127.0.0.1:9","cpa":"http://127.0.0.1:8"}`)
	const raw = "https://chatgpt.com/backend-api/codex/responses"
	ctx, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{dispatchPin(t, raw)})
	req := dispatchRequest(t, raw)
	if err := BeforeOCGHTTPDispatch(ctx, req); err != nil {
		t.Fatal(err)
	}
	if req.URL.Scheme != "https" || req.URL.Host != "chatgpt.com" || req.URL.Path != "/backend-api/codex/responses" {
		t.Fatalf("untagged rewrite url=%s", req.URL)
	}
	facts := generationFactsFrom(ctx)
	if facts == nil || !facts.dispatchConsumed || facts.matched == nil {
		t.Fatalf("facts = %+v", facts)
	}
}
