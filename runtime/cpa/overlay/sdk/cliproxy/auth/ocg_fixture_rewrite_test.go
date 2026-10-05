//go:build ocg_native_loopback_fixture

package auth

import (
	"net/http"
	"testing"
)

func TestOCGNativeDispatchGuardFixture(t *testing.T) {
	const codexURL = "https://chatgpt.com/backend-api/codex/responses"
	const compactURL = "https://api.x.ai/v1/responses/compact"
	const anthropicURL = "https://api.anthropic.com/v1/messages?beta=true"

	t.Run("exact codex target replaces scheme host and port", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://127.0.0.1:9","opencode":"http://127.0.0.1:8"}`)
		req, facts := dispatchFixture(t, true, GenerationKindExecute, codexURL, "http://127.0.0.1:9/backend-api/codex/responses", nil)
		if req.URL.Scheme != "http" || req.URL.Host != "127.0.0.1:9" || req.URL.Path != "/backend-api/codex/responses" || req.Host != "127.0.0.1:9" {
			t.Fatalf("rewritten url = %s host=%s", req.URL, req.Host)
		}
		if facts == nil || !facts.dispatchConsumed || facts.matched == nil || facts.matched.EndpointID != "lb-1" {
			t.Fatalf("facts = %+v", facts)
		}
	})

	t.Run("query and path stay on the anthropic count pair", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"anthropic":"http://localhost:9"}`)
		req, _ := dispatchFixture(t, true, GenerationKindExecute, anthropicURL, "http://localhost:9/v1/messages?beta=true", nil)
		if req.URL.Scheme != "http" || req.URL.Host != "localhost:9" || req.URL.Path != "/v1/messages" || req.URL.RawQuery != "beta=true" {
			t.Fatalf("url = %s raw=%s", req.URL, req.URL.RawQuery)
		}
	})

	t.Run("ipv6 origin", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://[::1]:9"}`)
		req, facts := dispatchFixture(t, true, GenerationKindExecute, codexURL, "http://[::1]:9/backend-api/codex/responses", nil)
		if req.URL.Host != "[::1]:9" || facts == nil || !facts.dispatchConsumed {
			t.Fatalf("url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("empty and partial maps leave the official url", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", "")
		req, facts := dispatchFixture(t, true, GenerationKindExecute, codexURL, codexURL, nil)
		if req.URL.Scheme != "https" || req.URL.Host != "chatgpt.com" || facts == nil || !facts.dispatchConsumed {
			t.Fatalf("empty map url=%s facts=%+v", req.URL, facts)
		}
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"command-code":"http://127.0.0.1:9","anthropic":"http://127.0.0.1:8"}`)
		req, facts = dispatchFixture(t, true, GenerationKindExecute, codexURL, codexURL, nil)
		if req.URL.Host != "chatgpt.com" || facts == nil || !facts.dispatchConsumed {
			t.Fatalf("partial map url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("shared compact uses the one matching target", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"xai.cli":"http://127.0.0.1:1","xai.api":"http://127.0.0.1:2"}`)
		req, facts := dispatchFixture(t, true, GenerationKindExecute, compactURL, "http://127.0.0.1:2/v1/responses/compact", nil)
		if req.URL.Host != "127.0.0.1:2" || req.URL.Path != "/v1/responses/compact" || facts == nil || !facts.dispatchConsumed {
			t.Fatalf("one match url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("shared compact rejects two distinct matches", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"xai.cli":"http://127.0.0.1:1","xai.api":"http://127.0.0.1:2"}`)
		pins := []EndpointPin{
			dispatchPin(t, "http://127.0.0.1:1/v1/responses/compact"),
			dispatchPin(t, "http://127.0.0.1:2/v1/responses/compact"),
		}
		pins[1].EndpointID = "lb-2"
		req, facts := dispatchFixturePins(t, true, GenerationKindExecute, compactURL, pins, nil)
		if req.URL.Host != "api.x.ai" || facts == nil || facts.dispatchConsumed {
			t.Fatalf("two matches url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("identical shared targets are one match", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"xai.cli":"http://127.0.0.1:9","xai.api":"http://127.0.0.1:09"}`)
		req, facts := dispatchFixture(t, true, GenerationKindExecute, compactURL, "http://127.0.0.1:9/v1/responses/compact", nil)
		if req.URL.Host != "127.0.0.1:9" || facts == nil || !facts.dispatchConsumed {
			t.Fatalf("same target url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("a present target that misses the pin does not fall back", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"xai.api":"http://127.0.0.1:9"}`)
		req, facts := dispatchFixture(t, true, GenerationKindExecute, compactURL, compactURL, nil)
		if req.URL.Host != "api.x.ai" || req.URL.Scheme != "https" || facts == nil || facts.dispatchConsumed || facts.matched != nil {
			t.Fatalf("fallback url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("bad host is not repaired", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://127.0.0.1:9"}`)
		req, facts := dispatchFixture(t, true, GenerationKindExecute, codexURL, "http://127.0.0.1:9/backend-api/codex/responses", func(req *http.Request) {
			req.Host = "evil.example"
		})
		if req.URL.Host != "chatgpt.com" || req.Host != "evil.example" || facts == nil || facts.dispatchConsumed {
			t.Fatalf("host repair url=%s host=%s facts=%+v", req.URL, req.Host, facts)
		}
	})

	t.Run("wrong method is not rewritten", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://127.0.0.1:9"}`)
		req, facts := dispatchFixture(t, true, GenerationKindExecute, codexURL, "http://127.0.0.1:9/backend-api/codex/responses", func(req *http.Request) {
			req.Method = http.MethodPut
		})
		if req.URL.Host != "chatgpt.com" || req.Method != http.MethodPut || facts == nil || facts.dispatchConsumed {
			t.Fatalf("method url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("api and local count are not rewritten", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://127.0.0.1:9"}`)
		req, facts := dispatchFixture(t, false, GenerationKindExecute, codexURL, codexURL, nil)
		if req.URL.Host != "chatgpt.com" || facts == nil || !facts.dispatchConsumed || facts.matched == nil {
			t.Fatalf("api url=%s facts=%+v", req.URL, facts)
		}
		req, facts = dispatchFixture(t, true, GenerationKindCount, codexURL, "http://127.0.0.1:9/backend-api/codex/responses", nil)
		if req.URL.Host != "chatgpt.com" || facts == nil || facts.dispatchConsumed || !facts.localCount {
			t.Fatalf("count url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("non matrix url ignores a malformed map", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", "{")
		const raw = "http://127.0.0.1:9/v1/responses"
		req, facts := dispatchFixture(t, true, GenerationKindExecute, raw, raw, nil)
		if req.URL.Host != "127.0.0.1:9" || req.URL.Path != "/v1/responses" || facts == nil || !facts.dispatchConsumed {
			t.Fatalf("non-matrix url=%s facts=%+v", req.URL, facts)
		}
	})

	t.Run("trailing whitespace still parses", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", "{\"codex\":\"http://127.0.0.1:9\"}\r\n ")
		req, facts := dispatchFixture(t, true, GenerationKindExecute, codexURL, "http://127.0.0.1:9/backend-api/codex/responses", nil)
		if req.URL.Scheme != "http" || req.URL.Host != "127.0.0.1:9" || req.Host != "127.0.0.1:9" || facts == nil || !facts.dispatchConsumed {
			t.Fatalf("whitespace url=%s host=%s facts=%+v", req.URL, req.Host, facts)
		}
	})

	t.Run("ipv6 explicit 80 matches the bracketed pin", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://[::1]:80"}`)
		const canonical = "http://[::1]/backend-api/codex/responses"
		pin := literalDispatchPin(canonical, "http://[::1]")
		req, facts := dispatchFixturePins(t, true, GenerationKindExecute, codexURL, []EndpointPin{pin}, nil)
		if req.URL.Scheme != "http" || req.URL.Host != "[::1]:80" || req.Host != "[::1]:80" || req.URL.Path != "/backend-api/codex/responses" || req.URL.RawQuery != "" {
			t.Fatalf("ipv6 url=%s host=%s raw=%s", req.URL, req.Host, req.URL.RawQuery)
		}
		if facts == nil || !facts.dispatchConsumed || facts.matched == nil || facts.matched.EndpointFingerprint != pin.EndpointFingerprint || facts.matched.Origin != "http://[::1]" || facts.matched.HTTPMethod != "POST" {
			t.Fatalf("ipv6 facts=%+v", facts)
		}
	})

	t.Run("ipv6 explicit 80 does not repair a bad host", func(t *testing.T) {
		t.Setenv("OCG_CPA_TEST_ENDPOINTS", `{"codex":"http://[::1]:80"}`)
		pin := literalDispatchPin("http://[::1]/backend-api/codex/responses", "http://[::1]")
		req, facts := dispatchFixturePins(t, true, GenerationKindExecute, codexURL, []EndpointPin{pin}, func(req *http.Request) {
			req.Host = "evil.example"
		})
		if req.URL.Host != "chatgpt.com" || req.URL.Scheme != "https" || req.Host != "evil.example" || facts == nil || facts.dispatchConsumed || facts.matched != nil {
			t.Fatalf("bad ipv6 host url=%s host=%s facts=%+v", req.URL, req.Host, facts)
		}
	})

	t.Run("malformed unknown and duplicate maps refuse the send", func(t *testing.T) {
		maps := []string{
			"{",
			"[]",
			`{"cpa":"http://127.0.0.1:9"}`,
			`{"codex":"http://127.0.0.1:9","codex":"http://127.0.0.1:8"}`,
			`{"codex":"https://127.0.0.1:9"}`,
			`{"codex":"http://127.0.0.1"}`,
			`{"codex":"http://127.0.0.1:9/v1"}`,
			`{"codex":"http://127.0.0.1:9?x=1"}`,
			`{"codex":"http://user@127.0.0.1:9"}`,
			`{"codex":"http://8.8.8.8:9"}`,
			`{"codex":"http://127.0.0.1:0"}`,
			`{"codex":1}`,
			`{"codex":"http://127.0.0.1:9"}}`,
			`{"codex":"http://127.0.0.1:9"}]`,
			`{"codex":"http://127.0.0.1:9"}{"opencode":"http://127.0.0.1:8"}`,
			`{"codex":"http://127.0.0.1:9"} true`,
			`{"codex":"http://127.0.0.1:9"} trailing`,
		}
		for _, rawMap := range maps {
			t.Setenv("OCG_CPA_TEST_ENDPOINTS", rawMap)
			req, facts := dispatchFixture(t, true, GenerationKindExecute, codexURL, "http://127.0.0.1:9/backend-api/codex/responses", nil)
			if req.URL.Host != "chatgpt.com" || facts == nil || facts.dispatchConsumed {
				t.Fatalf("map %s url=%s facts=%+v", rawMap, req.URL, facts)
			}
		}
	})
}

func dispatchFixture(t *testing.T, oauth bool, kind, raw, pinRaw string, mutate func(*http.Request)) (*http.Request, *generationFacts) {
	t.Helper()
	return dispatchFixturePins(t, oauth, kind, raw, []EndpointPin{dispatchPin(t, pinRaw)}, mutate)
}

func dispatchFixturePins(t *testing.T, oauth bool, kind, raw string, pins []EndpointPin, mutate func(*http.Request)) (*http.Request, *generationFacts) {
	t.Helper()
	ctx, _ := admitDispatch(t, oauth, kind, pins)
	req := dispatchRequest(t, raw)
	if mutate != nil {
		mutate(req)
	}
	_ = BeforeOCGHTTPDispatch(ctx, req)
	return req, generationFactsFrom(ctx)
}
