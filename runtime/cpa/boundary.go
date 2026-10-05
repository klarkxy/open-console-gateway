package main

import (
	"context"
	"errors"
	"sort"
	"strings"
	"time"

	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
)

type generationBoundary struct {
	host *Host
}

type attemptCapture struct {
	requestID         string
	processGeneration string
	revision          string
	digest            string
	providerID        string
	storedAt          time.Time
	deadline          time.Time
}

const (
	idleCapture           = 90 * time.Second
	publicationGrace      = 60 * time.Second
	duplicateTombstoneCap = 128
)

var nowFn = time.Now

func hostNow() time.Time { return nowFn() }

func (b generationBoundary) BeforeSend(ctx context.Context, auth *coreauth.Auth, kind, model, routeModel string) (coreauth.GenerationDecision, error) {
	if b.host == nil {
		return coreauth.GenerationDecision{}, errPolicy("host_missing")
	}
	b.sweepCaptures()
	if b.host.unavailable.Load() || !b.host.policyReady.Load() {
		return coreauth.GenerationDecision{}, errPolicy("execution_unavailable")
	}
	// The manager already exposes candidate routes here. Admitting the old
	// revision would authorize a send that commit or rollback cannot undo.
	if b.host.applyBarrier.Load() {
		return coreauth.GenerationDecision{}, errPolicy("apply_barrier")
	}
	if admissionClosed(ctx) {
		return coreauth.GenerationDecision{}, errPolicy("admission_closed")
	}
	identity := coreauth.CaptureGenerationIdentity(auth)
	if identity.OAuth && identity.CredentialID == "" {
		return coreauth.GenerationDecision{Action: "skip", Reason: "oauth_unmapped"}, nil
	}
	if !identity.OAuth && identity.CredentialID == "" {
		return coreauth.GenerationDecision{}, errPolicy("credential_unmapped")
	}
	requestID := requestIDFromContext(ctx)
	if !canonicalUUID(requestID) {
		return coreauth.GenerationDecision{}, errPolicy("missing_policy_identity")
	}
	callable := coreauth.OCGCallableProtocol(ctx)
	switch callable {
	case "chat_completions", "responses", "messages":
	default:
		return coreauth.GenerationDecision{}, errPolicy("unknown_protocol")
	}
	if coreauth.OCGRequestKind(ctx) == "validated" && coreauth.OCGValidatedProtocol(ctx) != callable {
		return coreauth.GenerationDecision{}, errPolicy("protocol_mismatch")
	}
	attemptID := newID()
	var deadline time.Time
	if ctx != nil {
		if value, ok := ctx.Deadline(); ok {
			deadline = value
		}
	}
	capture := attemptCapture{
		requestID:         requestID,
		processGeneration: b.host.processGeneration,
		revision:          b.host.revisionText(),
		digest:            b.host.digestText(),
		providerID:        providerOf(auth),
		storedAt:          hostNow(),
		deadline:          deadline,
	}
	response, err := b.host.postPolicy(ctx, policyRequest{
		ProtocolVersion:    1,
		Operation:          "admit",
		ProcessGeneration:  capture.processGeneration,
		ProjectionRevision: capture.revision,
		ProjectionDigest:   capture.digest,
		RequestID:          requestID,
		AttemptID:          attemptID,
		AuthID:             identity.AuthID,
		CredentialID:       identity.CredentialID,
		CredentialVersion:  identity.CredentialVersion,
		ProviderID:         providerOf(auth),
		PublicModel:        routeModel,
		UpstreamModel:      model,
		RegistrationEpoch:  decimalEpoch(identity.RegistrationEpoch),
		MaterialRevision:   identity.MaterialRevision,
		Kind:               wireRequestKind(ctx, kind),
		CallableProtocol:   callable,
		GenerationKind:     kind,
	}, false)
	if err != nil {
		if ctx.Err() == nil && !localPolicyError(err) {
			b.host.unavailable.Store(true)
		}
		return coreauth.GenerationDecision{}, err
	}
	if response.Unavailable {
		b.host.unavailable.Store(true)
		return coreauth.GenerationDecision{Action: "stop", Reason: response.Reason}, nil
	}
	if response.Action != "allow" && response.Action != "skip" && response.Action != "stop" {
		b.host.unavailable.Store(true)
		return coreauth.GenerationDecision{}, errPolicy("policy_malformed")
	}
	pins := copiedPins(response.EndpointPins)
	if response.Action != "allow" {
		pins = nil
	}
	if response.Action == "allow" {
		if (!identity.OAuth || coreauth.NativeLocalCount(auth, kind)) && len(response.EndpointPins) > 0 {
			b.host.unavailable.Store(true)
			return coreauth.GenerationDecision{}, errPolicy("policy_malformed")
		}
		b.host.captures.Store(attemptID, capture)
		b.host.unavailable.Store(false)
	}
	return coreauth.GenerationDecision{Action: response.Action, Reason: response.Reason, AttemptID: attemptID, EndpointPins: pins}, nil
}

