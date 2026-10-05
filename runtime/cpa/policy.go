package main

import (
	"bytes"
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

const (
	policyMaxRequest  = 128 << 10
	policyMaxResponse = 16 << 10
	policyTimeout     = 2 * time.Second
	maxEvidenceBody   = 64 << 10
	maxUsageTokens    = 50_000_000
)

type policyObservation struct {
	ID        string `json:"id"`
	FetchedAt string `json:"fetchedAt"`
}

type policyRequest struct {
	ProtocolVersion    int                `json:"protocolVersion"`
	Operation          string             `json:"operation"`
	ProcessGeneration  string             `json:"processGeneration"`
	ProjectionRevision string             `json:"projectionRevision"`
	ProjectionDigest   string             `json:"projectionDigest"`
	RequestID          string             `json:"requestId,omitempty"`
	AttemptID          string             `json:"attemptId,omitempty"`
	AuthID             string             `json:"authId,omitempty"`
	CredentialID       string             `json:"credentialId,omitempty"`
	CredentialVersion  string             `json:"credentialVersion,omitempty"`
	ProviderID         string             `json:"providerId,omitempty"`
	PublicModel        string             `json:"publicModel,omitempty"`
	UpstreamModel      string             `json:"upstreamModel,omitempty"`
	RegistrationEpoch  string             `json:"registrationEpoch,omitempty"`
	MaterialRevision   string             `json:"materialRevision,omitempty"`
	Kind               string             `json:"kind,omitempty"`
	CallableProtocol   string             `json:"callableProtocol,omitempty"`
	GenerationKind     string             `json:"generationKind,omitempty"`
	EndpointPin        *endpointPin       `json:"endpointPin,omitempty"`
	Sent               *bool              `json:"sent,omitempty"`
	Status             *int               `json:"status,omitempty"`
	BodyComplete       *bool              `json:"bodyComplete,omitempty"`
	StreamStarted      *bool              `json:"streamStarted,omitempty"`
	Outcome            string             `json:"outcome,omitempty"`
	ErrorCode          string             `json:"errorCode,omitempty"`
	ResponseBody       string             `json:"responseBody,omitempty"`
	Headers            map[string]string  `json:"headers,omitempty"`
	ReportedUsage      json.RawMessage    `json:"reportedUsage,omitempty"`
	Observation        *policyObservation `json:"observation,omitempty"`
	includeResult      bool               `json:"-"`
}

func (r policyRequest) MarshalJSON() ([]byte, error) {
	obj := map[string]any{
		"protocolVersion":    r.ProtocolVersion,
		"operation":          r.Operation,
		"processGeneration":  r.ProcessGeneration,
		"projectionRevision": r.ProjectionRevision,
		"projectionDigest":   r.ProjectionDigest,
	}
	if r.Operation != "ready" {
		obj["requestId"] = r.RequestID
		obj["attemptId"] = r.AttemptID
		obj["authId"] = r.AuthID
		obj["credentialId"] = r.CredentialID
		obj["credentialVersion"] = r.CredentialVersion
		obj["providerId"] = r.ProviderID
		obj["publicModel"] = r.PublicModel
		obj["upstreamModel"] = r.UpstreamModel
		obj["registrationEpoch"] = r.RegistrationEpoch
		obj["materialRevision"] = r.MaterialRevision
		obj["kind"] = r.Kind
	}
	if r.Operation == "admit" {
		obj["callableProtocol"] = r.CallableProtocol
		obj["generationKind"] = r.GenerationKind
	}
	if r.Operation == "result" || r.includeResult {
		obj["sent"] = boolValue(r.Sent)
		if r.Status == nil {
			obj["status"] = nil
		} else {
			obj["status"] = *r.Status
		}
		obj["bodyComplete"] = boolValue(r.BodyComplete)
		obj["streamStarted"] = boolValue(r.StreamStarted)
		obj["outcome"] = r.Outcome
		obj["errorCode"] = r.ErrorCode
		obj["responseBody"] = r.ResponseBody
		if len(r.Headers) > 0 {
			obj["headers"] = r.Headers
		}
		if len(r.ReportedUsage) > 0 && string(r.ReportedUsage) != "null" {
			obj["reportedUsage"] = json.RawMessage(r.ReportedUsage)
		}
		if r.Observation != nil {
			obj["observation"] = r.Observation
		}
		if r.EndpointPin == nil {
			obj["endpointPin"] = nil
		} else {
			obj["endpointPin"] = r.EndpointPin
		}
	}
	return json.Marshal(obj)
}

type endpointPin struct {
	Protocol            string `json:"protocol"`
	EndpointID          string `json:"endpointId"`
	Origin              string `json:"origin"`
	EndpointFingerprint string `json:"endpointFingerprint"`
	HTTPMethod          string `json:"httpMethod"`
}

type policyResponse struct {
	Action              string
	Reason              string
	RestrictionDeadline string
	Unavailable         bool
	EndpointPins        []endpointPin
}

type policyError struct{ reason string }

func (e *policyError) Error() string {
	if e == nil || e.reason == "" {
		return "policy_failure"
	}
	return e.reason
}

func errPolicy(reason string) error { return &policyError{reason: reason} }

func newPolicyClient() *http.Client {
	return &http.Client{
		Timeout:       policyTimeout,
		CheckRedirect: refuseRedirect,
		Transport:     &http.Transport{Proxy: nil},
	}
}

func (h *Host) postPolicy(ctx context.Context, request policyRequest, detach bool) (policyResponse, error) {
	if h == nil {
		return policyResponse{}, errPolicy("policy_unavailable")
	}
	payload, err := json.Marshal(request)
	if err != nil {
		return policyResponse{}, errPolicy("policy_encode")
	}
	if err := validatePolicyDocument(payload); err != nil {
		return policyResponse{}, errPolicy("policy_encode")
	}
	if len(payload) > policyMaxRequest {
		return policyResponse{}, errPolicy("policy_request_too_large")
	}
	h.mu.RLock()
	endpoint := h.policyURL
	token := h.policyToken
	origin := h.policyOrigin
	h.mu.RUnlock()
	if endpoint == "" || token == "" || origin == "" {
		return policyResponse{}, errPolicy("policy_unconfigured")
	}
	parent := ctx
	if parent == nil {
		parent = context.Background()
	}
	if request.Operation == "result" && detach {
		parent = context.Background()
	}
	callCtx, cancel := context.WithTimeout(parent, policyTimeout)
	defer cancel()
	httpRequest, err := http.NewRequestWithContext(callCtx, http.MethodPost, endpoint, bytes.NewReader(payload))
	if err != nil {
		return policyResponse{}, errPolicy("policy_request")
	}
	httpRequest.Header.Set("Content-Type", "application/json")
	httpRequest.Header.Set("Authorization", "Bearer "+token)
	httpRequest.Header.Set("Origin", origin)
	response, err := h.policyClient.Do(httpRequest)
	if err != nil {
		return policyResponse{}, errPolicy("policy_transport")
	}
	defer response.Body.Close()
	body, err := io.ReadAll(io.LimitReader(response.Body, policyMaxResponse+1))
	if err != nil {
		return policyResponse{}, errPolicy("policy_read")
	}
	if len(body) > policyMaxResponse || response.StatusCode != http.StatusOK {
		return policyResponse{}, errPolicy("policy_response")
	}
	if request.Operation == "ready" {
		return decodeReady(body, request)
	}
	decoded, err := decodeDecision(body)
	if err != nil {
		return policyResponse{}, err
	}
	if request.Operation == "result" && decoded.Action == "allow" {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	if decoded.Unavailable {
		decoded.Action = "stop"
	}
	return decoded, nil
}

func decodeReady(body []byte, expect policyRequest) (policyResponse, error) {
	raw, err := objectFields(body)
	if err != nil {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	if _, bad := raw["action"]; bad {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	for _, key := range []string{"protocolVersion", "policyReady", "processGeneration", "projectionRevision", "projectionDigest", "reason", "unavailable", "endpointPins"} {
		if _, ok := raw[key]; !ok {
			return policyResponse{}, errPolicy("policy_malformed")
		}
	}
	if !exactKeys(raw, "protocolVersion", "policyReady", "processGeneration", "projectionRevision", "projectionDigest", "reason", "unavailable", "endpointPins") {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	if string(bytes.TrimSpace(raw["endpointPins"])) != "null" {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	if !rawNumberIs(raw["protocolVersion"], 1) || !rawBoolIs(raw["policyReady"], true) || !rawBoolIs(raw["unavailable"], false) {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	generation, okGen := rawString(raw["processGeneration"])
	revision, okRev := rawString(raw["projectionRevision"])
	digest, okDigest := rawString(raw["projectionDigest"])
	if !okGen || !okRev || !okDigest || generation != expect.ProcessGeneration || revision != expect.ProjectionRevision || !strings.EqualFold(digest, expect.ProjectionDigest) {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	return policyResponse{Action: "allow", Reason: "ready"}, nil
}

func decodeDecision(body []byte) (policyResponse, error) {
	raw, err := objectFields(body)
	if err != nil {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	for _, key := range []string{"protocolVersion", "action", "reason", "restrictionDeadline", "unavailable", "endpointPins"} {
		if _, ok := raw[key]; !ok {
			return policyResponse{}, errPolicy("policy_malformed")
		}
	}
	if !exactKeys(raw, "protocolVersion", "action", "reason", "restrictionDeadline", "unavailable", "endpointPins") {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	if !rawNumberIs(raw["protocolVersion"], 1) {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	action, okAction := rawString(raw["action"])
	reason, okReason := rawString(raw["reason"])
	if !okAction || !okReason {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	switch action {
	case "allow", "skip", "stop":
	default:
		return policyResponse{}, errPolicy("policy_malformed")
	}
	deadline, err := rawDeadline(raw["restrictionDeadline"])
	if err != nil {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	unavailable, okBool := rawBool(raw["unavailable"])
	if !okBool {
		return policyResponse{}, errPolicy("policy_malformed")
	}
	pins, err := decodePinVector(raw["endpointPins"], action)
	if err != nil {
		return policyResponse{}, err
	}
	return policyResponse{Action: action, Reason: reason, RestrictionDeadline: deadline, Unavailable: unavailable, EndpointPins: pins}, nil
}

func validatePolicyDocument(body []byte) error {
	if len(body) > policyMaxRequest {
		return errPolicy("policy_request_too_large")
	}
	raw, err := objectFields(body)
	if err != nil {
		return err
	}
	if !rawNumberIs(raw["protocolVersion"], 1) {
		return errPolicy("policy_malformed")
	}
	operation, ok := rawString(raw["operation"])
	if !ok {
		return errPolicy("policy_malformed")
	}
	for _, key := range []string{"processGeneration", "projectionRevision", "projectionDigest"} {
		if _, present := raw[key]; !present {
			return errPolicy("policy_malformed")
		}
	}
	if _, err := rawDecimal(raw["processGeneration"]); err != nil {
		return err
	}
	if _, err := rawDecimal(raw["projectionRevision"]); err != nil {
		return err
	}
	digest, okDigest := rawString(raw["projectionDigest"])
	if !okDigest || len(digest) != 64 || !hexText(digest) {
		return errPolicy("policy_malformed")
	}
	allowed := map[string]bool{
		"protocolVersion": true, "operation": true, "processGeneration": true,
		"projectionRevision": true, "projectionDigest": true,
	}
	attemptPresent := false
	for _, key := range attemptKeys {
		if _, present := raw[key]; present {
			attemptPresent = true
		}
	}
	switch operation {
	case "ready":
		if attemptPresent {
			return errPolicy("policy_malformed")
		}
	case "admit", "result":
		for _, key := range attemptKeys {
			allowed[key] = true
			if _, present := raw[key]; !present {
				return errPolicy("policy_malformed")
			}
		}
		if err := validateAttempt(raw); err != nil {
			return err
		}
	default:
		return errPolicy("policy_malformed")
	}
	if operation == "admit" {
		for _, key := range []string{"callableProtocol", "generationKind"} {
			allowed[key] = true
			if _, present := raw[key]; !present {
				return errPolicy("policy_malformed")
			}
		}
		if err := validateAdmitOperation(raw); err != nil {
			return err
		}
	}
	if operation == "result" {
		for _, key := range []string{"sent", "status", "bodyComplete", "streamStarted", "outcome", "errorCode", "responseBody", "endpointPin"} {
			allowed[key] = true
			if _, present := raw[key]; !present {
				return errPolicy("policy_malformed")
			}
		}
		for _, key := range []string{"headers", "reportedUsage", "observation"} {
			allowed[key] = true
		}
		if err := validateResult(raw); err != nil {
			return err
		}
		if err := validateOptionalPin(raw["endpointPin"]); err != nil {
			return err
		}
	}
	for key := range raw {
		if !allowed[key] {
			return errPolicy("policy_malformed")
		}
	}
	return nil
}

var attemptKeys = []string{
	"requestId", "attemptId", "authId", "credentialId", "credentialVersion", "providerId",
	"publicModel", "upstreamModel", "registrationEpoch", "materialRevision", "kind",
}

func validateAttempt(raw map[string]json.RawMessage) error {
	requestID, okReq := rawString(raw["requestId"])
	attemptID, okAttempt := rawString(raw["attemptId"])
	if !okReq || !okAttempt || !canonicalUUID(requestID) || !canonicalUUID(attemptID) {
		return errPolicy("policy_malformed")
	}
	for _, key := range []string{"authId", "credentialId", "providerId", "materialRevision"} {
		text, ok := rawString(raw[key])
		if !ok || text == "" || len(text) > 128 {
			return errPolicy("policy_malformed")
		}
	}
	for _, key := range []string{"publicModel", "upstreamModel"} {
		text, ok := rawString(raw[key])
		if !ok || text == "" || len(text) > 256 {
			return errPolicy("policy_malformed")
		}
	}
	if _, err := rawDecimal(raw["credentialVersion"]); err != nil {
		return err
	}
	if _, err := rawDecimal(raw["registrationEpoch"]); err != nil {
		return err
	}
	kind, okKind := rawString(raw["kind"])
	if !okKind || (kind != "accepted" && kind != "validated") {
		return errPolicy("policy_malformed")
	}
	return nil
}

func validateAdmitOperation(raw map[string]json.RawMessage) error {
	protocol, okProtocol := rawString(raw["callableProtocol"])
	if !okProtocol || !approvedProtocol(protocol) {
		return errPolicy("policy_malformed")
	}
	kind, okKind := rawString(raw["generationKind"])
	switch kind {
	case "execute", "refresh-resend", "stream", "stream-refresh", "stream-bootstrap", "internal", "count-tokens":
		if okKind {
			return nil
		}
	}
	return errPolicy("policy_malformed")
}

func validateOptionalPin(raw json.RawMessage) error {
	if string(bytes.TrimSpace(raw)) == "null" {
		return nil
	}
	_, err := parsePinObject(raw)
	return err
}

func decodePinVector(raw json.RawMessage, action string) ([]endpointPin, error) {
	trimmed := bytes.TrimSpace(raw)
	if string(trimmed) == "null" {
		return nil, nil
	}
	if action != "allow" || len(trimmed) == 0 || trimmed[0] != '[' {
		return nil, errPolicy("policy_malformed")
	}
	var items []json.RawMessage
	if err := json.Unmarshal(trimmed, &items); err != nil || len(items) == 0 || len(items) > 32 {
		return nil, errPolicy("policy_malformed")
	}
	pins := make([]endpointPin, 0, len(items))
	seenID := map[string]struct{}{}
	seenFingerprint := map[string]struct{}{}
	for _, item := range items {
		pin, err := parsePinObject(item)
		if err != nil {
			return nil, err
		}
		if _, ok := seenID[pin.EndpointID]; ok {
			return nil, errPolicy("policy_malformed")
		}
		if _, ok := seenFingerprint[pin.EndpointFingerprint]; ok {
			return nil, errPolicy("policy_malformed")
		}
		seenID[pin.EndpointID] = struct{}{}
		seenFingerprint[pin.EndpointFingerprint] = struct{}{}
		pins = append(pins, pin)
	}
	return pins, nil
}

func parsePinObject(raw json.RawMessage) (endpointPin, error) {
	fields, err := objectFields(raw)
	if err != nil || !exactKeys(fields, "protocol", "endpointId", "origin", "endpointFingerprint", "httpMethod") {
		return endpointPin{}, errPolicy("policy_malformed")
	}
	protocol, okProtocol := rawString(fields["protocol"])
	id, okID := rawString(fields["endpointId"])
	origin, okOrigin := rawString(fields["origin"])
	fingerprint, okFingerprint := rawString(fields["endpointFingerprint"])
	method, okMethod := rawString(fields["httpMethod"])
	if !okProtocol || !okID || !okOrigin || !okFingerprint || !okMethod || !approvedProtocol(protocol) || !validPinID(id) || !validPinOrigin(origin) || !validFingerprint(fingerprint) || method != "POST" {
		return endpointPin{}, errPolicy("policy_malformed")
	}
	return endpointPin{Protocol: protocol, EndpointID: id, Origin: origin, EndpointFingerprint: fingerprint, HTTPMethod: method}, nil
}

func validPinID(value string) bool {
	if value == "" || len(value) > 128 || strings.TrimSpace(value) != value {
		return false
	}
	return !strings.ContainsAny(value, " \t\r\n")
}

func validFingerprint(value string) bool {
	return len(value) == 64 && value == strings.ToLower(value) && hexText(value)
}

func validPinOrigin(raw string) bool {
	if raw == "" || len(raw) > 128 {
		return false
	}
	parsed, err := url.Parse(raw)
	if err != nil || parsed.User != nil || parsed.Fragment != "" || parsed.RawQuery != "" || parsed.Opaque != "" || parsed.Host == "" {
		return false
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return false
	}
	return parsed.Path == "" || parsed.Path == "/"
}

func validateResult(raw map[string]json.RawMessage) error {
	if _, ok := rawBool(raw["sent"]); !ok {
		return errPolicy("policy_malformed")
	}
	if _, ok := rawBool(raw["bodyComplete"]); !ok {
		return errPolicy("policy_malformed")
	}
	if _, ok := rawBool(raw["streamStarted"]); !ok {
		return errPolicy("policy_malformed")
	}
	if err := rawStatus(raw["status"]); err != nil {
		return err
	}
	outcome, okOutcome := rawString(raw["outcome"])
	if !okOutcome || !knownOutcome(outcome) {
		return errPolicy("policy_malformed")
	}
	code, okCode := rawString(raw["errorCode"])
	if !okCode || !knownErrorCode(code) {
		return errPolicy("policy_malformed")
	}
	body, okBody := rawString(raw["responseBody"])
	if !okBody || len(body) > maxEvidenceBody {
		return errPolicy("policy_malformed")
	}
	if err := rawHeaders(raw["headers"]); err != nil {
		return err
	}
	if err := rawUsage(raw["reportedUsage"]); err != nil {
		return err
	}
	return rawObservation(raw["observation"])
}

func knownOutcome(value string) bool {
	switch value {
	case "success", "explicit_rejection", "uncertain", "cancelled", "deadline", "local_failure":
		return true
	default:
		return false
	}
}

func knownErrorCode(value string) bool {
	for _, code := range acceptedErrorCodes {
		if code == value {
			return true
		}
	}
	return false
}

func objectFields(body []byte) (map[string]json.RawMessage, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(body, &raw); err != nil || raw == nil {
		return nil, errPolicy("policy_malformed")
	}
	return raw, nil
}

func exactKeys(raw map[string]json.RawMessage, keys ...string) bool {
	if len(raw) != len(keys) {
		return false
	}
	for _, key := range keys {
		if _, ok := raw[key]; !ok {
			return false
		}
	}
	return true
}

func rawString(value json.RawMessage) (string, bool) {
	if len(value) == 0 {
		return "", false
	}
	var text string
	if err := json.Unmarshal(value, &text); err != nil {
		return "", false
	}
	return text, true
}

func rawBool(value json.RawMessage) (bool, bool) {
	if string(value) == "true" {
		return true, true
	}
	if string(value) == "false" {
		return false, true
	}
	return false, false
}

func rawBoolIs(value json.RawMessage, want bool) bool {
	got, ok := rawBool(value)
	return ok && got == want
}

func rawNumberIs(value json.RawMessage, want int) bool {
	var number int
	if err := json.Unmarshal(value, &number); err != nil {
		return false
	}
	return number == want
}

func rawDecimal(value json.RawMessage) (uint64, error) {
	text, ok := rawString(value)
	if !ok || text == "" || len(text) > 20 {
		return 0, errPolicy("policy_malformed")
	}
	for _, char := range text {
		if char < '0' || char > '9' {
			return 0, errPolicy("policy_malformed")
		}
	}
	if len(text) > 1 && text[0] == '0' {
		return 0, errPolicy("policy_malformed")
	}
	parsed, err := strconv.ParseUint(text, 10, 64)
	if err != nil {
		return 0, errPolicy("policy_malformed")
	}
	return parsed, nil
}

func rawStatus(value json.RawMessage) error {
	if string(value) == "null" {
		return nil
	}
	var status uint64
	if err := json.Unmarshal(value, &status); err != nil || status > 599 {
		return errPolicy("policy_malformed")
	}
	return nil
}

func rawDeadline(value json.RawMessage) (string, error) {
	if string(value) == "null" {
		return "", nil
	}
	text, ok := rawString(value)
	if !ok {
		return "", errPolicy("policy_malformed")
	}
	if _, err := time.Parse(time.RFC3339, text); err != nil {
		return "", err
	}
	return text, nil
}

func rawHeaders(value json.RawMessage) error {
	if len(value) == 0 || string(value) == "null" {
		return nil
	}
	var headers map[string]string
	if err := json.Unmarshal(value, &headers); err != nil || len(headers) > 8 {
		return errPolicy("policy_malformed")
	}
	for name, header := range headers {
		if len(name) > 128 || len(header) > 128 {
			return errPolicy("policy_malformed")
		}
	}
	return nil
}

func rawUsage(value json.RawMessage) error {
	if len(value) == 0 || string(value) == "null" {
		return nil
	}
	var usage map[string]json.RawMessage
	if err := json.Unmarshal(value, &usage); err != nil {
		return errPolicy("policy_malformed")
	}
	for key, token := range usage {
		if key != "inputTokens" && key != "outputTokens" {
			return errPolicy("policy_malformed")
		}
		if _, ok := usageUint(token); !ok {
			return errPolicy("policy_malformed")
		}
	}
	return nil
}

func rawObservation(value json.RawMessage) error {
	if len(value) == 0 || string(value) == "null" {
		return nil
	}
	var observation map[string]json.RawMessage
	if err := json.Unmarshal(value, &observation); err != nil {
		return errPolicy("policy_malformed")
	}
	if len(observation) != 2 {
		return errPolicy("policy_malformed")
	}
	id, okID := rawString(observation["id"])
	fetched, okFetched := rawString(observation["fetchedAt"])
	if !okID || !okFetched || id == "" || len(id) > 128 {
		return errPolicy("policy_malformed")
	}
	if _, err := time.Parse(time.RFC3339, fetched); err != nil {
		return errPolicy("policy_malformed")
	}
	return nil
}

func hexText(value string) bool {
	for _, char := range value {
		switch {
		case char >= '0' && char <= '9':
		case char >= 'a' && char <= 'f':
		case char >= 'A' && char <= 'F':
		default:
			return false
		}
	}
	return true
}

func canonicalUUID(value string) bool {
	if len(value) != 36 {
		return false
	}
	for i := 0; i < len(value); i++ {
		char := value[i]
		if i == 8 || i == 13 || i == 18 || i == 23 {
			if char != '-' {
				return false
			}
			continue
		}
		switch {
		case char >= '0' && char <= '9':
		case char >= 'a' && char <= 'f':
		case char >= 'A' && char <= 'F':
		default:
			return false
		}
	}
	return true
}

func decimalEpoch(value uint64) string {
	return strconv.FormatUint(value, 10)
}

func wireSendKind(sdkKind string) string {
	if sdkKind == "validated" {
		return "validated"
	}
	return "accepted"
}

func boolValue(value *bool) bool {
	return value != nil && *value
}

func canonicalReportedUsage(raw []byte) json.RawMessage {
	if len(raw) == 0 || string(raw) == "null" || !json.Valid(raw) {
		return nil
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(raw, &fields); err != nil {
		return nil
	}
	out := map[string]uint64{}
	if number, ok := firstUsageUint(fields, "inputTokens", "input_tokens", "prompt_tokens"); ok {
		out["inputTokens"] = number
	}
	if number, ok := firstUsageUint(fields, "outputTokens", "output_tokens", "completion_tokens"); ok {
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

func firstUsageUint(fields map[string]json.RawMessage, keys ...string) (uint64, bool) {
	for _, key := range keys {
		if number, ok := usageUint(fields[key]); ok {
			return number, true
		}
	}
	return 0, false
}

func usageUint(value json.RawMessage) (uint64, bool) {
	if len(value) == 0 || strings.ContainsAny(string(value), ".eE") {
		return 0, false
	}
	var number uint64
	if err := json.Unmarshal(value, &number); err != nil || number > maxUsageTokens {
		return 0, false
	}
	return number, true
}

func tokenEqual(got, want string) bool {
	if want == "" {
		return false
	}
	gotBytes := []byte(got)
	wantBytes := []byte(want)
	if len(gotBytes) != len(wantBytes) {
		dummy := make([]byte, len(gotBytes))
		subtle.ConstantTimeCompare(gotBytes, dummy)
		return false
	}
	return subtle.ConstantTimeCompare(gotBytes, wantBytes) == 1
}

func bearerToken(header string) string {
	header = strings.TrimSpace(header)
	if len(header) < len("Bearer ") || !strings.EqualFold(header[:len("Bearer ")], "Bearer ") {
		return ""
	}
	return strings.TrimSpace(header[len("Bearer "):])
}

func wrapPolicy(err error) error {
	if err == nil {
		return nil
	}
	var typed *policyError
	if errors.As(err, &typed) {
		return err
	}
	return fmt.Errorf("policy: %w", err)
}
