package executor

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"strconv"
	"strings"

	cliproxyauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/executor"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v8/sdk/translator"
	"github.com/tidwall/gjson"
	"github.com/tidwall/sjson"
)

const (
	ocgRoutesAttribute     = "ocg_protocol_routes"
	ocgEndpointAttribute   = "ocg_endpoint"
	ocgProtocolAttribute   = "ocg_protocol"
	ocgAuthSchemeAttribute = "ocg_auth_scheme"
	ocgIdentityAttribute   = "ocg_identity"
	ocgWireAttribute       = "ocg_wire"
	ocgMaxOllamaTokens     = 65535
	ocgZenClient           = "cli"
	ocgZenUserAgent        = "opencode"
)

type ocgProtocolRoute struct {
	Protocol       string `json:"protocol"`
	Endpoint       string `json:"endpoint"`
	AuthScheme     string `json:"authScheme"`
	UpstreamModel  string `json:"upstreamModel"`
	PublicAlias    string `json:"publicAlias"`
	Identity       string `json:"identity"`
	Wire           string `json:"wire"`
	ValidationOnly bool   `json:"validationOnly"`
}

type ocgActiveRouteKey struct{}

func ocgProtocolRoutes(auth *cliproxyauth.Auth, models ...string) ([]ocgProtocolRoute, error) {
	if auth == nil || auth.Attributes == nil {
		return nil, nil
	}
	raw := strings.TrimSpace(auth.Attributes[ocgRoutesAttribute])
	if raw == "" {
		return nil, nil
	}
	var routes []ocgProtocolRoute
	if err := json.Unmarshal([]byte(raw), &routes); err != nil {
		return nil, err
	}
	return ocgMatchRoutes(routes, models...), nil
}

func ocgRequestedModel(req cliproxyexecutor.Request, opts cliproxyexecutor.Options) []string {
	models := []string{req.Model}
	if opts.Metadata != nil {
		if value, _ := opts.Metadata[cliproxyexecutor.RequestedModelMetadataKey].(string); strings.TrimSpace(value) != "" {
			models = append(models, value)
		}
	}
	return models
}

func ocgMatchRoutes(routes []ocgProtocolRoute, models ...string) []ocgProtocolRoute {
	if len(routes) == 0 {
		return nil
	}
	matched := make([]ocgProtocolRoute, 0, len(routes))
	for _, route := range routes {
		alias := strings.TrimSpace(route.PublicAlias)
		upstream := strings.TrimSpace(route.UpstreamModel)
		if alias == "" && upstream == "" {
			matched = append(matched, route)
			continue
		}
		for _, model := range models {
			model = strings.TrimSpace(model)
			if model == "" {
				continue
			}
			if strings.EqualFold(alias, model) || strings.EqualFold(upstream, model) {
				matched = append(matched, route)
				break
			}
		}
	}
	if len(matched) == 0 {
		return nil
	}
	return matched
}

func (e *OpenAICompatExecutor) maybeExecuteOCGRoutes(ctx context.Context, auth *cliproxyauth.Auth, req cliproxyexecutor.Request, opts cliproxyexecutor.Options) (bool, cliproxyexecutor.Response, error) {
	handled, routes, errRoutes := ocgSelectableRoutes(ctx, auth, req, opts)
	if errRoutes != nil || !handled {
		return handled, cliproxyexecutor.Response{}, errRoutes
	}
	var lastResp cliproxyexecutor.Response
	var lastErr error
	activeCtx := ctx
	for i, route := range routes {
		if !ocgRouteAllowed(ctx, route) {
			return true, lastResp, errors.New("validated protocol is not routed")
		}
		routedAuth := ocgAuthForRoute(auth, route)
		routedReq := req
		if strings.TrimSpace(route.UpstreamModel) != "" {
			routedReq.Model = route.UpstreamModel
		}
		if i == 0 {
			activeCtx = context.WithValue(ctx, ocgActiveRouteKey{}, route.Protocol)
		} else {
			if pubErr := cliproxyauth.PublishInternalGeneration(activeCtx, routedAuth, routedReq.Model, lastErr); pubErr != nil {
				if cliproxyauth.RequestStopped(pubErr) || !errors.Is(pubErr, cliproxyauth.ErrAttemptSkip) {
					return true, lastResp, pubErr
				}
			}
			next, errGate := cliproxyauth.AdmitInternalGeneration(context.WithValue(ctx, ocgActiveRouteKey{}, route.Protocol), routedAuth, routedReq.Model)
			if errGate != nil {
				return true, lastResp, errGate
			}
			activeCtx = next
		}
		resp, errExec := e.Execute(activeCtx, routedAuth, routedReq, opts)
		if errExec == nil {
			if i > 0 {
				if pubErr := cliproxyauth.PublishInternalGeneration(activeCtx, routedAuth, routedReq.Model, nil); pubErr != nil {
					return true, resp, pubErr
				}
			}
			return true, resp, nil
		}
		if cliproxyauth.RequestStopped(errExec) {
			return true, resp, errExec
		}
		lastResp = resp
		lastErr = errExec
	}
	return true, lastResp, lastErr
}

