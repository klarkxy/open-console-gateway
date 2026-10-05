package auth

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/router-for-me/CLIProxyAPI/v8/internal/registry"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/executor"
)

const (
	GenerationKindExecute         = "execute"
	GenerationKindRefreshResend   = "refresh-resend"
	GenerationKindStream          = "stream"
	GenerationKindStreamRefresh   = "stream-refresh"
	GenerationKindStreamBootstrap = "stream-bootstrap"
	GenerationKindInternal        = "internal"
	GenerationKindCount           = "count-tokens"
)

// GenerationDecision is the synchronous per-send policy result.
// Action is allow, skip, or stop. skip does not send and lets the existing
// selector try another credential. stop ends the request.
// AttemptID is the policy attempt shared by this send and its result.
type GenerationDecision struct {
	Action       string
	Reason       string
	AttemptID    string
	EndpointPins []EndpointPin
}

// GenerationBoundary is the mandatory before-send and after-result hook.
// Implementations must not be called with a Manager lock held.
type GenerationBoundary interface {
	BeforeSend(ctx context.Context, auth *Auth, kind, model, routeModel string) (GenerationDecision, error)
	AfterResult(ctx context.Context, auth *Auth, kind, model, routeModel string, result *Result) error
}

// ErrAttemptSkip means this credential must not be sent and another may be chosen.
var ErrAttemptSkip = errors.New("ocg attempt skip")

// ErrAttemptStop means the request must end without another generation send.
var ErrAttemptStop = errors.New("ocg attempt stop")

type generationBoundaryHolder struct {
	boundary GenerationBoundary
}

type requestStop struct {
	stopped atomic.Bool
}

type requestStopKey struct{}
type generationKindKey struct{}
type generationIdentityKey struct{}
type internalGateKey struct{}

// GenerationIdentity is the non-secret credential fence captured at send time.
// RegistrationEpoch fences replacement. Credential version fences OCG API-key
// rotation. Material revision fences non-OAuth secret replacement. SDK
// Generation is intentionally absent: MarkResult increments it after every
// ordinary result, so fencing on it would drop concurrent quota evidence.
type GenerationIdentity struct {
	AuthID            string
	RegistrationEpoch uint64
	CredentialID      string
	CredentialVersion string
	MaterialRevision  string
	OAuth             bool
}

type generationAttemptKey struct{}

type internalGate struct {
	manager          *Manager
	auth             *Auth
	routeModel       string
	attemptID        atomic.Value
	resultAction     atomic.Value
	publishedAttempt atomic.Value
	halted           atomic.Bool
}

type generationFacts struct {
	mu               sync.Mutex
	sent             bool
	bodyComplete     bool
	streamStarted    bool
	hasStatus        bool
	status           int
	transport        string
	body             string
	headers          map[string]string
	usage            json.RawMessage
	observedAt       time.Time
	admitted         bool
	native           bool
	localCount       bool
	callableProtocol string
	generationKind   string
	allowed          []EndpointPin
	dispatchConsumed bool
	matched          *EndpointPin
}

type generationFactsKey struct{}
type generationBoundaryRequiredKey struct{}
type callerErrKey struct{}

const (
	transportUnsent = "unsent"
	transportLost   = "lost"
)

// SetGenerationBoundary installs the per-attempt policy hook.
func (m *Manager) SetGenerationBoundary(boundary GenerationBoundary) {
	if m == nil {
		return
	}
	if boundary == nil {
		m.generationBoundary.Store(nil)
		return
	}
	m.generationBoundary.Store(&generationBoundaryHolder{boundary: boundary})
}

func (m *Manager) currentGenerationBoundary() GenerationBoundary {
	if m == nil {
		return nil
	}
	holder := m.generationBoundary.Load()
	if holder == nil {
		return nil
	}
	return holder.boundary
}

func ensureRequestStop(ctx context.Context) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	if _, ok := ctx.Value(requestStopKey{}).(*requestStop); ok {
		return ctx
	}
	return context.WithValue(ctx, requestStopKey{}, &requestStop{})
}

func markRequestStopped(ctx context.Context) {
	stop, _ := ctx.Value(requestStopKey{}).(*requestStop)
	if stop != nil {
		stop.stopped.Store(true)
	}
}

func noteRotationHalted(ctx context.Context) {
	markRequestStopped(ctx)
	if gate, ok := ctx.Value(internalGateKey{}).(*internalGate); ok && gate != nil {
		gate.halted.Store(true)
	}
}