func (b generationBoundary) AfterResult(ctx context.Context, auth *coreauth.Auth, kind, model, routeModel string, result *coreauth.Result) error {
	if b.host == nil || result == nil {
		return errPolicy("host_missing")
	}
	attemptID := coreauth.CurrentAttemptID(ctx)
	if _, loaded := b.host.published.Load(attemptID); loaded && attemptID != "" {
		result.HaltRotation = true
		return errPolicy("duplicate_result")
	}
	raw, ok := b.host.captures.Load(attemptID)
	capture, _ := raw.(attemptCapture)
	if attemptID == "" || !ok || capture.requestID == "" {
		result.HaltRotation = true
		return errPolicy("missing_attempt")
	}
	if _, loaded := b.host.published.LoadOrStore(attemptID, hostNow()); loaded {
		b.host.captures.Delete(attemptID)
		result.HaltRotation = true
		return errPolicy("duplicate_result")
	}
	// Bound the duplicate set when the tombstone is stored. Waiting for the next
	// admit leaves one entry above the cap after the publish that crossed it.
	defer b.sweepPublished()
	identity := coreauth.CaptureGenerationIdentity(auth)
	if result.IdentityBound {
		identity.RegistrationEpoch = result.RegistrationEpoch
		if result.CredentialID != "" {
			identity.CredentialID = result.CredentialID
		}
		if result.CredentialVersion != "" {
			identity.CredentialVersion = result.CredentialVersion
		}
		if result.MaterialRevision != "" {
			identity.MaterialRevision = result.MaterialRevision
		}
		identity.OAuth = identity.OAuth || result.OAuthIdentity
	}
	sent, bodyComplete, streamStarted, status, hasStatus, transport, body, headers, usage := coreauth.GenerationFacts(ctx)
	outcome, code, halt := classifyFacts(kind, result, sent, bodyComplete, streamStarted, status, hasStatus, transport, body, coreauth.CallerErr(ctx))
	reported := canonicalReportedUsage(usage)
	var observation *policyObservation
	if trustedRejection(outcome, body, bodyComplete, status, hasStatus, streamStarted) {
		fetchedAt := coreauth.GenerationObservedAt(ctx)
		if fetchedAt.IsZero() {
			fetchedAt = hostNow().UTC()
		}
		observation = &policyObservation{ID: attemptID, FetchedAt: fetchedAt.UTC().Format(time.RFC3339)}
	}
	response, err := b.host.postPolicy(ctx, policyRequest{
		ProtocolVersion:    1,
		Operation:          "result",
		ProcessGeneration:  capture.processGeneration,
		ProjectionRevision: capture.revision,
		ProjectionDigest:   capture.digest,
		RequestID:          capture.requestID,
		AttemptID:          attemptID,
		AuthID:             result.AuthID,
		CredentialID:       identity.CredentialID,
		CredentialVersion:  identity.CredentialVersion,
		ProviderID:         capturedProvider(capture, auth),
		PublicModel:        routeModel,
		UpstreamModel:      model,
		RegistrationEpoch:  decimalEpoch(identity.RegistrationEpoch),
		MaterialRevision:   identity.MaterialRevision,
		Kind:               wireRequestKind(ctx, kind),
		Sent:               boolPtr(sent),
		Status:             statusPointer(status, hasStatus),
		BodyComplete:       boolPtr(bodyComplete),
		StreamStarted:      boolPtr(streamStarted),
		Outcome:            outcome,
		ErrorCode:          code,
		ResponseBody:       body,
		Headers:            headers,
		ReportedUsage:      reported,
		Observation:        observation,
		EndpointPin:        wireEndpointPin(coreauth.MatchedEndpointPin(ctx)),
	}, true)
	b.host.captures.Delete(attemptID)
	if err != nil {
		result.HaltRotation = true
		if !localPolicyError(err) {
			b.host.unavailable.Store(true)
		}
		return err
	}
	coreauth.NoteResultAction(ctx, response.Action)
	if response.Unavailable || response.Action == "stop" || result.HaltRotation {
		halt = true
	}
	result.HaltRotation = halt
	if response.Unavailable {
		b.host.unavailable.Store(true)
	} else {
		b.host.unavailable.Store(false)
	}
	return nil
}

