package auth

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// EndpointPin is one frozen native dispatch target from the admit response.
type EndpointPin struct {
	Protocol            string
	EndpointID          string
	Origin              string
	EndpointFingerprint string
	HTTPMethod          string
}

var (
	errUnknownProtocol  = errors.New("unknown_protocol")
	errProtocolMismatch = errors.New("protocol_mismatch")
)

type ocgCallableProtocolKey struct{}
type ocgSourceFormatKey struct{}

// WithOCGCallableProtocol stores the selected callable route. A protocol already
// stored on this context is kept, including across internal translation.
func WithOCGCallableProtocol(ctx context.Context, protocol string) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	if OCGCallableProtocol(ctx) != "" {
		return ctx
	}
	protocol = strings.ToLower(strings.TrimSpace(protocol))
	switch protocol {
	case "chat_completions", "responses", "messages":
		return context.WithValue(ctx, ocgCallableProtocolKey{}, protocol)
	default:
		return ctx
	}
}

// OCGCallableProtocol returns the selected callable route, or empty when unset.
func OCGCallableProtocol(ctx context.Context) string {
	if ctx == nil {
		return ""
	}
	protocol, _ := ctx.Value(ocgCallableProtocolKey{}).(string)
	return protocol
}

// NoteOCGSourceFormat stores the first source format seen for this context.
func NoteOCGSourceFormat(ctx context.Context, format string) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	if OCGSourceFormat(ctx) != "" {
		return ctx
	}
	format = strings.ToLower(strings.TrimSpace(format))
	if format == "" {
		return ctx
	}
	return context.WithValue(ctx, ocgSourceFormatKey{}, format)
}

// OCGSourceFormat returns the first stored source format.
func OCGSourceFormat(ctx context.Context) string {
	if ctx == nil {
		return ""
	}
	format, _ := ctx.Value(ocgSourceFormatKey{}).(string)
	return format
}

func callableProtocolFromSource(format string) string {
	switch strings.ToLower(strings.TrimSpace(format)) {
	case "openai", "gemini":
		return "chat_completions"
	case "openai-response", "codex":
		return "responses"
	case "claude":
		return "messages"
	default:
		return ""
	}
}

func resolveCallableProtocol(ctx context.Context) (context.Context, string, error) {
	protocol := OCGCallableProtocol(ctx)
	if protocol == "" {
		protocol = callableProtocolFromSource(OCGSourceFormat(ctx))
	}
	validated := OCGValidatedProtocol(ctx)
	if validated != "" {
		if protocol == "" {
			return ctx, "", errUnknownProtocol
		}
		if protocol != validated {
			return ctx, "", errProtocolMismatch
		}
	}
	switch protocol {
	case "chat_completions", "responses", "messages":
	default:
		return ctx, "", errUnknownProtocol
	}
	if OCGCallableProtocol(ctx) == "" {
		ctx = context.WithValue(ctx, ocgCallableProtocolKey{}, protocol)
	}
	return ctx, protocol, nil
}

func nativeFamily(auth *Auth) string {
	if auth == nil {
		return ""
	}
	provider := strings.ToLower(strings.TrimSpace(auth.Provider))
	typeName := ""
	if auth.Metadata != nil {
		typeName, _ = auth.Metadata["type"].(string)
		typeName = strings.ToLower(strings.TrimSpace(typeName))
	}
	switch {
	case provider == "codex" || typeName == "codex":
		return "codex"
	case provider == "xai" || typeName == "xai":
		return "xai"
	case provider == "claude" || typeName == "claude":
		return "claude"
	case provider == "kimi" || typeName == "kimi" || typeName == "kimi-ai" || typeName == "kimi.ai" || typeName == "kimi.com":
		return "kimi"
	case provider == "antigravity" || typeName == "antigravity":
		return "antigravity"
	default:
		return ""
	}
}

// NativeLocalCount reports a native CountTokens operation with no HTTP target.
func NativeLocalCount(auth *Auth, kind string) bool {
	if kind != GenerationKindCount {
		return false
	}
	switch nativeFamily(auth) {
	case "codex", "xai":
		return true
	default:
		return false
	}
}