func rotationHalted(ctx context.Context) bool {
	if requestStopped(ctx) {
		return true
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	return gate != nil && gate.halted.Load()
}

func requestStopped(ctx context.Context) bool {
	stop, _ := ctx.Value(requestStopKey{}).(*requestStop)
	if stop != nil && stop.stopped.Load() {
		return true
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	return gate != nil && gate.halted.Load()
}

func generationGateError(err error) (stop bool, skip bool) {
	if err == nil {
		return false, false
	}
	if errors.Is(err, ErrAttemptSkip) {
		return false, true
	}
	return true, false
}

// CaptureGenerationIdentity reads the non-secret fence fields from an auth.
func CaptureGenerationIdentity(auth *Auth) GenerationIdentity {
	id := GenerationIdentity{}
	if auth == nil {
		return id
	}
	id.AuthID = auth.ID
	id.RegistrationEpoch = auth.RegistrationEpoch
	if auth.Metadata != nil {
		id.CredentialID, _ = auth.Metadata["ocg_credential_id"].(string)
		id.CredentialVersion = metadataString(auth.Metadata["ocg_credential_version"])
		id.MaterialRevision, _ = auth.Metadata["ocg_material_revision"].(string)
		if oauth, ok := auth.Metadata["ocg_oauth"].(bool); ok {
			id.OAuth = oauth
		}
	}
	if authHasRefreshCredential(auth) {
		id.OAuth = true
	}
	id.CredentialID = strings.TrimSpace(id.CredentialID)
	id.CredentialVersion = strings.TrimSpace(id.CredentialVersion)
	id.MaterialRevision = strings.TrimSpace(id.MaterialRevision)
	return id
}

func metadataString(value any) string {
	switch typed := value.(type) {
	case string:
		return typed
	case int:
		return strconv.Itoa(typed)
	case int64:
		return strconv.FormatInt(typed, 10)
	case float64:
		return strconv.FormatInt(int64(typed), 10)
	default:
		return ""
	}
}

func (m *Manager) beforeGenerationSend(ctx context.Context, auth *Auth, kind, model, routeModel string) (context.Context, error) {
	ctx = ensureRequestStop(ctx)
	if requestStopped(ctx) {
		return ctx, ErrAttemptStop
	}
	identity := CaptureGenerationIdentity(auth)
	ctx = context.WithValue(ctx, generationIdentityKey{}, identity)
	ctx = context.WithValue(ctx, generationKindKey{}, kind)
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil {
		gate = &internalGate{manager: m, auth: auth, routeModel: routeModel}
		ctx = context.WithValue(ctx, internalGateKey{}, gate)
	} else {
		gate.manager = m
		gate.auth = auth
		if routeModel != "" {
			gate.routeModel = routeModel
		}
	}
	gate.resultAction.Store("")
	facts := &generationFacts{}
	ctx = context.WithValue(ctx, generationFactsKey{}, facts)
	boundary := m.currentGenerationBoundary()
	if boundary == nil {
		return ctx, nil
	}
	ctx = context.WithValue(ctx, generationBoundaryRequiredKey{}, true)
	ctx, protocol, errProtocol := resolveCallableProtocol(ctx)
	if errProtocol != nil {
		noteRotationHalted(ctx)
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, errProtocol))
	}
	if err := ctx.Err(); err != nil {
		noteRotationHalted(ctx)
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, err))
	}
	decision, err := boundary.BeforeSend(ctx, auth, kind, model, routeModel)
	if decision.AttemptID != "" {
		gate.attemptID.Store(decision.AttemptID)
		ctx = context.WithValue(ctx, generationAttemptKey{}, decision.AttemptID)
	}
	if err != nil {
		noteRotationHalted(ctx)
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, err))
	}
	switch strings.ToLower(strings.TrimSpace(decision.Action)) {
	case "allow":
		if err := ctx.Err(); err != nil {
			noteRotationHalted(ctx)
			return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, err))
		}
		rememberAdmission(facts, auth, identity, protocol, kind, decision.EndpointPins)
		return applyOCGProxy(ctx, auth, routeModel), nil
	case "skip":
		return ctx, errors.Join(ErrAttemptSkip, errors.New(decision.Reason))
	case "stop":
		noteRotationHalted(ctx)
		if decision.Reason == "" {
			return ctx, wrapRequestStopError(ErrAttemptStop)
		}
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, errors.New(decision.Reason)))
	default:
		noteRotationHalted(ctx)
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, errors.New("invalid generation decision")))
	}
}

// CurrentAttemptID returns the attempt id captured for the latest gated send.
func CurrentAttemptID(ctx context.Context) string {
	if ctx == nil {
		return ""
	}
	if id, ok := ctx.Value(generationAttemptKey{}).(string); ok && id != "" {
		return id
	}
	if gate, ok := ctx.Value(internalGateKey{}).(*internalGate); ok && gate != nil {
		if id, okID := gate.attemptID.Load().(string); okID {
			return id
		}
	}
	return ""
}

func detachPolicyContext(ctx context.Context) (context.Context, context.CancelFunc) {
	base := context.WithoutCancel(ctx)
	if base == nil {
		base = context.Background()
	}
	return context.WithTimeout(base, 2*time.Second)
}

func (m *Manager) observeGenerationResult(ctx context.Context, result *Result) {
	if m == nil || result == nil || result.AuthID == "" {
		return
	}
	if identity, ok := ctx.Value(generationIdentityKey{}).(GenerationIdentity); ok && identity.AuthID == result.AuthID {
		if result.RegistrationEpoch == 0 {
			result.RegistrationEpoch = identity.RegistrationEpoch
		}
		if result.CredentialID == "" {
			result.CredentialID = identity.CredentialID
		}
		if result.CredentialVersion == "" {
			result.CredentialVersion = identity.CredentialVersion
		}
		if result.MaterialRevision == "" {
			result.MaterialRevision = identity.MaterialRevision
		}
		result.OAuthIdentity = identity.OAuth
		result.IdentityBound = identity.RegistrationEpoch != 0 || identity.CredentialID != "" || identity.CredentialVersion != "" || identity.MaterialRevision != ""
	}
	boundary := m.currentGenerationBoundary()
	if boundary == nil {
		return
	}
	if attemptPublished(ctx, CurrentAttemptID(ctx)) {
		return
	}
	kind, _ := ctx.Value(generationKindKey{}).(string)
	cbCtx, cancel := detachPolicyContext(ctx)
	defer cancel()
	if callerErr := ctx.Err(); callerErr != nil {
		cbCtx = context.WithValue(cbCtx, callerErrKey{}, callerErr)
	}
	err := boundary.AfterResult(cbCtx, m.authSnapshot(result.AuthID), kind, result.Model, result.RouteModel, result)
	if err != nil {
		noteRotationHalted(ctx)
		result.AuthID = ""
		return
	}
	if result.HaltRotation {
		noteRotationHalted(ctx)
	}
}