func classifyFacts(kind string, result *coreauth.Result, sent, bodyComplete, streamStarted bool, status int, hasStatus bool, transport, body string, caller error) (outcome, code string, halt bool) {
	if definiteRejection(sent, bodyComplete, streamStarted, status, hasStatus) && strings.TrimSpace(body) != "" {
		return "explicit_rejection", "provider_rejected", false
	}
	if caller != nil {
		if errors.Is(caller, context.Canceled) {
			return "cancelled", "cancelled", true
		}
		if errors.Is(caller, context.DeadlineExceeded) {
			return "deadline", "deadline", true
		}
	}
	switch transport {
	case "unsent":
		return "local_failure", "transport", false
	case "lost":
		return "uncertain", "body_lost", true
	}
	streamKind := strings.Contains(kind, "stream")
	if streamStarted && (result == nil || !result.Success) {
		return "uncertain", "stream_lost", true
	}
	if sent && !bodyComplete {
		if streamKind || streamStarted {
			return "uncertain", "stream_lost", true
		}
		return "uncertain", "body_lost", true
	}
	if result != nil && result.Success {
		if sent && (!bodyComplete || !hasStatus) {
			if streamKind || streamStarted {
				return "uncertain", "stream_lost", true
			}
			return "uncertain", "body_lost", true
		}
		return "success", "none", false
	}
	if sent && bodyComplete && hasStatus && status >= 200 && status < 300 {
		return "uncertain", "parser", true
	}
	if definiteRejection(sent, bodyComplete, streamStarted, status, hasStatus) {
		return "explicit_rejection", "provider_rejected", false
	}
	if !sent && !hasStatus && transport == "" {
		return "local_failure", "validation", true
	}
	return "uncertain", "unknown", true
}

func definiteRejection(sent, bodyComplete, streamStarted bool, status int, hasStatus bool) bool {
	if !sent || !bodyComplete || streamStarted || !hasStatus {
		return false
	}
	return status == 401 || status == 403 || status == 429
}

func trustedRejection(outcome, body string, bodyComplete bool, status int, hasStatus, streamStarted bool) bool {
	return outcome == "explicit_rejection" && bodyComplete && !streamStarted && strings.TrimSpace(body) != "" && hasStatus && (status == 401 || status == 403 || status == 429)
}

func admissionClosed(ctx context.Context) bool {
	if ctx == nil {
		return false
	}
	if ctx.Err() != nil {
		return true
	}
	deadline, ok := ctx.Deadline()
	return ok && !hostNow().Before(deadline)
}