func rememberAdmission(facts *generationFacts, auth *Auth, identity GenerationIdentity, protocol, kind string, pins []EndpointPin) {
	if facts == nil {
		return
	}
	facts.mu.Lock()
	defer facts.mu.Unlock()
	facts.admitted = true
	facts.native = identity.OAuth
	facts.localCount = identity.OAuth && NativeLocalCount(auth, kind)
	facts.callableProtocol = protocol
	facts.generationKind = kind
	if len(pins) > 0 {
		facts.allowed = append([]EndpointPin(nil), pins...)
	}
}

// MatchedEndpointPin returns the pin consumed by this attempt, or nil.
func MatchedEndpointPin(ctx context.Context) *EndpointPin {
	facts := generationFactsFrom(ctx)
	if facts == nil {
		return nil
	}
	facts.mu.Lock()
	defer facts.mu.Unlock()
	if facts.matched == nil {
		return nil
	}
	copied := *facts.matched
	return &copied
}

// BeforeOCGHTTPDispatch guards one generation Do. It does not choose a
// credential or an endpoint and it does not publish a result.
// A context with no installed generation boundary stays ordinary CPA.
// Token-refresh HTTP must not call this helper.
func BeforeOCGHTTPDispatch(ctx context.Context, req *http.Request) error {
	facts := generationFactsFrom(ctx)
	admitted := false
	if facts != nil {
		facts.mu.Lock()
		admitted = facts.admitted
		facts.mu.Unlock()
	}
	if facts == nil || !admitted {
		if generationBoundaryRequired(ctx) {
			return stopDispatch(ctx, errors.New("facts_unadmitted"))
		}
		return nil
	}
	if requestStopped(ctx) {
		return stopDispatch(ctx, ErrAttemptStop)
	}
	if ctx != nil {
		if err := ctx.Err(); err != nil {
			return stopDispatch(ctx, err)
		}
		if deadline, ok := ctx.Deadline(); ok && !time.Now().Before(deadline) {
			return stopDispatch(ctx, context.DeadlineExceeded)
		}
	}
	facts.mu.Lock()
	defer facts.mu.Unlock()
	if requestStopped(ctx) {
		return stopDispatch(ctx, ErrAttemptStop)
	}
	if facts.dispatchConsumed {
		return stopDispatch(ctx, errors.New("dispatch_reused"))
	}
	if len(facts.allowed) == 0 {
		if facts.native {
			return stopDispatch(ctx, errors.New("endpoint_unpinned"))
		}
		facts.dispatchConsumed = true
		suppressStdlibPhysicalReplay(req)
		return nil
	}
	if err := applyAdmittedNativeFixture(facts, req); err != nil {
		return stopDispatch(ctx, err)
	}
	matched, ok := matchDispatchLocked(facts, req)
	if !ok {
		return stopDispatch(ctx, errors.New("endpoint_mismatch"))
	}
	facts.dispatchConsumed = true
	facts.matched = &matched
	suppressStdlibPhysicalReplay(req)
	return nil
}