func (m *Manager) authSnapshot(id string) *Auth {
	if m == nil || id == "" {
		return nil
	}
	m.mu.RLock()
	defer m.mu.RUnlock()
	auth := m.auths[id]
	if auth == nil {
		return nil
	}
	return auth.Clone()
}

// generationIdentityAllows reports whether a result still belongs to the live
// credential. RegistrationEpoch rejects a captured result after replacement.
// A changed SDK Generation does not reject the result.
func generationIdentityAllows(auth *Auth, result Result) bool {
	if auth == nil || !result.IdentityBound {
		return true
	}
	if result.RegistrationEpoch != 0 && auth.RegistrationEpoch != result.RegistrationEpoch {
		return false
	}
	live := CaptureGenerationIdentity(auth)
	oauth := live.OAuth || result.OAuthIdentity
	if result.CredentialID != "" && live.CredentialID == "" {
		return false
	}
	if result.CredentialID != "" && live.CredentialID != "" && result.CredentialID != live.CredentialID {
		return false
	}
	if result.CredentialVersion != "" && live.CredentialVersion != "" && result.CredentialVersion != live.CredentialVersion {
		return false
	}
	if oauth && result.CredentialID != "" && result.CredentialVersion != "" && live.CredentialID == result.CredentialID && live.CredentialVersion == result.CredentialVersion {
		return true
	}
	if !oauth && result.MaterialRevision != "" && live.MaterialRevision != "" && result.MaterialRevision != live.MaterialRevision {
		return false
	}
	return true
}