func (e *OpenAICompatExecutor) maybeExecuteOCGRoutesStream(ctx context.Context, auth *cliproxyauth.Auth, req cliproxyexecutor.Request, opts cliproxyexecutor.Options) (bool, *cliproxyexecutor.StreamResult, error) {
	handled, routes, errRoutes := ocgSelectableRoutes(ctx, auth, req, opts)
	if errRoutes != nil || !handled {
		return handled, nil, errRoutes
	}
	var last *cliproxyexecutor.StreamResult
	var lastErr error
	activeCtx := ctx
	for i, route := range routes {
		if !ocgRouteAllowed(ctx, route) {
			return true, last, errors.New("validated protocol is not routed")
		}
		routedAuth := ocgAuthForRoute(auth, route)
		routedReq := req
		if strings.TrimSpace(route.UpstreamModel) != "" {
			routedReq.Model = route.UpstreamModel
		}
		if i == 0 {
			activeCtx = context.WithValue(ctx, ocgActiveRouteKey{}, route.Protocol)
		} else {
			if pubErr := cliproxyauth.PublishInternalGeneration(activeCtx, routedAuth, routedReq.Model, lastErr); pubErr != nil {
				if cliproxyauth.RequestStopped(pubErr) || !errors.Is(pubErr, cliproxyauth.ErrAttemptSkip) {
					return true, last, pubErr
				}
			}
			next, errGate := cliproxyauth.AdmitInternalGeneration(context.WithValue(ctx, ocgActiveRouteKey{}, route.Protocol), routedAuth, routedReq.Model)
			if errGate != nil {
				return true, last, errGate
			}
			activeCtx = next
		}
		stream, errExec := e.ExecuteStream(activeCtx, routedAuth, routedReq, opts)
		if errExec == nil {
			if i > 0 {
				stream = publishStreamWhenFinished(activeCtx, routedAuth, routedReq.Model, stream)
			}
			return true, stream, nil
		}
		if cliproxyauth.RequestStopped(errExec) {
			return true, stream, errExec
		}
		last = stream
		lastErr = errExec
	}
	return true, last, lastErr
}

func publishStreamWhenFinished(ctx context.Context, auth *cliproxyauth.Auth, model string, stream *cliproxyexecutor.StreamResult) *cliproxyexecutor.StreamResult {
	if stream == nil || stream.Chunks == nil {
		_ = cliproxyauth.PublishInternalGeneration(ctx, auth, model, nil)
		return stream
	}
	out := make(chan cliproxyexecutor.StreamChunk)
	go func() {
		defer close(out)
		var chunkErr error
		for chunk := range stream.Chunks {
			if chunk.Err != nil && chunkErr == nil {
				chunkErr = chunk.Err
			}
			out <- chunk
		}
		_ = cliproxyauth.PublishInternalGeneration(ctx, auth, model, chunkErr)
	}()
	return &cliproxyexecutor.StreamResult{Headers: stream.Headers, Chunks: out}
}

func ocgAuthForRoute(auth *cliproxyauth.Auth, route ocgProtocolRoute) *cliproxyauth.Auth {
	if auth == nil {
		return nil
	}
	clone := auth.Clone()
	if clone.Attributes == nil {
		clone.Attributes = map[string]string{}
	}
	if endpoint := strings.TrimSpace(route.Endpoint); endpoint != "" {
		clone.Attributes[ocgEndpointAttribute] = endpoint
	}
	if protocol := strings.TrimSpace(route.Protocol); protocol != "" {
		clone.Attributes[ocgProtocolAttribute] = protocol
	}
	if scheme := strings.TrimSpace(route.AuthScheme); scheme != "" {
		clone.Attributes[ocgAuthSchemeAttribute] = scheme
	}
	if identity := strings.TrimSpace(route.Identity); identity != "" {
		clone.Attributes[ocgIdentityAttribute] = identity
	}
	if wire := strings.TrimSpace(route.Wire); wire != "" {
		clone.Attributes[ocgWireAttribute] = wire
	}
	delete(clone.Attributes, ocgRoutesAttribute)
	return clone
}