func generationBoundaryRequired(ctx context.Context) bool {
	if ctx == nil {
		return false
	}
	if required, _ := ctx.Value(generationBoundaryRequiredKey{}).(bool); required {
		return true
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	return gate != nil && gate.manager != nil && gate.manager.currentGenerationBoundary() != nil
}

func stopDispatch(ctx context.Context, err error) error {
	noteRotationHalted(ctx)
	if err == nil {
		err = ErrAttemptStop
	}
	return wrapRequestStopError(errors.Join(ErrAttemptStop, err))
}

func matchDispatchLocked(facts *generationFacts, req *http.Request) (EndpointPin, bool) {
	if req == nil {
		return EndpointPin{}, false
	}
	canonical, origin, wireHost, err := canonicalRequestTarget(req)
	if err != nil {
		return EndpointPin{}, false
	}
	if !hostHeaderMatches(req.Host, req.URL.Scheme, wireHost) {
		return EndpointPin{}, false
	}
	sum := sha256.Sum256([]byte(canonical))
	fingerprint := hex.EncodeToString(sum[:])
	method := strings.ToUpper(strings.TrimSpace(req.Method))
	for _, pin := range facts.allowed {
		if pin.Protocol != facts.callableProtocol || !strings.EqualFold(pin.HTTPMethod, method) {
			continue
		}
		if pin.EndpointFingerprint != fingerprint || !originEqual(pin.Origin, origin) {
			continue
		}
		return pin, true
	}
	return EndpointPin{}, false
}

// CanonicalDispatchURL returns the canonical full URL, the normalized origin,
// and the lowercase SHA-256 of the canonical URL.
func CanonicalDispatchURL(raw string) (canonical, origin, fingerprint string, err error) {
	parsed, err := url.Parse(strings.TrimSpace(raw))
	if err != nil {
		return "", "", "", err
	}
	req := &http.Request{Method: http.MethodPost, URL: parsed, Host: parsed.Host}
	canonical, origin, _, err = canonicalRequestTarget(req)
	if err != nil {
		return "", "", "", err
	}
	sum := sha256.Sum256([]byte(canonical))
	return canonical, origin, hex.EncodeToString(sum[:]), nil
}

func canonicalRequestTarget(req *http.Request) (canonical, origin, wireHost string, err error) {
	if req == nil || req.URL == nil {
		return "", "", "", errors.New("missing url")
	}
	parsed := *req.URL
	if parsed.Opaque != "" || parsed.User != nil || parsed.Fragment != "" || parsed.Host == "" {
		return "", "", "", errors.New("malformed url")
	}
	scheme := strings.ToLower(parsed.Scheme)
	if scheme != "http" && scheme != "https" {
		return "", "", "", errors.New("malformed url")
	}
	wireHost, err = normalizeHost(parsed.Hostname(), parsed.Port(), scheme)
	if err != nil {
		return "", "", "", err
	}
	path := parsed.EscapedPath()
	if path == "" {
		path = "/"
	}
	canonical = scheme + "://" + wireHost + path
	if parsed.ForceQuery || parsed.RawQuery != "" {
		canonical += "?" + parsed.RawQuery
	}
	origin = scheme + "://" + wireHost
	return canonical, origin, wireHost, nil
}

func normalizeHost(host, port, scheme string) (string, error) {
	host = strings.TrimSpace(host)
	if host == "" || strings.Contains(host, " ") {
		return "", errors.New("malformed host")
	}
	host = strings.TrimSuffix(host, ".")
	if strings.Contains(host, ":") && !strings.HasPrefix(host, "[") {
		host = "[" + host + "]"
	}
	port = strings.TrimSpace(port)
	if port != "" {
		number, err := strconv.Atoi(port)
		if err != nil || number < 1 || number > 65535 {
			return "", errors.New("malformed port")
		}
		port = strconv.Itoa(number)
	}
	if (scheme == "https" && (port == "" || port == "443")) || (scheme == "http" && (port == "" || port == "80")) {
		// Omit the default port and keep IPv6 brackets. Stripping them yields
		// the unparsable authority http://::1/path.
		return strings.ToLower(host), nil
	}
	if port == "" {
		return "", errors.New("malformed port")
	}
	return strings.ToLower(net.JoinHostPort(trimHostBrackets(host), port)), nil
}

func trimHostBrackets(host string) string {
	if strings.HasPrefix(host, "[") && strings.HasSuffix(host, "]") {
		return strings.TrimSuffix(strings.TrimPrefix(host, "["), "]")
	}
	return host
}

func hostHeaderMatches(headerHost, scheme, wireHost string) bool {
	headerHost = strings.TrimSpace(headerHost)
	if headerHost == "" {
		return true
	}
	host, port, err := net.SplitHostPort(headerHost)
	if err != nil {
		host = headerHost
		port = ""
	}
	normalized, err := normalizeHost(host, port, scheme)
	if err != nil {
		return false
	}
	return strings.EqualFold(normalized, wireHost)
}

func originEqual(pinOrigin, requestOrigin string) bool {
	left, errLeft := normalizeOrigin(pinOrigin)
	right, errRight := normalizeOrigin(requestOrigin)
	return errLeft == nil && errRight == nil && left == right
}

func normalizeOrigin(raw string) (string, error) {
	parsed, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || parsed.Scheme == "" || parsed.Host == "" || parsed.User != nil || parsed.Fragment != "" || parsed.RawQuery != "" || parsed.Opaque != "" {
		return "", errors.New("origin")
	}
	if parsed.Path != "" && parsed.Path != "/" {
		return "", errors.New("origin")
	}
	scheme := strings.ToLower(parsed.Scheme)
	host, err := normalizeHost(parsed.Hostname(), parsed.Port(), scheme)
	if err != nil {
		return "", err
	}
	return scheme + "://" + host, nil
}