// AdmitInternalGeneration is the manager boundary for a nested generation send
// such as a websocket HTTP fallback, compaction resend, or protocol route.
// A missing gate means this process is not the OCG host and ordinary CPA behavior remains.
func AdmitInternalGeneration(ctx context.Context, auth *Auth, model string) (context.Context, error) {
	if ctx == nil {
		return context.Background(), nil
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil || gate.manager == nil {
		return ctx, nil
	}
	routeModel := gate.routeModel
	useAuth := auth
	if useAuth == nil {
		useAuth = gate.auth
	}
	return gate.manager.beforeGenerationSend(ctx, useAuth, GenerationKindInternal, model, routeModel)
}

// PublishInternalGeneration records a nested send that will not return to the
// manager's own result path, such as an intermediate protocol route.
func PublishInternalGeneration(ctx context.Context, auth *Auth, model string, err error) error {
	if ctx == nil {
		return nil
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil || gate.manager == nil {
		return nil
	}
	provider := ""
	if auth != nil {
		provider = auth.Provider
	}
	result := generationFailureResult(auth, provider, model, gate.routeModel, cliproxyexecutor.Options{}, err)
	if err == nil {
		result.Success = true
		result.Error = nil
	}
	gate.manager.observeGenerationResult(ctx, &result)
	markAttemptPublished(ctx, CurrentAttemptID(ctx))
	if requestStopped(ctx) {
		if err == nil {
			err = ErrAttemptStop
		}
		return wrapRequestStopError(err)
	}
	if action, _ := gate.resultAction.Load().(string); action == "skip" {
		return ErrAttemptSkip
	}
	return nil
}

func NoteResultAction(ctx context.Context, action string) {
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil {
		return
	}
	gate.resultAction.Store(strings.ToLower(strings.TrimSpace(action)))
}

func NoteGenerationSent(ctx context.Context) {
	if facts := generationFactsFrom(ctx); facts != nil {
		facts.mu.Lock()
		facts.sent = true
		facts.mu.Unlock()
	}
}

func NoteGenerationBodyComplete(ctx context.Context) {
	if facts := generationFactsFrom(ctx); facts != nil {
		facts.mu.Lock()
		facts.bodyComplete = true
		facts.mu.Unlock()
	}
}

func NoteGenerationStreamStarted(ctx context.Context) {
	if facts := generationFactsFrom(ctx); facts != nil {
		facts.mu.Lock()
		facts.streamStarted = true
		facts.sent = true
		facts.mu.Unlock()
	}
}

func NoteGenerationStatus(ctx context.Context, status int, header http.Header, body []byte) {
	facts := generationFactsFrom(ctx)
	if facts == nil {
		return
	}
	facts.mu.Lock()
	defer facts.mu.Unlock()
	facts.hasStatus = true
	facts.status = status
	facts.headers = trustedHeaders(header)
	if facts.observedAt.IsZero() {
		facts.observedAt = time.Now().UTC()
	}
	if status < 200 || status >= 300 {
		if text, ok := boundedFactBody(body); ok {
			facts.body = text
		}
	}
}

// GenerationObservedAt is the UTC time of the actual upstream response status.
func GenerationObservedAt(ctx context.Context) time.Time {
	facts := generationFactsFrom(ctx)
	if facts == nil {
		return time.Time{}
	}
	facts.mu.Lock()
	defer facts.mu.Unlock()
	return facts.observedAt
}

func NoteGenerationUsage(ctx context.Context, usage []byte) {
	facts := generationFactsFrom(ctx)
	if facts == nil || len(usage) == 0 || !json.Valid(usage) {
		return
	}
	facts.mu.Lock()
	facts.usage = append(json.RawMessage(nil), usage...)
	facts.mu.Unlock()
}

func NoteGenerationTransport(ctx context.Context, err error) {
	facts := generationFactsFrom(ctx)
	if facts == nil || err == nil {
		return
	}
	class := transportLost
	var op *net.OpError
	if errors.As(err, &op) && op != nil && op.Op == "dial" {
		class = transportUnsent
	}
	if errors.Is(err, syscall.ECONNREFUSED) {
		class = transportUnsent
	}
	facts.mu.Lock()
	facts.transport = class
	facts.mu.Unlock()
}

func GenerationFacts(ctx context.Context) (sent, bodyComplete, streamStarted bool, status int, hasStatus bool, transport, body string, headers map[string]string, usage json.RawMessage) {
	facts := generationFactsFrom(ctx)
	if facts == nil {
		return false, false, false, 0, false, "", "", nil, nil
	}
	facts.mu.Lock()
	defer facts.mu.Unlock()
	return facts.sent, facts.bodyComplete, facts.streamStarted, facts.status, facts.hasStatus, facts.transport, facts.body, cloneHeaderMap(facts.headers), append(json.RawMessage(nil), facts.usage...)
}

func CallerErr(ctx context.Context) error {
	if ctx == nil {
		return nil
	}
	err, _ := ctx.Value(callerErrKey{}).(error)
	return err
}

func generationFactsFrom(ctx context.Context) *generationFacts {
	if ctx == nil {
		return nil
	}
	facts, _ := ctx.Value(generationFactsKey{}).(*generationFacts)
	return facts
}

func applyOCGProxy(ctx context.Context, auth *Auth, publicModel string) context.Context {
	if auth == nil || auth.Attributes == nil {
		return ctx
	}
	direction := strings.ToLower(strings.TrimSpace(auth.Attributes["ocg_proxy_direction"]))
	proxyURL := strings.TrimSpace(auth.Attributes["ocg_proxy_url"])
	if direction == "" {
		return ctx
	}
	listed := false
	for _, model := range strings.Split(auth.Attributes["ocg_proxy_models"], ",") {
		if strings.EqualFold(strings.TrimSpace(model), strings.TrimSpace(publicModel)) && strings.TrimSpace(model) != "" {
			listed = true
			break
		}
	}
	useProxy := direction == "whitelist" && listed || direction == "blacklist" && !listed
	if useProxy && proxyURL != "" {
		return cliproxyexecutor.WithRequestProxyURL(ctx, proxyURL)
	}
	return cliproxyexecutor.WithRequestProxyURL(ctx, "direct")
}

func trustedHeaders(header http.Header) map[string]string {
	if header == nil {
		return nil
	}
	out := map[string]string{}
	for _, key := range []string{"Retry-After", "X-Ratelimit-Reset", "X-Ratelimit-Remaining", "X-Ratelimit-Limit"} {
		if value := strings.TrimSpace(header.Get(key)); value != "" && len(value) <= 128 {
			out[key] = value
		}
	}
	if len(out) == 0 {
		return nil
	}
	return out
}

func cloneHeaderMap(in map[string]string) map[string]string {
	if len(in) == 0 {
		return nil
	}
	out := make(map[string]string, len(in))
	for key, value := range in {
		out[key] = value
	}
	return out
}

func boundedFactBody(body []byte) (string, bool) {
	// A prefix of an oversize body is not a complete provider document.
	if len(body) > 64<<10 {
		return "", false
	}
	return string(body), true
}

func generationFailureResult(auth *Auth, provider, model, routeModel string, opts cliproxyexecutor.Options, err error) Result {
	result := Result{Provider: provider, Model: model, RouteModel: routeModel, Success: false, Options: opts}
	if auth != nil {
		result.AuthID = auth.ID
	}
	if err != nil {
		result.Error = resultErrorFromError(err)
		result.RetryAfter = retryAfterFromError(err)
		result.CredentialScope = isCredentialScopedError(err)
	}
	return result
}

type ocgRequestKindContextKey struct{}

type ocgValidatedProtocolContextKey struct{}

// WithOCGRequestKind stores the private request kind. It does not replace the
// SDK generation kind recorded for execute, refresh, and stream attempts.
func WithOCGRequestKind(ctx context.Context, kind string) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	if strings.ToLower(strings.TrimSpace(kind)) != "validated" {
		kind = "accepted"
	} else {
		kind = "validated"
	}
	return context.WithValue(ctx, ocgRequestKindContextKey{}, kind)
}

// OCGRequestKind returns validated or accepted. A missing value is accepted.
func OCGRequestKind(ctx context.Context) string {
	if ctx == nil {
		return "accepted"
	}
	kind, _ := ctx.Value(ocgRequestKindContextKey{}).(string)
	if kind == "validated" {
		return "validated"
	}
	return "accepted"
}

// WithOCGValidatedProtocol stores the approved protocol for a validated request.
func WithOCGValidatedProtocol(ctx context.Context, protocol string) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	protocol = strings.ToLower(strings.TrimSpace(protocol))
	return context.WithValue(ctx, ocgValidatedProtocolContextKey{}, protocol)
}

// OCGValidatedProtocol returns the approved protocol, or empty when unset.
func OCGValidatedProtocol(ctx context.Context) string {
	if ctx == nil {
		return ""
	}
	protocol, _ := ctx.Value(ocgValidatedProtocolContextKey{}).(string)
	return strings.ToLower(strings.TrimSpace(protocol))
}