func ocgComposeURL(baseURL, endpoint string) string {
	endpoint = strings.TrimSpace(endpoint)
	if strings.HasPrefix(endpoint, "http://") || strings.HasPrefix(endpoint, "https://") {
		return endpoint
	}
	if endpoint == "" {
		endpoint = "/chat/completions"
	}
	if !strings.HasPrefix(endpoint, "/") {
		endpoint = "/" + endpoint
	}
	return strings.TrimSuffix(strings.TrimSpace(baseURL), "/") + endpoint
}

func ocgProtocolTarget(auth *cliproxyauth.Auth, to sdktranslator.Format, endpoint, alt string) (sdktranslator.Format, string) {
	if alt == "responses/compact" {
		return sdktranslator.FromString("openai-response"), "/responses/compact"
	}
	if auth == nil || auth.Attributes == nil {
		if endpoint == "" {
			endpoint = "/chat/completions"
		}
		return to, endpoint
	}
	switch strings.ToLower(strings.TrimSpace(auth.Attributes[ocgProtocolAttribute])) {
	case "openai-response", "responses":
		to = sdktranslator.FromString("openai-response")
		if strings.TrimSpace(auth.Attributes[ocgEndpointAttribute]) == "" && endpoint == "/chat/completions" {
			endpoint = "/responses"
		}
	case "messages", "anthropic":
		to = sdktranslator.FromString("claude")
	}
	if configured := strings.TrimSpace(auth.Attributes[ocgEndpointAttribute]); configured != "" {
		if strings.HasPrefix(configured, "http://") || strings.HasPrefix(configured, "https://") || strings.HasPrefix(configured, "/") {
			endpoint = configured
		} else {
			endpoint = "/" + configured
		}
	}
	if endpoint == "" {
		endpoint = "/chat/completions"
	}
	return to, endpoint
}

func ocgApplyAuthorization(header http.Header, apiKey string, auth *cliproxyauth.Auth) {
	if header == nil {
		return
	}
	scheme := "bearer"
	if auth != nil && auth.Attributes != nil {
		if configured := strings.TrimSpace(auth.Attributes[ocgAuthSchemeAttribute]); configured != "" {
			scheme = strings.ToLower(configured)
		}
	}
	apiKey = strings.TrimSpace(apiKey)
	switch scheme {
	case "none":
		return
	case "raw":
		if apiKey != "" {
			header.Set("Authorization", apiKey)
		}
	case "x-api-key", "api-key":
		if apiKey != "" {
			header.Set("x-api-key", apiKey)
		}
	default:
		if apiKey != "" {
			header.Set("Authorization", "Bearer "+apiKey)
		}
	}
}

func ocgStripCorrelationHeaders(header http.Header) {
	if header == nil {
		return
	}
	for key := range header {
		if strings.HasPrefix(strings.ToLower(key), "x-ocg-") {
			header.Del(key)
		}
	}
}

func ocgSelectableRoutes(ctx context.Context, auth *cliproxyauth.Auth, req cliproxyexecutor.Request, opts cliproxyexecutor.Options) (bool, []ocgProtocolRoute, error) {
	if ctx != nil && ctx.Value(ocgActiveRouteKey{}) != nil {
		return false, nil, nil
	}
	routes, errRoutes := ocgProtocolRoutes(auth, ocgRequestedModel(req, opts)...)
	if errRoutes != nil {
		return true, nil, errRoutes
	}
	if len(routes) == 0 {
		if cliproxyauth.OCGRequestKind(ctx) == "validated" {
			return true, nil, errors.New("validated protocol is not routed")
		}
		return false, nil, nil
	}
	selected := make([]ocgProtocolRoute, 0, len(routes))
	for _, route := range routes {
		if ocgRouteAllowed(ctx, route) {
			selected = append(selected, route)
		}
	}
	if len(selected) == 0 {
		return true, nil, errors.New("validated protocol is not routed")
	}
	return true, selected, nil
}

func ocgRouteAllowed(ctx context.Context, route ocgProtocolRoute) bool {
	protocol := strings.ToLower(strings.TrimSpace(route.Protocol))
	if cliproxyauth.OCGRequestKind(ctx) == "validated" {
		want := cliproxyauth.OCGValidatedProtocol(ctx)
		return want != "" && protocol == want
	}
	return !route.ValidationOnly
}

func ocgHeaderValue(header http.Header, names ...string) string {
	if header == nil {
		return ""
	}
	for _, name := range names {
		if value := strings.TrimSpace(header.Get(name)); value != "" {
			return value
		}
	}
	return ""
}

