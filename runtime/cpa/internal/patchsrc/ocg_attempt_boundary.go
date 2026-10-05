package auth

import (
	"context"
	"errors"
	"strconv"
	"strings"
	"sync/atomic"
	"time"

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
	Action    string
	Reason    string
	AttemptID string
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
	manager    *Manager
	auth       *Auth
	routeModel string
	attemptID  atomic.Value
}

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

func requestStopped(ctx context.Context) bool {
	stop, _ := ctx.Value(requestStopKey{}).(*requestStop)
	return stop != nil && stop.stopped.Load()
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
	boundary := m.currentGenerationBoundary()
	if boundary == nil {
		return ctx, nil
	}
	cbCtx, cancel := detachPolicyContext(ctx)
	defer cancel()
	decision, err := boundary.BeforeSend(cbCtx, auth, kind, model, routeModel)
	if decision.AttemptID != "" {
		gate.attemptID.Store(decision.AttemptID)
		ctx = context.WithValue(ctx, generationAttemptKey{}, decision.AttemptID)
	}
	if err != nil {
		markRequestStopped(ctx)
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, err))
	}
	switch strings.ToLower(strings.TrimSpace(decision.Action)) {
	case "", "allow":
		return ctx, nil
	case "skip":
		return ctx, errors.Join(ErrAttemptSkip, errors.New(decision.Reason))
	case "stop":
		markRequestStopped(ctx)
		if decision.Reason == "" {
			return ctx, wrapRequestStopError(ErrAttemptStop)
		}
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, errors.New(decision.Reason)))
	default:
		markRequestStopped(ctx)
		return ctx, wrapRequestStopError(errors.Join(ErrAttemptStop, errors.New("invalid generation decision")))
	}
}

// CurrentAttemptID returns the attempt id captured for the latest gated send.
func CurrentAttemptID(ctx context.Context) string {
	if ctx == nil {
		return ""
	}
	if gate, ok := ctx.Value(internalGateKey{}).(*internalGate); ok && gate != nil {
		if id, okID := gate.attemptID.Load().(string); okID && id != "" {
			return id
		}
	}
	id, _ := ctx.Value(generationAttemptKey{}).(string)
	return id
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
	kind, _ := ctx.Value(generationKindKey{}).(string)
	cbCtx, cancel := detachPolicyContext(ctx)
	defer cancel()
	err := boundary.AfterResult(cbCtx, m.authSnapshot(result.AuthID), kind, result.Model, result.RouteModel, result)
	if err != nil {
		markRequestStopped(ctx)
		result.AuthID = ""
		return
	}
	if result.HaltRotation {
		markRequestStopped(ctx)
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
// credential. A changed SDK Generation does not reject the result.
func generationIdentityAllows(auth *Auth, result Result) bool {
	if auth == nil || !result.IdentityBound {
		return true
	}
	if result.RegistrationEpoch != 0 && auth.RegistrationEpoch != result.RegistrationEpoch {
		return false
	}
	live := CaptureGenerationIdentity(auth)
	if result.CredentialID != "" && live.CredentialID != "" && result.CredentialID != live.CredentialID {
		return false
	}
	if result.CredentialVersion != "" && live.CredentialVersion != "" && result.CredentialVersion != live.CredentialVersion {
		return false
	}
	if !live.OAuth && !result.OAuthIdentity && result.MaterialRevision != "" && live.MaterialRevision != "" && result.MaterialRevision != live.MaterialRevision {
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
func PublishInternalGeneration(ctx context.Context, auth *Auth, model string, err error) {
	if ctx == nil {
		return
	}
	gate, _ := ctx.Value(internalGateKey{}).(*internalGate)
	if gate == nil || gate.manager == nil {
		return
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