func attemptPublished(ctx context.Context, attempt string) bool {
	if attempt == "" || ctx == nil {
		return false
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil {
		return false
	}
	got, _ := gate.publishedAttempt.Load().(string)
	return got == attempt
}

func markAttemptPublished(ctx context.Context, attempt string) {
	if attempt == "" || ctx == nil {
		return
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil {
		return
	}
	gate.publishedAttempt.Store(attempt)
}

// RequestStopped reports a wrapped request-stop error.
func RequestStopped(err error) bool {
	return isRequestStopError(err)
}

// StopRequest ends later sends for this logical request.
func StopRequest(ctx context.Context, err error) error {
	noteRotationHalted(ctx)
	if err == nil {
		err = ErrAttemptStop
	}
	return wrapRequestStopError(err)
}

// WithAttemptContext attaches the admission identity and fact slot used by result publication.
func WithAttemptContext(ctx context.Context, attemptID string) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil {
		gate = &internalGate{}
		ctx = context.WithValue(ctx, internalGateKey{}, gate)
	}
	if attemptID != "" {
		gate.attemptID.Store(attemptID)
		ctx = context.WithValue(ctx, generationAttemptKey{}, attemptID)
	}
	if generationFactsFrom(ctx) == nil {
		ctx = context.WithValue(ctx, generationFactsKey{}, &generationFacts{})
	}
	return ctx
}

// WithCallerErr preserves the caller cancellation that result publication observes after detach.
func WithCallerErr(ctx context.Context, err error) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	return context.WithValue(ctx, callerErrKey{}, err)
}

// NoteOCGHTTPDispatch records whether the existing HTTP client actually dispatched.
// A successful response body is wrapped so the executor's own read supplies the facts.
func NoteOCGHTTPDispatch(ctx context.Context, err error, resp *http.Response) {
	if err != nil || resp == nil {
		NoteGenerationTransport(ctx, err)
		return
	}
	NoteGenerationSent(ctx)
	NoteGenerationStatus(ctx, resp.StatusCode, resp.Header, nil)
	if resp.Body == nil {
		return
	}
	stream := strings.Contains(strings.ToLower(resp.Header.Get("Content-Type")), "event-stream")
	resp.Body = &ocgResponseBody{ctx: ctx, inner: resp.Body, stream: stream}
}

// NoteOCGUsagePayload records a canonical usage object from one JSON payload or SSE data line.
func NoteOCGUsagePayload(ctx context.Context, payload []byte) {
	if usage := usageObject(payload); len(usage) > 0 {
		NoteGenerationUsage(ctx, usage)
	}
}

// OCGMaterialRevision is the non-secret hash of the live OAuth token fields.
func OCGMaterialRevision(metadata map[string]any) string {
	if len(metadata) == 0 {
		return ""
	}
	parts := make([]string, 0, 6)
	for _, key := range []string{"refresh_token", "refreshToken", "access_token", "accessToken", "id_token", "idToken"} {
		value, _ := metadata[key].(string)
		if value != "" {
			parts = append(parts, value)
		}
	}
	if len(parts) == 0 {
		return ""
	}
	sum := sha256.Sum256([]byte(strings.Join(parts, "\n")))
	return hex.EncodeToString(sum[:])
}

func refreshOCGMaterial(auth *Auth) {
	if auth == nil || auth.Metadata == nil {
		return
	}
	revision := OCGMaterialRevision(auth.Metadata)
	if revision == "" {
		return
	}
	auth.Metadata["ocg_material_revision"] = revision
}

// OCGScopeAuthModels limits a native auth to the binding's public models.
func OCGScopeAuthModels(auth *Auth, modelIDs []string) {
	if auth == nil {
		return
	}
	if auth.Attributes == nil {
		auth.Attributes = map[string]string{}
	}
	clean := uniqueModelIDs(modelIDs)
	allowed := map[string]struct{}{}
	for _, id := range clean {
		allowed[id] = struct{}{}
	}
	var excluded []string
	for _, info := range catalogForProvider(auth.Provider) {
		if info == nil || strings.TrimSpace(info.ID) == "" {
			continue
		}
		if _, ok := allowed[info.ID]; ok {
			continue
		}
		excluded = append(excluded, info.ID)
	}
	if len(excluded) > 0 {
		auth.Attributes["excluded_models"] = strings.Join(excluded, ",")
	}
	auth.Attributes["ocg_model_scope"] = strings.Join(clean, ",")
}

// OCGRegisterModelScope publishes the binding's public models for scheduler eligibility.
func OCGRegisterModelScope(clientID, provider string, modelIDs []string) {
	clientID = strings.TrimSpace(clientID)
	if clientID == "" {
		return
	}
	clean := uniqueModelIDs(modelIDs)
	models := make([]*registry.ModelInfo, 0, len(clean))
	for _, id := range clean {
		models = append(models, &registry.ModelInfo{ID: id})
	}
	registry.GetGlobalRegistry().RegisterClient(clientID, provider, models)
}

// OCGRegisteredModelIDs returns the public model ids currently registered for an auth.
func OCGRegisteredModelIDs(clientID string) []string {
	models := registry.GetGlobalRegistry().GetModelsForClient(strings.TrimSpace(clientID))
	out := make([]string, 0, len(models))
	for _, model := range models {
		if model == nil || strings.TrimSpace(model.ID) == "" {
			continue
		}
		out = append(out, model.ID)
	}
	return out
}

func uniqueModelIDs(modelIDs []string) []string {
	seen := map[string]struct{}{}
	out := make([]string, 0, len(modelIDs))
	for _, id := range modelIDs {
		id = strings.TrimSpace(id)
		if id == "" {
			continue
		}
		if _, ok := seen[id]; ok {
			continue
		}
		seen[id] = struct{}{}
		out = append(out, id)
	}
	return out
}

// OCGHoldModelPublication runs after the preliminary current-auth check and
// before the publication lock. The argument is the captured auth. Production
// leaves it nil.
var OCGHoldModelPublication func(*Auth)

