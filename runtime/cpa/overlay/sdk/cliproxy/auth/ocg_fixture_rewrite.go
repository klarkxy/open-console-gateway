//go:build ocg_native_loopback_fixture

package auth

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
)

// nativeFixtureMatrix maps one exact canonical generation URL to the env keys
// that may replace its scheme, host, and port. X3 is shared by both xAI keys.
var nativeFixtureMatrix = map[string][]string{
	"https://chatgpt.com/backend-api/codex/responses":                                    {"codex"},
	"https://chatgpt.com/backend-api/codex/responses/compact":                            {"codex"},
	"https://api.anthropic.com/v1/messages?beta=true":                                    {"anthropic"},
	"https://api.anthropic.com/v1/messages/count_tokens?beta=true":                       {"anthropic"},
	"https://api.kimi.com/coding/v1/chat/completions":                                    {"kimi.com"},
	"https://api.kimi.com/coding/v1/responses":                                           {"kimi.com"},
	"https://api.kimi.com/coding/v1/messages?beta=true":                                  {"kimi.com"},
	"https://api.kimi.com/coding/v1/messages/count_tokens?beta=true":                     {"kimi.com"},
	"https://api.kimi.ai/coding/v1/chat/completions":                                     {"kimi.ai"},
	"https://api.kimi.ai/coding/v1/responses":                                            {"kimi.ai"},
	"https://api.kimi.ai/coding/v1/messages?beta=true":                                   {"kimi.ai"},
	"https://api.kimi.ai/coding/v1/messages/count_tokens?beta=true":                      {"kimi.ai"},
	"https://cli-chat-proxy.grok.com/v1/responses":                                       {"xai.cli"},
	"https://api.x.ai/v1/responses":                                                      {"xai.api"},
	"https://api.x.ai/v1/responses/compact":                                              {"xai.cli", "xai.api"},
	"https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent":               {"antigravity"},
	"https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse": {"antigravity"},
	"https://daily-cloudcode-pa.googleapis.com/v1internal:countTokens":                   {"antigravity"},
}

var recognizedFixtureKeys = map[string]struct{}{
	"codex":        {},
	"anthropic":    {},
	"kimi.com":     {},
	"kimi.ai":      {},
	"xai.cli":      {},
	"xai.api":      {},
	"antigravity":  {},
	"opencode":     {},
	"command-code": {},
}

// applyAdmittedNativeFixture may replace the scheme, host, and port of one
// already admitted native network request. It does not admit, consume the
// dispatch bit, or publish a result. The original method, canonical URL, and
// Host are checked before any mutation and before the env map is read.
func applyAdmittedNativeFixture(facts *generationFacts, req *http.Request) error {
	if facts == nil || req == nil || !facts.native || facts.localCount {
		return nil
	}
	canonical, _, wireHost, err := canonicalRequestTarget(req)
	if err != nil {
		return errors.New("endpoint_mismatch")
	}
	if req.URL == nil || !hostHeaderMatches(req.Host, req.URL.Scheme, wireHost) {
		return errors.New("endpoint_mismatch")
	}
	if !originalMethodAllowed(facts, req) {
		return errors.New("endpoint_mismatch")
	}
	keys := nativeFixtureMatrix[canonical]
	if len(keys) == 0 {
		return nil
	}
	mapped, err := parseFixtureEndpointMap(os.Getenv("OCG_CPA_TEST_ENDPOINTS"))
	if err != nil {
		return err
	}
	if len(mapped) == 0 {
		return nil
	}
	present := false
	var targets []string
	seen := map[string]struct{}{}
	for _, key := range keys {
		origin, ok := mapped[key]
		if !ok {
			continue
		}
		present = true
		if _, ok := seen[origin]; ok {
			continue
		}
		seen[origin] = struct{}{}
		targets = append(targets, origin)
	}
	if !present {
		return nil
	}
	matched := make([]string, 0, 1)
	for _, origin := range targets {
		if _, ok := matchDispatchLocked(facts, fixtureScratch(req, origin)); ok {
			matched = append(matched, origin)
		}
	}
	if len(matched) != 1 {
		return errors.New("endpoint_mismatch")
	}
	rewriteFixtureTarget(req, matched[0])
	return nil
}