func ocgApplyIdentityHeaders(header http.Header, auth *cliproxyauth.Auth, opts cliproxyexecutor.Options) {
	if header == nil || auth == nil || auth.Attributes == nil {
		return
	}
	identity := strings.ToLower(strings.TrimSpace(auth.Attributes[ocgIdentityAttribute]))
	sessionOnly := identity == "opencode-session" || identity == "opencode-go"
	zen := identity == "opencode-zen" || identity == "zen-free" || identity == "zen"
	if !sessionOnly && !zen {
		return
	}
	session := ocgHeaderValue(opts.Headers, "x-opencode-session", "x-session-id", "x-session-affinity")
	if session == "" {
		session = ocgHeaderValue(header, "x-opencode-session", "x-session-id", "x-session-affinity")
	}
	requestID := ocgReadRequestID(opts)
	if session == "" {
		session = requestID
	}
	if session == "" {
		session = "ocg-session"
	}
	header.Set("x-opencode-session", session)
	if zen {
		if header.Get("x-opencode-client") == "" {
			header.Set("x-opencode-client", ocgZenClient)
		}
		if header.Get("x-opencode-request") == "" && requestID != "" {
			header.Set("x-opencode-request", requestID)
		}
		if header.Get("x-opencode-project") == "" {
			header.Set("x-opencode-project", session)
		}
		ua := strings.ToLower(header.Get("User-Agent"))
		if !strings.Contains(ua, "opencode") {
			header.Set("User-Agent", ocgZenUserAgent)
		}
	}
}

func ocgWireIsOllama(auth *cliproxyauth.Auth) bool {
	if auth == nil || auth.Attributes == nil {
		return false
	}
	switch strings.ToLower(strings.TrimSpace(auth.Attributes[ocgWireAttribute])) {
	case "ollama", "ollama-reasoning":
		return true
	default:
		return false
	}
}

func ocgNormalizeOllamaRequest(auth *cliproxyauth.Auth, body []byte) []byte {
	if !ocgWireIsOllama(auth) || len(body) == 0 || !gjson.ValidBytes(body) {
		return body
	}
	changed := false
	messages := gjson.GetBytes(body, "messages")
	if messages.IsArray() {
		index := 0
		messages.ForEach(func(_, message gjson.Result) bool {
			path := "messages." + strconv.Itoa(index)
			index++
			if message.Get("role").String() != "assistant" {
				return true
			}
			content := message.Get("reasoning_content").String()
			if content == "" {
				return true
			}
			reasoning := message.Get("reasoning")
			if reasoning.Exists() && reasoning.String() != "" && reasoning.Type != gjson.Null {
				return true
			}
			updated, err := sjson.SetBytes(body, path+".reasoning", content)
			if err == nil {
				body = updated
				changed = true
			}
			return true
		})
	}
	for _, field := range []string{"max_tokens", "max_completion_tokens"} {
		value := gjson.GetBytes(body, field)
		if value.Exists() && value.Int() > ocgMaxOllamaTokens {
			updated, err := sjson.SetBytes(body, field, ocgMaxOllamaTokens)
			if err == nil {
				body = updated
				changed = true
			}
		}
	}
	if !changed {
		return body
	}
	return body
}

func ocgBackfillOllamaReasoning(auth *cliproxyauth.Auth, body []byte) []byte {
	if !ocgWireIsOllama(auth) || len(body) == 0 || !gjson.ValidBytes(body) {
		return body
	}
	choices := gjson.GetBytes(body, "choices")
	if !choices.IsArray() {
		return body
	}
	index := 0
	choices.ForEach(func(_, choice gjson.Result) bool {
		base := "choices." + strconv.Itoa(index)
		index++
		for _, container := range []string{"message", "delta"} {
			target := choice.Get(container)
			if !target.Exists() {
				continue
			}
			if target.Get("reasoning_content").Exists() && target.Get("reasoning_content").Type != gjson.Null {
				continue
			}
			source := target.Get("reasoning").String()
			if source == "" {
				source = target.Get("thinking").String()
			}
			if source == "" {
				continue
			}
			updated, err := sjson.SetBytes(body, base+"."+container+".reasoning_content", source)
			if err == nil {
				body = updated
			}
		}
		return true
	})
	return body
}

func ocgReadRequestID(opts cliproxyexecutor.Options) string {
	if opts.Headers != nil {
		if value := strings.TrimSpace(opts.Headers.Get("X-Request-Id")); value != "" {
			return value
		}
	}
	if opts.Metadata != nil {
		if value, _ := opts.Metadata["ocg_request_id"].(string); strings.TrimSpace(value) != "" {
			return strings.TrimSpace(value)
		}
	}
	return ""
}