// OCGPublishFencedModelScope publishes the manager's current scope only when
// the captured auth still matches that current identity, epoch, generation,
// version, and scope. The comparison and the registry write share Manager.mu.
// A true result owns registration. A false result continues the stock catalog
// path; that path's final registry writes go through OCGCommitCatalogRegistration.
func OCGPublishFencedModelScope(manager *Manager, captured *Auth) bool {
	if captured == nil || strings.TrimSpace(captured.ID) == "" {
		return false
	}
	if manager == nil {
		return publishStoredModelScope(captured)
	}
	manager.mu.Lock()
	defer manager.mu.Unlock()
	current := manager.auths[strings.TrimSpace(captured.ID)]
	if current == nil || current.Disabled || current.Status == StatusDisabled {
		return true
	}
	if present, _ := ocgScopeValue(current); !present {
		return false
	}
	if !ocgPublicationMatches(current, captured) {
		return true
	}
	return publishStoredModelScope(current)
}

// publishStoredModelScope writes the auth's own scope. Callers that need the
// current-auth fence hold Manager.mu. An absent scope, or an empty scope with
// no bound credential, returns false and does not change the registry.
func publishStoredModelScope(auth *Auth) bool {
	if auth == nil || strings.TrimSpace(auth.ID) == "" || auth.Attributes == nil {
		return false
	}
	raw, ok := auth.Attributes["ocg_model_scope"]
	if !ok {
		return false
	}
	ids := uniqueModelIDs(strings.Split(raw, ","))
	if len(ids) == 0 {
		if !currentScopedCredential(auth) {
			return false
		}
		OCGRegisterModelScope(auth.ID, auth.Provider, nil)
		return true
	}
	if !currentScopedCredential(auth) {
		OCGRegisterModelScope(auth.ID, auth.Provider, nil)
		return true
	}
	OCGRegisterModelScope(auth.ID, auth.Provider, ids)
	return true
}

// OCGHoldDisabledUnregister is nil in production. The disabled registration
// branch calls OCGCommitDisabledUnregister after its preliminary current-auth
// read. A non-nil hook must call commit once.
var OCGHoldDisabledUnregister func(captured *Auth, commit func())

// OCGCommitDisabledUnregister unregisters captured.ID only when the current
// auth is missing or still disabled. The read and UnregisterClient share
// Manager.mu. An enabled replacement is not erased.
func OCGCommitDisabledUnregister(manager *Manager, captured *Auth) {
	if captured == nil || strings.TrimSpace(captured.ID) == "" {
		return
	}
	commit := func() { ocgApplyDisabledUnregister(manager, captured) }
	if OCGHoldDisabledUnregister != nil {
		OCGHoldDisabledUnregister(captured, commit)
		return
	}
	commit()
}

func ocgApplyDisabledUnregister(manager *Manager, captured *Auth) {
	id := strings.TrimSpace(captured.ID)
	if manager == nil {
		registry.GetGlobalRegistry().UnregisterClient(id)
		return
	}
	manager.mu.Lock()
	defer manager.mu.Unlock()
	current := manager.auths[id]
	if current != nil && !current.Disabled && current.Status != StatusDisabled {
		return
	}
	registry.GetGlobalRegistry().UnregisterClient(id)
}

// OCGHoldCatalogPublication is nil in production. Catalog registration calls
// it after model resolution and before the registry mutation. A non-nil hook
// must call commit once.
var OCGHoldCatalogPublication func(captured *Auth, commit func())

// OCGCommitCatalogRegistration writes a prepared catalog slice, or unregisters
// when that slice is empty. The current-auth decision and the registry call
// share Manager.mu. A protected current scope is left unchanged.
func OCGCommitCatalogRegistration(manager *Manager, captured *Auth, provider string, models []*registry.ModelInfo) {
	if captured == nil || strings.TrimSpace(captured.ID) == "" {
		return
	}
	commit := func() { ocgApplyCatalogRegistration(manager, captured, provider, models) }
	if OCGHoldCatalogPublication != nil {
		OCGHoldCatalogPublication(captured, commit)
		return
	}
	commit()
}

func ocgApplyCatalogRegistration(manager *Manager, captured *Auth, provider string, models []*registry.ModelInfo) {
	id := strings.TrimSpace(captured.ID)
	provider = strings.TrimSpace(provider)
	if manager == nil {
		ocgWriteCatalog(id, provider, models)
		return
	}
	manager.mu.Lock()
	defer manager.mu.Unlock()
	current := manager.auths[id]
	if current == nil || current.Disabled || current.Status == StatusDisabled {
		registry.GetGlobalRegistry().UnregisterClient(id)
		return
	}
	if !ocgCatalogWriteAllowed(current, captured) {
		return
	}
	ocgWriteCatalog(id, provider, models)
}

func ocgWriteCatalog(id, provider string, models []*registry.ModelInfo) {
	if provider == "" || len(models) == 0 {
		registry.GetGlobalRegistry().UnregisterClient(id)
		return
	}
	registry.GetGlobalRegistry().RegisterClient(id, provider, models)
}

func ocgCatalogWriteAllowed(current, captured *Auth) bool {
	if current == nil || captured == nil || strings.TrimSpace(current.ID) != strings.TrimSpace(captured.ID) {
		return false
	}
	if current.RegistrationEpoch != captured.RegistrationEpoch || current.Generation != captured.Generation {
		return false
	}
	if strings.TrimSpace(current.Provider) != strings.TrimSpace(captured.Provider) {
		return false
	}
	return !ocgScopeProtected(current)
}

func ocgScopeProtected(auth *Auth) bool {
	present, scope := ocgScopeValue(auth)
	if !present {
		return false
	}
	if scope != "" {
		return true
	}
	return currentScopedCredential(auth)
}