func originalMethodAllowed(facts *generationFacts, req *http.Request) bool {
	method := strings.ToUpper(strings.TrimSpace(req.Method))
	if method == "" {
		return false
	}
	for _, pin := range facts.allowed {
		if pin.Protocol == facts.callableProtocol && strings.EqualFold(pin.HTTPMethod, method) {
			return true
		}
	}
	return false
}

func parseFixtureEndpointMap(raw string) (map[string]string, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return nil, nil
	}
	dec := json.NewDecoder(strings.NewReader(raw))
	token, err := dec.Token()
	if err != nil {
		return nil, errors.New("fixture_map")
	}
	delim, ok := token.(json.Delim)
	if !ok || delim != '{' {
		return nil, errors.New("fixture_map")
	}
	out := map[string]string{}
	for dec.More() {
		keyToken, err := dec.Token()
		if err != nil {
			return nil, errors.New("fixture_map")
		}
		key, ok := keyToken.(string)
		if !ok {
			return nil, errors.New("fixture_map")
		}
		if _, dup := out[key]; dup {
			return nil, errors.New("fixture_map")
		}
		if _, known := recognizedFixtureKeys[key]; !known {
			return nil, errors.New("fixture_map")
		}
		var value string
		if err := dec.Decode(&value); err != nil {
			return nil, errors.New("fixture_map")
		}
		origin, err := normalizeLoopbackOrigin(value)
		if err != nil {
			return nil, err
		}
		out[key] = origin
	}
	token, err = dec.Token()
	if err != nil {
		return nil, errors.New("fixture_map")
	}
	end, ok := token.(json.Delim)
	if !ok || end != '}' {
		return nil, errors.New("fixture_map")
	}
	// More reports another array or object element. A trailing } or ] is not
	// an element, so it is not EOF. A second decode must see the real end.
	var trailing json.RawMessage
	if err := dec.Decode(&trailing); !errors.Is(err, io.EOF) {
		return nil, errors.New("fixture_map")
	}
	return out, nil
}

func normalizeLoopbackOrigin(raw string) (string, error) {
	parsed, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || parsed.Opaque != "" || parsed.User != nil || parsed.Fragment != "" || parsed.RawQuery != "" || parsed.ForceQuery {
		return "", errors.New("fixture_map")
	}
	if parsed.Path != "" || parsed.RawPath != "" {
		return "", errors.New("fixture_map")
	}
	if !strings.EqualFold(parsed.Scheme, "http") {
		return "", errors.New("fixture_map")
	}
	host := strings.ToLower(parsed.Hostname())
	switch host {
	case "127.0.0.1", "localhost", "::1":
	default:
		return "", errors.New("fixture_map")
	}
	port := parsed.Port()
	number, err := strconv.Atoi(port)
	if err != nil || number < 1 || number > 65535 {
		return "", errors.New("fixture_map")
	}
	if host == "::1" {
		host = "[::1]"
	}
	return "http://" + host + ":" + strconv.Itoa(number), nil
}

func fixtureScratch(req *http.Request, origin string) *http.Request {
	copied := *req
	if req.URL != nil {
		cloned := *req.URL
		copied.URL = &cloned
	}
	rewriteFixtureTarget(&copied, origin)
	return &copied
}

func rewriteFixtureTarget(req *http.Request, origin string) {
	parsed, err := url.Parse(origin)
	if err != nil || req == nil || req.URL == nil {
		return
	}
	req.URL.Scheme = parsed.Scheme
	req.URL.Host = parsed.Host
	if strings.TrimSpace(req.Host) != "" {
		req.Host = parsed.Host
	}
}