// wireRequestKind emits the private request kind. The SDK generation kind
// stays on the attempt for stream and refresh tracking.
func wireRequestKind(ctx context.Context, sdkKind string) string {
	if coreauth.OCGRequestKind(ctx) == "validated" {
		return "validated"
	}
	return wireSendKind(sdkKind)
}

func captureExpireAt(capture attemptCapture) time.Time {
	base := capture.storedAt.Add(idleCapture)
	if !capture.deadline.IsZero() {
		base = capture.deadline
	}
	return base.Add(publicationGrace)
}

func (b generationBoundary) sweepCaptures() {
	if b.host == nil {
		return
	}
	now := hostNow()
	b.host.captures.Range(func(key, value any) bool {
		capture, ok := value.(attemptCapture)
		if ok && !captureExpireAt(capture).After(now) {
			b.host.captures.Delete(key)
		}
		return true
	})
	b.sweepPublished()
}

func (b generationBoundary) sweepPublished() {
	if b.host == nil {
		return
	}
	now := hostNow()
	type tombstone struct {
		key any
		at  time.Time
	}
	kept := make([]tombstone, 0)
	b.host.published.Range(func(key, value any) bool {
		at, _ := value.(time.Time)
		if !at.IsZero() && now.Sub(at) > publicationGrace {
			b.host.published.Delete(key)
			return true
		}
		kept = append(kept, tombstone{key: key, at: at})
		return true
	})
	if len(kept) <= duplicateTombstoneCap {
		return
	}
	sort.Slice(kept, func(i, j int) bool { return kept[i].at.Before(kept[j].at) })
	for _, item := range kept[:len(kept)-duplicateTombstoneCap] {
		b.host.published.Delete(item.key)
	}
}

func localPolicyError(err error) bool {
	var typed *policyError
	if !errors.As(err, &typed) || typed == nil {
		return false
	}
	switch typed.reason {
	case "missing_policy_identity", "credential_unmapped", "missing_attempt", "admission_closed", "duplicate_result", "unknown_protocol", "protocol_mismatch":
		return true
	default:
		return false
	}
}

func providerOf(auth *coreauth.Auth) string {
	if auth == nil || auth.Attributes == nil {
		return ""
	}
	return strings.TrimSpace(auth.Attributes["ocg_provider_id"])
}

func capturedProvider(capture attemptCapture, auth *coreauth.Auth) string {
	if capture.providerID != "" {
		return capture.providerID
	}
	return providerOf(auth)
}

func copiedPins(pins []endpointPin) []coreauth.EndpointPin {
	if len(pins) == 0 {
		return nil
	}
	out := make([]coreauth.EndpointPin, len(pins))
	for i, pin := range pins {
		out[i] = coreauth.EndpointPin{
			Protocol:            pin.Protocol,
			EndpointID:          pin.EndpointID,
			Origin:              pin.Origin,
			EndpointFingerprint: pin.EndpointFingerprint,
			HTTPMethod:          pin.HTTPMethod,
		}
	}
	return out
}

func wireEndpointPin(pin *coreauth.EndpointPin) *endpointPin {
	if pin == nil {
		return nil
	}
	return &endpointPin{
		Protocol:            pin.Protocol,
		EndpointID:          pin.EndpointID,
		Origin:              pin.Origin,
		EndpointFingerprint: pin.EndpointFingerprint,
		HTTPMethod:          pin.HTTPMethod,
	}
}

func boolPtr(value bool) *bool { return &value }

func statusPointer(status int, hasStatus bool) *int {
	if !hasStatus {
		return nil
	}
	value := status
	return &value
}

var acceptedErrorCodes = []string{
	"none",
	"validation",
	"transport",
	"provider_rejected",
	"body_lost",
	"stream_lost",
	"cancelled",
	"deadline",
	"usage_observation",
	"parser",
	"unknown",
}