func ocgPublicationMatches(current, captured *Auth) bool {
	if current == nil || captured == nil || current.ID != captured.ID {
		return false
	}
	if current.RegistrationEpoch != captured.RegistrationEpoch || current.Generation != captured.Generation {
		return false
	}
	if strings.TrimSpace(current.Provider) != strings.TrimSpace(captured.Provider) {
		return false
	}
	if ocgMetaString(current, "ocg_credential_id") != ocgMetaString(captured, "ocg_credential_id") {
		return false
	}
	if ocgMetaString(current, "ocg_credential_version") != ocgMetaString(captured, "ocg_credential_version") {
		return false
	}
	if strings.TrimSpace(ocgAttr(current, "ocg_provider_id")) != strings.TrimSpace(ocgAttr(captured, "ocg_provider_id")) {
		return false
	}
	currentPresent, currentScope := ocgScopeValue(current)
	capturedPresent, capturedScope := ocgScopeValue(captured)
	return currentPresent == capturedPresent && currentScope == capturedScope
}

func ocgScopeValue(auth *Auth) (bool, string) {
	if auth == nil || auth.Attributes == nil {
		return false, ""
	}
	raw, ok := auth.Attributes["ocg_model_scope"]
	if !ok {
		return false, ""
	}
	return true, strings.Join(uniqueModelIDs(strings.Split(raw, ",")), ",")
}

func ocgMetaString(auth *Auth, key string) string {
	if auth == nil || auth.Metadata == nil {
		return ""
	}
	value, _ := auth.Metadata[key].(string)
	return strings.TrimSpace(value)
}

func ocgAttr(auth *Auth, key string) string {
	if auth == nil || auth.Attributes == nil {
		return ""
	}
	return auth.Attributes[key]
}

func currentScopedCredential(auth *Auth) bool {
	if auth == nil || auth.Attributes == nil || auth.Metadata == nil {
		return false
	}
	if strings.TrimSpace(auth.Attributes["ocg_provider_id"]) == "" {
		return false
	}
	id, _ := auth.Metadata["ocg_credential_id"].(string)
	version, _ := auth.Metadata["ocg_credential_version"].(string)
	return strings.TrimSpace(id) != "" && strings.TrimSpace(version) != ""
}

func catalogForProvider(provider string) []*registry.ModelInfo {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "claude":
		return registry.GetClaudeModels()
	case "antigravity":
		return registry.GetAntigravityModels()
	case "kimi", "kimi-ai", "kimi.ai", "kimi.com":
		return registry.GetKimiModels()
	case "xai":
		return registry.GetXAIModels()
	default:
		var all []*registry.ModelInfo
		all = append(all, registry.GetCodexProModels()...)
		all = append(all, registry.GetCodexPlusModels()...)
		all = append(all, registry.GetCodexTeamModels()...)
		all = append(all, registry.GetCodexFreeModels()...)
		return all
	}
}

type ocgResponseBody struct {
	ctx      context.Context
	inner    io.ReadCloser
	stream   bool
	buf      bytes.Buffer
	tail     bytes.Buffer
	event    string
	terminal bool
	oversize bool
	done     bool
}

func (b *ocgResponseBody) Read(p []byte) (int, error) {
	n, err := b.inner.Read(p)
	if n > 0 {
		chunk := p[:n]
		if b.stream {
			b.consume(chunk)
		} else {
			b.keep(chunk)
		}
	}
	if err != nil {
		b.finish(err)
	}
	return n, err
}

func (b *ocgResponseBody) Close() error {
	if b.inner == nil {
		return nil
	}
	return b.inner.Close()
}

func (b *ocgResponseBody) keep(chunk []byte) {
	if b.oversize {
		return
	}
	if b.buf.Len()+len(chunk) > 64<<10 {
		b.oversize = true
		b.buf.Reset()
		return
	}
	_, _ = b.buf.Write(chunk)
}

func (b *ocgResponseBody) consume(chunk []byte) {
	_, _ = b.tail.Write(chunk)
	raw := b.tail.Bytes()
	for {
		i := bytes.IndexByte(raw, '\n')
		if i < 0 {
			break
		}
		b.scanLine(raw[:i])
		raw = raw[i+1:]
	}
	if len(raw) > 65536 {
		raw = raw[len(raw)-65536:]
	}
	kept := append([]byte(nil), raw...)
	b.tail.Reset()
	_, _ = b.tail.Write(kept)
}

func (b *ocgResponseBody) scanLine(line []byte) {
	if len(bytes.TrimSpace(line)) == 0 {
		b.event = ""
		return
	}
	line = bytes.TrimSpace(line)
	lower := bytes.ToLower(line)
	if bytes.HasPrefix(lower, []byte("event:")) {
		b.event = strings.TrimSpace(string(line[len("event:"):]))
		if structuredTerminalEvent(b.event) {
			b.terminal = true
		}
		return
	}
	if !bytes.HasPrefix(lower, []byte("data:")) {
		return
	}
	data := bytes.TrimSpace(line[len("data:"):])
	if bytes.Equal(data, []byte("[DONE]")) {
		b.terminal = true
		return
	}
	if structuredTerminalEvent(b.event) {
		b.terminal = true
	}
	if !json.Valid(data) {
		return
	}
	if structuredTerminalData(data) || structuredFinishReason(data) {
		b.terminal = true
	}
	NoteGenerationStreamStarted(b.ctx)
	NoteOCGUsagePayload(b.ctx, data)
}

func structuredTerminalEvent(name string) bool {
	switch strings.TrimSpace(name) {
	case "response.completed", "message_stop":
		return true
	default:
		return false
	}
}

func structuredTerminalData(data []byte) bool {
	var doc struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		return false
	}
	return structuredTerminalEvent(doc.Type)
}

func structuredFinishReason(data []byte) bool {
	var doc struct {
		Candidates []struct {
			FinishReason string `json:"finishReason"`
		} `json:"candidates"`
		Response struct {
			Candidates []struct {
				FinishReason string `json:"finishReason"`
			} `json:"candidates"`
		} `json:"response"`
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		return false
	}
	for _, item := range doc.Candidates {
		if strings.TrimSpace(item.FinishReason) != "" {
			return true
		}
	}
	for _, item := range doc.Response.Candidates {
		if strings.TrimSpace(item.FinishReason) != "" {
			return true
		}
	}
	return false
}

func (b *ocgResponseBody) finish(err error) {
	if b == nil || b.done {
		return
	}
	b.done = true
	clean := err == nil || errors.Is(err, io.EOF)
	if b.stream {
		if b.tail.Len() > 0 {
			b.scanLine(b.tail.Bytes())
		}
		if clean && b.terminal {
			NoteGenerationBodyComplete(b.ctx)
		}
		return
	}
	if b.oversize || !clean {
		return
	}
	NoteOCGHTTPBody(b.ctx, b.buf.Bytes(), nil)
}

// NoteOCGHTTPBody records a finished body read and any real usage object in that body.
func NoteOCGHTTPBody(ctx context.Context, body []byte, readErr error) {
	if readErr != nil {
		return
	}
	text, complete := boundedFactBody(body)
	if !complete {
		return
	}
	NoteGenerationBodyComplete(ctx)
	if usage := usageObject(body); len(usage) > 0 {
		NoteGenerationUsage(ctx, usage)
	}
	facts := generationFactsFrom(ctx)
	if facts == nil {
		return
	}
	facts.mu.Lock()
	defer facts.mu.Unlock()
	if facts.hasStatus && (facts.status < 200 || facts.status >= 300) {
		facts.body = text
	}
}

func usageObject(body []byte) json.RawMessage {
	if extracted := usageFromObject(body); len(extracted) > 0 {
		return extracted
	}
	if extracted := antigravityUsageMetadata(body); len(extracted) > 0 {
		return extracted
	}
	if len(body) == 0 || !json.Valid(body) {
		return nil
	}
	var wrap struct {
		Response json.RawMessage `json:"response"`
		Message  json.RawMessage `json:"message"`
	}
	if err := json.Unmarshal(body, &wrap); err != nil {
		return nil
	}
	if extracted := usageFromObject(wrap.Response); len(extracted) > 0 {
		return extracted
	}
	if extracted := antigravityUsageMetadata(wrap.Response); len(extracted) > 0 {
		return extracted
	}
	return usageFromObject(wrap.Message)
}

func antigravityUsageMetadata(body []byte) json.RawMessage {
	if len(body) == 0 || !json.Valid(body) {
		return nil
	}
	var doc struct {
		UsageMetadata   json.RawMessage `json:"usageMetadata"`
		UsageMetadataSN json.RawMessage `json:"usage_metadata"`
		Response        struct {
			UsageMetadata   json.RawMessage `json:"usageMetadata"`
			UsageMetadataSN json.RawMessage `json:"usage_metadata"`
		} `json:"response"`
	}
	if err := json.Unmarshal(body, &doc); err != nil {
		return nil
	}
	node := doc.Response.UsageMetadata
	if len(node) == 0 || string(node) == "null" {
		node = doc.Response.UsageMetadataSN
	}
	if len(node) == 0 || string(node) == "null" {
		node = doc.UsageMetadata
	}
	if len(node) == 0 || string(node) == "null" {
		node = doc.UsageMetadataSN
	}
	if len(node) == 0 || string(node) == "null" || !json.Valid(node) {
		return nil
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(node, &fields); err != nil {
		return nil
	}
	out := map[string]uint64{}
	input, okInput := firstCanonicalUsage(fields, "promptTokenCount")
	if tool, okTool := firstCanonicalUsage(fields, "toolUsePromptTokenCount", "tool_use_prompt_token_count"); okTool {
		if !okInput {
			input = tool
			okInput = true
		} else if input <= 50_000_000-tool {
			input += tool
		}
	}
	if okInput {
		out["inputTokens"] = input
	}
	if number, ok := firstCanonicalUsage(fields, "candidatesTokenCount"); ok {
		out["outputTokens"] = number
	}
	if len(out) == 0 {
		return nil
	}
	encoded, err := json.Marshal(out)
	if err != nil {
		return nil
	}
	return encoded
}

func usageFromObject(body []byte) json.RawMessage {
	if len(body) == 0 || !json.Valid(body) {
		return nil
	}
	var doc struct {
		Usage json.RawMessage `json:"usage"`
	}
	if err := json.Unmarshal(body, &doc); err != nil || len(doc.Usage) == 0 || string(doc.Usage) == "null" {
		return nil
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(doc.Usage, &fields); err != nil {
		return nil
	}
	out := map[string]uint64{}
	if number, ok := firstCanonicalUsage(fields, "inputTokens", "input_tokens", "prompt_tokens"); ok {
		out["inputTokens"] = number
	}
	if number, ok := firstCanonicalUsage(fields, "outputTokens", "output_tokens", "completion_tokens"); ok {
		out["outputTokens"] = number
	}
	if len(out) == 0 {
		return nil
	}
	encoded, err := json.Marshal(out)
	if err != nil {
		return nil
	}
	return encoded
}

func firstCanonicalUsage(fields map[string]json.RawMessage, keys ...string) (uint64, bool) {
	for _, key := range keys {
		raw := fields[key]
		if len(raw) == 0 || strings.ContainsAny(string(raw), ".eE") {
			continue
		}
		var number uint64
		if err := json.Unmarshal(raw, &number); err != nil || number > 50_000_000 {
			continue
		}
		return number, true
	}
	return 0, false
}
