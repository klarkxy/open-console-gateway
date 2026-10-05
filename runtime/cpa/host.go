package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gin-gonic/gin"
	sdkapi "github.com/router-for-me/CLIProxyAPI/v8/sdk/api"
	sdkhandlers "github.com/router-for-me/CLIProxyAPI/v8/sdk/api/handlers"
	"github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy"
	coreauth "github.com/router-for-me/CLIProxyAPI/v8/sdk/cliproxy/auth"
	"github.com/router-for-me/CLIProxyAPI/v8/sdk/config"
)

const sourceCommit = "6fecc6e5567912661654a4eaf9b8f5436facd1c2"

// Host reads one OCG-written CPA config and serves the private hop.
type Host struct {
	mu                sync.RWMutex
	policyURL         string
	policyToken       string
	policyOrigin      string
	policyClient      *http.Client
	unavailable       atomic.Bool
	policyReady       atomic.Bool
	processGeneration string
	appliedRevision   atomic.Value
	appliedDigest     atomic.Value
	applyStatus       atomic.Value
	desiredRevision   string
	desiredDigest     string
	service           *cliproxy.Service
	cancel            context.CancelFunc
	configPath        string
	authDir           string
	listenURL         string
	port              int
	hopSecret         string
	readyToken        string
	document          ocgDocument
	previousCfg       *config.Config
	stickyGlobal      bool
	reloadMu          sync.Mutex
	applyBarrier      atomic.Bool
	captures          sync.Map
	published         sync.Map
}

func (h *Host) revisionText() string {
	if h == nil {
		return ""
	}
	value, _ := h.appliedRevision.Load().(string)
	return value
}

func (h *Host) digestText() string {
	if h == nil {
		return ""
	}
	value, _ := h.appliedDigest.Load().(string)
	return value
}

func (h *Host) statusText() string {
	if h == nil {
		return ""
	}
	value, _ := h.applyStatus.Load().(string)
	return value
}

func (h *Host) URL() string {
	if h == nil {
		return ""
	}
	return h.listenURL
}

func (h *Host) AuthDir() string {
	if h == nil {
		return ""
	}
	return h.authDir
}

func (h *Host) ProcessGeneration() string {
	if h == nil {
		return ""
	}
	return h.processGeneration
}

// Start reads the private config, applies it in memory, and serves loopback HTTP.
func Start(ctx context.Context, configPath string) (*Host, error) {
	if strings.TrimSpace(configPath) == "" {
		return nil, fmt.Errorf("config path is required")
	}
	_, doc, cfg, digest, err := loadPrivateConfig(configPath)
	if err != nil {
		return nil, err
	}
	if err := validatePrivateConfig(doc, cfg); err != nil {
		return nil, err
	}
	applyInMemory(cfg, doc)
	if err := os.MkdirAll(cfg.AuthDir, 0o700); err != nil {
		return nil, err
	}
	host := &Host{
		policyURL:         doc.Policy.URL,
		policyToken:       doc.Policy.Token,
		policyOrigin:      doc.Policy.Origin,
		policyClient:      newPolicyClient(),
		processGeneration: doc.ProcessGeneration.String(),
		configPath:        configPath,
		authDir:           cfg.AuthDir,
		listenURL:         "http://127.0.0.1:" + strconv.Itoa(cfg.Port),
		port:              cfg.Port,
		hopSecret:         cfg.APIKeys[0],
		readyToken:        doc.ReadyKey,
		document:          doc,
		previousCfg:       cfg,
		stickyGlobal:      doc.Routing.StickyGlobal,
		desiredRevision:   doc.ProjectionRevision.String(),
		desiredDigest:     digest,
	}
	host.appliedRevision.Store("")
	host.appliedDigest.Store("")
	host.applyStatus.Store("starting")
	gin.SetMode(gin.ReleaseMode)
	service, err := cliproxy.NewBuilder().
		WithConfig(cfg).
		WithConfigPath(configPath).
		WithLocalManagementPassword(os.Getenv("MANAGEMENT_PASSWORD")).
		WithGenerationBoundary(generationBoundary{host: host}).
		WithServerOptions(
			sdkapi.WithMiddleware(host.gate()),
			sdkapi.WithRouterConfigurator(func(engine *gin.Engine, _ *sdkhandlers.BaseAPIHandler, _ *config.Config) {
				engine.GET("/_internal/ocg/ready", host.ready)
			}),
		).
		Build()
	if err != nil {
		return nil, err
	}
	host.service = service
	service.SetConfigFileReloader(func(path string) error {
		_, err := host.Reload(context.Background(), path)
		return err
	})
	host.reloadMu.Lock()
	defer host.reloadMu.Unlock()
	runCtx, cancel := context.WithCancel(ctx)
	host.cancel = cancel
	errCh := make(chan error, 1)
	go func() {
		errCh <- service.Run(runCtx)
	}()
	if err := waitListen(cfg.Port, errCh); err != nil {
		cancel()
		return nil, err
	}
	if err := host.stamp(); err != nil {
		cancel()
		return nil, err
	}
	if _, err := host.postPolicy(runCtx, policyRequest{
		ProtocolVersion:    1,
		Operation:          "ready",
		ProcessGeneration:  host.processGeneration,
		ProjectionRevision: doc.ProjectionRevision.String(),
		ProjectionDigest:   digest,
	}, true); err != nil {
		cancel()
		host.applyStatus.Store("apply_failure")
		return nil, err
	}
	host.appliedRevision.Store(doc.ProjectionRevision.String())
	host.appliedDigest.Store(digest)
	host.applyStatus.Store("applied")
	host.policyReady.Store(true)
	go host.watchConfigFile(runCtx)
	return host, nil
}

// watchConfigFile re-reads the config OCG replaced. On Windows, replacing a
// watched file with MoveFileEx does not deliver an fsnotify event, so the
// stock watcher never reaches Reload.
func (h *Host) watchConfigFile(ctx context.Context) {
	if h == nil {
		return
	}
	last := fileDigest(h.configPathText())
	ticker := time.NewTicker(200 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			path := h.configPathText()
			sum := fileDigest(path)
			if sum == "" || sum == last {
				continue
			}
			last = sum
			_, _ = h.Reload(context.Background(), path)
		}
	}
}

func (h *Host) configPathText() string {
	if h == nil {
		return ""
	}
	h.mu.RLock()
	defer h.mu.RUnlock()
	return h.configPath
}

func fileDigest(path string) string {
	raw, err := os.ReadFile(path)
	if err != nil || len(raw) == 0 {
		return ""
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:])
}

func (h *Host) Close() {
	if h == nil || h.cancel == nil {
		return
	}
	h.cancel()
}

// Reload reads a config OCG already wrote. It does not create, rewrite, or remove that file.
func (h *Host) Reload(ctx context.Context, configPath string) (string, error) {
	if h == nil || h.service == nil {
		return "apply_failure", fmt.Errorf("host is not running")
	}
	h.reloadMu.Lock()
	defer h.reloadMu.Unlock()
	if strings.TrimSpace(configPath) == "" {
		configPath = h.configPath
	}
	_, doc, cfg, digest, err := loadPrivateConfig(configPath)
	if err != nil {
		// A file that does not parse leaves the live generation in place. Startup
		// auth refresh re-reads this path, and an invalid replace must not downgrade
		// an already applied projection.
		if h.policyReady.Load() && h.digestText() != "" {
			return h.statusText(), err
		}
		h.applyStatus.Store("save_failure")
		return "save_failure", err
	}
	if err := validatePrivateConfig(doc, cfg); err != nil {
		status := "unsupported"
		if strings.Contains(err.Error(), "self_loop") {
			status = "apply_failure"
		}
		h.applyStatus.Store(status)
		return status, err
	}
	if digest == h.digestText() && h.policyReady.Load() && h.revisionText() == doc.ProjectionRevision.String() {
		h.applyStatus.Store("applied")
		return "applied", nil
	}
	snap := h.captureSnap()
	h.mu.Lock()
	h.desiredRevision = doc.ProjectionRevision.String()
	h.desiredDigest = digest
	h.mu.Unlock()
	applyInMemory(cfg, doc)
	// Ready runs against the previous applied routes. The candidate is installed
	// only after ready accepts, and ingress stays closed until that revision is committed.
	if _, err := h.postPolicy(ctx, policyRequest{
		ProtocolVersion:    1,
		Operation:          "ready",
		ProcessGeneration:  doc.ProcessGeneration.String(),
		ProjectionRevision: doc.ProjectionRevision.String(),
		ProjectionDigest:   digest,
	}, true); err != nil {
		return h.rollbackApply(ctx, snap, doc, cfg)
	}
	h.applyBarrier.Store(true)
	defer h.applyBarrier.Store(false)
	if !h.service.ApplyRuntimeConfig(ctx, cfg) {
		return h.rollbackApply(ctx, snap, doc, cfg)
	}
	h.stage(doc, cfg, configPath)
	if err := h.stamp(); err != nil {
		return h.rollbackApply(ctx, snap, doc, cfg)
	}
	h.commitApply(doc, cfg, configPath, digest)
	return "applied", nil
}

type hostSnap struct {
	document          ocgDocument
	previousCfg       *config.Config
	policyURL         string
	policyToken       string
	policyOrigin      string
	hopSecret         string
	readyToken        string
	stickyGlobal      bool
	configPath        string
	authDir           string
	processGeneration string
	desiredRevision   string
	desiredDigest     string
	policyReady       bool
}

func (h *Host) captureSnap() hostSnap {
	h.mu.RLock()
	defer h.mu.RUnlock()
	return hostSnap{
		document:          h.document,
		previousCfg:       h.previousCfg,
		policyURL:         h.policyURL,
		policyToken:       h.policyToken,
		policyOrigin:      h.policyOrigin,
		hopSecret:         h.hopSecret,
		readyToken:        h.readyToken,
		stickyGlobal:      h.stickyGlobal,
		configPath:        h.configPath,
		authDir:           h.authDir,
		processGeneration: h.processGeneration,
		desiredRevision:   h.desiredRevision,
		desiredDigest:     h.desiredDigest,
		policyReady:       h.policyReady.Load(),
	}
}

func (h *Host) stage(doc ocgDocument, cfg *config.Config, configPath string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.document = doc
	h.previousCfg = cfg
	h.policyURL = doc.Policy.URL
	h.policyToken = doc.Policy.Token
	h.policyOrigin = doc.Policy.Origin
	if cfg != nil && len(cfg.APIKeys) > 0 {
		h.hopSecret = cfg.APIKeys[0]
	}
	h.readyToken = doc.ReadyKey
	h.stickyGlobal = doc.Routing.StickyGlobal
	h.configPath = configPath
	if cfg != nil {
		h.authDir = cfg.AuthDir
	}
}

func (h *Host) restoreSnap(snap hostSnap) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.document = snap.document
	h.previousCfg = snap.previousCfg
	h.policyURL = snap.policyURL
	h.policyToken = snap.policyToken
	h.policyOrigin = snap.policyOrigin
	h.hopSecret = snap.hopSecret
	h.readyToken = snap.readyToken
	h.stickyGlobal = snap.stickyGlobal
	h.configPath = snap.configPath
	h.authDir = snap.authDir
	h.processGeneration = snap.processGeneration
	h.desiredRevision = snap.desiredRevision
	h.desiredDigest = snap.desiredDigest
}

func (h *Host) commitApply(doc ocgDocument, cfg *config.Config, configPath, digest string) {
	h.stage(doc, cfg, configPath)
	h.mu.Lock()
	h.processGeneration = doc.ProcessGeneration.String()
	h.mu.Unlock()
	revision := doc.ProjectionRevision.String()
	h.appliedRevision.Store(revision)
	h.appliedDigest.Store(digest)
	h.applyStatus.Store("applied")
	h.policyReady.Store(true)
	h.unavailable.Store(false)
}

func (h *Host) rollbackApply(ctx context.Context, snap hostSnap, doc ocgDocument, cfg *config.Config) (string, error) {
	if secretsRotated(snap.document, doc, snap.previousCfg, cfg) {
		h.restoreSnap(snap)
		h.mu.Lock()
		h.desiredRevision = doc.ProjectionRevision.String()
		h.mu.Unlock()
		h.clearCredentialMappings()
		h.unavailable.Store(true)
		h.policyReady.Store(false)
		h.applyStatus.Store("apply_failure")
		return "apply_failure", fmt.Errorf("rotated credentials were not revived")
	}
	if snap.previousCfg != nil {
		_ = h.service.ApplyRuntimeConfig(ctx, snap.previousCfg)
	}
	h.restoreSnap(snap)
	if err := h.stamp(); err != nil {
		h.unavailable.Store(true)
		h.policyReady.Store(false)
		h.applyStatus.Store("apply_failure")
		return "apply_failure", err
	}
	h.policyReady.Store(snap.policyReady)
	h.unavailable.Store(false)
	h.applyStatus.Store("rollback")
	return "rollback", fmt.Errorf("rollback")
}

func secretsRotated(prevDoc, nextDoc ocgDocument, prevCfg, nextCfg *config.Config) bool {
	prevKeys := compatKeyMap(prevCfg)
	nextKeys := compatKeyMap(nextCfg)
	for name, key := range prevKeys {
		next, ok := nextKeys[name]
		if !ok || next != key {
			return true
		}
	}
	prevBindings := bindingFenceMap(prevDoc)
	nextBindings := bindingFenceMap(nextDoc)
	for id, version := range prevBindings {
		next, ok := nextBindings[id]
		if !ok || next != version {
			return true
		}
	}
	return false
}

func compatKeyMap(cfg *config.Config) map[string]string {
	out := map[string]string{}
	if cfg == nil {
		return out
	}
	for _, entry := range cfg.OpenAICompatibility {
		name := strings.TrimSpace(entry.Name)
		key := ""
		if len(entry.APIKeyEntries) > 0 {
			key = entry.APIKeyEntries[0].APIKey
		}
		out[name] = key
	}
	return out
}

func bindingFenceMap(doc ocgDocument) map[string]string {
	out := map[string]string{}
	for _, binding := range doc.OAuthBindings {
		if binding.CredentialID == "" {
			continue
		}
		out[binding.CredentialID] = binding.CredentialVersion.String()
	}
	for _, credential := range doc.Credentials {
		if credential.CredentialID == "" {
			continue
		}
		out[credential.CredentialID] = credential.CredentialVersion.String()
	}
	return out
}

func (h *Host) clearCredentialMappings() {
	manager := h.service.CoreAuthManager()
	if manager == nil {
		return
	}
	for _, auth := range manager.List() {
		if auth == nil {
			continue
		}
		clone := auth.Clone()
		if clone.Metadata != nil {
			delete(clone.Metadata, "ocg_credential_id")
			delete(clone.Metadata, "ocg_credential_version")
			delete(clone.Metadata, "ocg_binding_id")
		}
		if clone.Attributes != nil {
			delete(clone.Attributes, "ocg_provider_id")
			delete(clone.Attributes, "priority")
			delete(clone.Attributes, coreauth.AttributeFilePriority)
		}
		_, _ = manager.Update(context.Background(), clone)
	}
}

func (h *Host) gate() gin.HandlerFunc {
	return func(c *gin.Context) {
		if c.Request == nil || c.Request.URL == nil {
			c.AbortWithStatus(http.StatusBadRequest)
			return
		}
		path := c.Request.URL.Path
		if ownedManagementDenied(c.Request.Method, path) {
			c.AbortWithStatusJSON(http.StatusForbidden, gin.H{"error": "owned_projection"})
			return
		}
		if !policyIngress(c.Request.Method, path) {
			c.Next()
			return
		}
		if h.applyBarrier.Load() || !h.policyReady.Load() || h.unavailable.Load() {
			c.AbortWithStatusJSON(http.StatusServiceUnavailable, gin.H{"error": "execution_unavailable"})
			return
		}
		requestID := strings.TrimSpace(c.GetHeader("X-OCG-Request-Id"))
		generation := strings.TrimSpace(c.GetHeader("X-OCG-Process-Generation"))
		revision := strings.TrimSpace(c.GetHeader("X-OCG-Projection-Revision"))
		if !canonicalUUID(requestID) || generation == "" || revision == "" {
			c.AbortWithStatusJSON(http.StatusBadRequest, gin.H{"error": "missing_policy_identity"})
			return
		}
		if generation != h.ProcessGeneration() || revision != h.revisionText() {
			c.AbortWithStatusJSON(http.StatusConflict, gin.H{"error": "projection_mismatch"})
			return
		}
		kind, pin, protocol, bridgeErr := privateBridge(
			c.GetHeader("X-OCG-Request-Kind"),
			c.GetHeader("X-OCG-Pinned-Auth-Id"),
			c.GetHeader("X-OCG-Validated-Protocol"),
			path,
		)
		if bridgeErr != nil || (kind == "validated" && !h.hasAppliedAuth(pin)) {
			c.AbortWithStatusJSON(http.StatusBadRequest, gin.H{"error": "invalid_private_bridge"})
			return
		}
		deadline, deadlineErr := parseRequestDeadline(c.GetHeader("X-OCG-Request-Deadline"), time.Now())
		if deadlineErr != nil {
			c.AbortWithStatusJSON(http.StatusBadRequest, gin.H{"error": "invalid_request_deadline"})
			return
		}
		stripPrivateOCG(c.Request.Header)
		h.mu.RLock()
		sticky := h.stickyGlobal
		h.mu.RUnlock()
		if sticky && strings.TrimSpace(c.GetHeader("X-Session-ID")) == "" {
			c.Request.Header.Set("X-Session-ID", "ocg-sticky-global")
		}
		ctx, stop := bridgeContext(c.Request.Context(), requestID, kind, pin, protocol, deadline)
		if kind == "validated" {
			ctx = coreauth.WithOCGCallableProtocol(ctx, protocol)
		} else {
			ctx = coreauth.WithOCGCallableProtocol(ctx, ingressCallableProtocol(path))
		}
		defer stop()
		c.Request = c.Request.WithContext(ctx)
		c.Next()
	}
}

// ownedManagementDenied rejects product config, key, and route writers on the
// owned host before the management handler can create a temp file or apply runtime config.
// OAuth, credential-file, import, refresh, and logout operations stay on their handlers.
func ownedManagementDenied(method, path string) bool {
	if path != "/v0/management" && !strings.HasPrefix(path, "/v0/management/") && path != "/v8/management" && !strings.HasPrefix(path, "/v8/management/") {
		return false
	}
	switch method {
	case http.MethodGet, http.MethodHead, http.MethodOptions:
		return false
	}
	switch method + " " + path {
	case http.MethodPost + " /v0/management/oauth-callback",
		http.MethodDelete + " /v0/management/oauth-session",
		http.MethodPost + " /v0/management/vertex/import",
		http.MethodPost + " /v0/management/auth-files",
		http.MethodDelete + " /v0/management/auth-files",
		http.MethodPatch + " /v0/management/auth-files/status",
		http.MethodPatch + " /v0/management/auth-files/fields",
		http.MethodPost + " /v0/management/auth-files/refresh",
		http.MethodPost + " /v8/management/oauth/callback",
		http.MethodDelete + " /v8/management/oauth/session",
		http.MethodPost + " /v8/management/oauth/import",
		http.MethodPost + " /v8/management/credentials",
		http.MethodDelete + " /v8/management/credentials",
		http.MethodPatch + " /v8/management/credentials/status",
		http.MethodPatch + " /v8/management/credentials/fields",
		http.MethodPost + " /v8/management/credentials/refresh":
		return false
	default:
		return true
	}
}

// policyIngress is the CPA route inventory that can start a generation send.
// /v1/ covers the OpenAI, Claude, and realtime handlers. Gemini generate, stream,
// and count live under /v1beta. Codex aliases and video creates sit outside /v1/.
func policyIngress(method, path string) bool {
	if path == "/_internal/ocg/ready" {
		return false
	}
	if method == http.MethodGet && cataloguePath(path) {
		return false
	}
	if strings.HasPrefix(path, "/v1/") || strings.HasPrefix(path, "/openai/v1/") || strings.HasPrefix(path, "/backend-api/codex/") {
		return true
	}
	if path == "/v1beta/interactions" {
		return true
	}
	if strings.HasPrefix(path, "/v1beta/models/") {
		return strings.Contains(path, ":streamGenerateContent") || strings.Contains(path, ":generateContent") || strings.Contains(path, ":countTokens")
	}
	return false
}

func cataloguePath(path string) bool {
	switch path {
	case "/v1/models", "/v1/models/", "/openai/v1/models", "/openai/v1/models/":
		return true
	default:
		return false
	}
}

func (h *Host) ready(c *gin.Context) {
	if c.Request == nil || !loopback(c.Request.RemoteAddr) {
		c.AbortWithStatus(http.StatusForbidden)
		return
	}
	if !tokenEqual(c.GetHeader("X-OCG-Ready-Token"), h.readyToken) {
		c.AbortWithStatus(http.StatusUnauthorized)
		return
	}
	if strings.TrimSpace(c.GetHeader("Origin")) != h.policyOrigin {
		c.AbortWithStatus(http.StatusForbidden)
		return
	}
	var caps capabilityDocument
	_ = json.Unmarshal(capabilityBytes, &caps)
	h.mu.RLock()
	desiredRevision := h.desiredRevision
	desiredDigest := h.desiredDigest
	h.mu.RUnlock()
	body := readyDocument{
		ProtocolVersion: 1,
		Artifact: artifactDocument{
			SourceCommit:     sourceCommit,
			ProtocolVersion:  caps.ProtocolVersion,
			Capabilities:     caps.Capabilities,
			ExecutableSHA256: executableSHA256(),
		},
		ProcessGeneration:         h.ProcessGeneration(),
		AppliedProjectionRevision: h.revisionText(),
		AppliedProjectionDigest:   h.digestText(),
		DesiredProjectionRevision: desiredRevision,
		DesiredProjectionDigest:   desiredDigest,
		PolicyReady:               h.policyReady.Load(),
		ApplyStatus:               h.statusText(),
		AuthRefs:                  h.authRefs(),
	}
	c.Header("Cache-Control", "no-store")
	c.JSON(http.StatusOK, body)
}

func (h *Host) stamp() error {
	manager := h.service.CoreAuthManager()
	if manager == nil {
		return fmt.Errorf("core auth manager is unavailable")
	}
	deadline := time.Now().Add(8 * time.Second)
	for {
		if h.stampOnce(manager) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("credentials were not registered")
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func (h *Host) stampOnce(manager *coreauth.Manager) bool {
	h.mu.RLock()
	doc := h.document
	cfg := h.previousCfg
	authDir := h.authDir
	h.mu.RUnlock()
	found := 0
	listedOAuth := 0
	matchedBindings := map[int]struct{}{}
	for _, auth := range manager.List() {
		if auth == nil {
			continue
		}
		name := ""
		if auth.Attributes != nil {
			name = auth.Attributes["compat_name"]
		}
		var credential *ocgCredential
		for i := range doc.Credentials {
			if doc.Credentials[i].Namespace == name {
				credential = &doc.Credentials[i]
				break
			}
		}
		if credential == nil {
			if !oauthAuth(auth) {
				continue
			}
			listedOAuth++
			if index, ok := h.stampOAuth(manager, auth, doc.OAuthBindings, cfg); ok {
				matchedBindings[index] = struct{}{}
			}
			continue
		}
		clone := auth.Clone()
		if clone.Metadata == nil {
			clone.Metadata = map[string]any{}
		}
		clone.Metadata["ocg_credential_id"] = credential.CredentialID
		clone.Metadata["ocg_credential_version"] = credential.CredentialVersion.String()
		clone.Metadata["ocg_material_revision"] = credential.MaterialRevision
		clone.Metadata["ocg_binding_id"] = credential.BindingID
		clone.Metadata["ocg_oauth"] = false
		if clone.Attributes == nil {
			clone.Attributes = map[string]string{}
		}
		clone.Attributes["ocg_provider_id"] = credential.ProviderID
		clone.Attributes["websockets"] = "false"
		if doc.Routing.StickyGlobal {
			clone.Attributes["header:X-Session-ID"] = "ocg-sticky-global"
		}
		if raw, err := json.Marshal(stampedRoutes(credential.Routes)); err == nil {
			clone.Attributes["ocg_protocol_routes"] = string(raw)
		}
		applyProxyAttributes(clone, doc, cfg)
		if _, err := manager.Update(context.Background(), clone); err != nil {
			return false
		}
		found++
	}
	if len(doc.Credentials) > 0 && found != len(doc.Credentials) {
		return false
	}
	pendingJSON := countAuthJSON(authDir)
	if len(doc.Credentials) == 0 && len(doc.OAuthBindings) == 0 && pendingJSON == 0 {
		return true
	}
	if len(doc.OAuthBindings) == 0 {
		if len(doc.Credentials) == 0 && listedOAuth == 0 {
			return false
		}
		return len(doc.Credentials) == 0 || found == len(doc.Credentials)
	}
	if listedOAuth == 0 || len(matchedBindings) != len(doc.OAuthBindings) {
		return false
	}
	return true
}

func stampedRoutes(routes []ocgRoute) []stampedRoute {
	out := make([]stampedRoute, 0, len(routes))
	for _, route := range routes {
		out = append(out, stampedRoute{
			Protocol:       route.Protocol,
			Endpoint:       route.Endpoint,
			AuthScheme:     route.AuthScheme,
			UpstreamModel:  route.UpstreamModel,
			PublicAlias:    route.PublicModel,
			Identity:       route.RequestIdentity,
			Wire:           route.Wire,
			ValidationOnly: route.ValidationOnly,
		})
	}
	return out
}

func (h *Host) stampOAuth(manager *coreauth.Manager, auth *coreauth.Auth, bindings []OAuthBinding, cfg *config.Config) (int, bool) {
	if auth == nil || !oauthAuth(auth) {
		return 0, false
	}
	h.mu.RLock()
	doc := h.document
	authDir := h.authDir
	h.mu.RUnlock()
	material := oauthMaterial(auth)
	clone := auth.Clone()
	if clone.Metadata == nil {
		clone.Metadata = map[string]any{}
	}
	if clone.Attributes == nil {
		clone.Attributes = map[string]string{}
	}
	clone.Attributes["websockets"] = "false"
	clone.Metadata["ocg_oauth"] = true
	if material != "" {
		clone.Metadata["ocg_material_revision"] = material
	}
	if raw, _ := clone.Metadata["base_url"].(string); loopbackHTTP(raw) && strings.TrimSpace(clone.Attributes["base_url"]) == "" {
		clone.Attributes["base_url"] = strings.TrimSpace(raw)
	}
	// Present empty values so a later file reload cannot restore a cleared stamp.
	clone.Metadata["ocg_credential_id"] = ""
	clone.Metadata["ocg_credential_version"] = ""
	clone.Metadata["ocg_binding_id"] = ""
	clone.Attributes["ocg_provider_id"] = ""
	clone.Attributes["priority"] = ""
	clone.Attributes["excluded_models"] = ""
	clone.Attributes["ocg_model_scope"] = ""
	delete(clone.Attributes, coreauth.AttributeFilePriority)
	rel := relativeAuthPath(authDir, auth.FileName)
	matched := -1
	for i, binding := range bindings {
		if !bindingMatches(binding, auth.ID, rel) {
			continue
		}
		clone.Metadata["ocg_credential_id"] = binding.CredentialID
		clone.Metadata["ocg_credential_version"] = binding.CredentialVersion.String()
		clone.Metadata["ocg_binding_id"] = binding.RelativePath
		clone.Attributes["ocg_provider_id"] = binding.ProviderID
		clone.Attributes["priority"] = binding.Priority.String()
		delete(clone.Attributes, coreauth.AttributeFilePriority)
		coreauth.OCGScopeAuthModels(clone, binding.Models)
		applyProxyAttributes(clone, doc, cfg)
		if strings.TrimSpace(clone.ProxyURL) == "" && cfg != nil && strings.TrimSpace(cfg.ProxyURL) != "" {
			clone.ProxyURL = strings.TrimSpace(cfg.ProxyURL)
		}
		matched = i
		break
	}
	if _, err := manager.Update(context.Background(), clone); err != nil {
		return 0, false
	}
	manager.RefreshSchedulerEntry(clone.ID)
	if matched >= 0 {
		return matched, true
	}
	return 0, false
}

func (h *Host) authRefs() []authRef {
	if h == nil || h.service == nil || h.service.CoreAuthManager() == nil {
		return nil
	}
	h.mu.RLock()
	doc := h.document
	h.mu.RUnlock()
	refs := make([]authRef, 0)
	for _, auth := range h.service.CoreAuthManager().List() {
		if auth == nil {
			continue
		}
		provider := providerOf(auth)
		if provider == "" {
			provider = strings.TrimSpace(auth.Provider)
		}
		ref := authRef{
			RelativePath:      relativeAuthPath(h.authDir, auth.FileName),
			AuthID:            auth.ID,
			Provider:          provider,
			ProviderID:        provider,
			Upstream:          strings.TrimSpace(auth.Provider),
			RegistrationEpoch: strconv.FormatUint(auth.RegistrationEpoch, 10),
			MaterialRevision:  metadataText(auth, "ocg_material_revision"),
			Disabled:          auth.Disabled || auth.Status == coreauth.StatusDisabled,
			Status:            string(auth.Status),
			Mapped:            metadataText(auth, "ocg_credential_id") != "",
			CredentialID:      metadataText(auth, "ocg_credential_id"),
			CredentialVersion: metadataText(auth, "ocg_credential_version"),
		}
		name := ""
		if auth.Attributes != nil {
			name = auth.Attributes["compat_name"]
		}
		for _, credential := range doc.Credentials {
			if credential.Namespace != name {
				continue
			}
			if ref.Provider == "" {
				ref.Provider = credential.ProviderID
			}
			ref.OpaqueRemote = credential.OpaqueRemote
			if credential.OpaqueRemote {
				ref.Quota = "unknown"
				ref.InnerIdentity = "unknown"
			}
			for _, route := range credential.Routes {
				ref.Models = append(ref.Models, route.PublicModel)
			}
		}
		matchedBinding := false
		if oauthAuth(auth) && len(ref.Models) == 0 {
			for _, binding := range doc.OAuthBindings {
				if bindingMatches(binding, auth.ID, ref.RelativePath) {
					ref.Models = append(ref.Models, binding.Models...)
					if binding.ProviderID != "" {
						ref.Provider = binding.ProviderID
						ref.ProviderID = binding.ProviderID
					}
					matchedBinding = true
					break
				}
			}
		}
		if oauthAuth(auth) && len(ref.Models) == 0 && !matchedBinding {
			ref.Models = append(ref.Models, coreauth.OCGRegisteredModelIDs(auth.ID)...)
		}
		if matchedBinding && len(ref.Models) == 0 {
			ref.Models = []string{}
		}
		if oauthAuth(auth) {
			facts := nativeEffectiveFacts(auth)
			ref.RawProviderLabel = facts.label
			ref.EffectiveSubtype = facts.subtype
			ref.EffectiveMode = facts.mode
			ref.EffectiveGenerationBase = facts.base
			ref.EffectiveAuthKind = facts.authKind
		}
		refs = append(refs, ref)
	}
	return refs
}

func bindingMatches(binding OAuthBinding, authID, relativePath string) bool {
	if strings.TrimSpace(binding.AuthID) == "" && strings.TrimSpace(binding.RelativePath) == "" {
		return false
	}
	if binding.AuthID != "" && binding.AuthID != authID {
		return false
	}
	if binding.RelativePath != "" && filepath.ToSlash(binding.RelativePath) != filepath.ToSlash(relativePath) {
		return false
	}
	return binding.CredentialID != "" && binding.CredentialVersion.String() != "" && binding.Priority.String() != ""
}

func oauthAuth(auth *coreauth.Auth) bool {
	if auth == nil || auth.Metadata == nil {
		return false
	}
	for _, key := range []string{"refresh_token", "refreshToken", "access_token", "accessToken"} {
		if value, _ := auth.Metadata[key].(string); strings.TrimSpace(value) != "" {
			return true
		}
	}
	return false
}

func oauthMaterial(auth *coreauth.Auth) string {
	if auth == nil {
		return ""
	}
	return coreauth.OCGMaterialRevision(auth.Metadata)
}

func applyProxyAttributes(auth *coreauth.Auth, doc ocgDocument, cfg *config.Config) {
	if auth == nil {
		return
	}
	if auth.Attributes == nil {
		auth.Attributes = map[string]string{}
	}
	if doc.ProxyList == nil {
		return
	}
	auth.Attributes["ocg_proxy_direction"] = strings.ToLower(strings.TrimSpace(doc.ProxyList.Direction))
	auth.Attributes["ocg_proxy_models"] = strings.Join(doc.ProxyList.Models, ",")
	auth.Attributes["ocg_proxy_url"] = strings.TrimSpace(doc.ProxyList.ProxyURL)
	_ = cfg
}

func countAuthJSON(dir string) int {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return 0
	}
	count := 0
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		if strings.HasSuffix(strings.ToLower(entry.Name()), ".json") {
			count++
		}
	}
	return count
}

func loopbackHTTP(raw string) bool {
	parsed, err := url.Parse(strings.TrimSpace(raw))
	if err != nil {
		return false
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return false
	}
	host := parsed.Hostname()
	return host == "127.0.0.1" || host == "localhost"
}

func metadataText(auth *coreauth.Auth, key string) string {
	if auth == nil || auth.Metadata == nil {
		return ""
	}
	value, _ := auth.Metadata[key].(string)
	return value
}

func relativeAuthPath(authDir, fileName string) string {
	fileName = strings.TrimSpace(fileName)
	if fileName == "" {
		return ""
	}
	slash := filepath.ToSlash(fileName)
	if strings.Contains(slash, "..") {
		return ""
	}
	if !filepath.IsAbs(fileName) {
		if strings.Contains(slash, ":") {
			return ""
		}
		return slash
	}
	if strings.TrimSpace(authDir) == "" {
		return ""
	}
	rel, err := filepath.Rel(authDir, fileName)
	if err != nil {
		return ""
	}
	rel = filepath.ToSlash(rel)
	if rel == ".." || strings.HasPrefix(rel, "../") || filepath.IsAbs(rel) || strings.Contains(rel, ":") {
		return ""
	}
	return rel
}

func loopback(remote string) bool {
	host, _, err := net.SplitHostPort(remote)
	if err != nil {
		host = remote
	}
	return host == "127.0.0.1" || host == "::1" || host == "localhost"
}

func refuseRedirect(*http.Request, []*http.Request) error {
	return http.ErrUseLastResponse
}

func waitListen(port int, errCh <-chan error) error {
	address := "127.0.0.1:" + strconv.Itoa(port)
	deadline := time.Now().Add(25 * time.Second)
	for time.Now().Before(deadline) {
		select {
		case err := <-errCh:
			if err != nil {
				return err
			}
		default:
		}
		conn, err := net.DialTimeout("tcp", address, 200*time.Millisecond)
		if err == nil {
			_ = conn.Close()
			return nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	return fmt.Errorf("CPA listener did not open")
}

func executableSHA256() string {
	path, err := os.Executable()
	if err != nil {
		return ""
	}
	file, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer file.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return ""
	}
	return hex.EncodeToString(hash.Sum(nil))
}

type capabilityDocument struct {
	ProtocolVersion int      `json:"protocolVersion"`
	Capabilities    []string `json:"capabilities"`
}

type artifactDocument struct {
	SourceCommit     string   `json:"sourceCommit"`
	ProtocolVersion  int      `json:"protocolVersion"`
	Capabilities     []string `json:"capabilities"`
	ExecutableSHA256 string   `json:"executableSHA256"`
}

type readyDocument struct {
	ProtocolVersion           int              `json:"protocolVersion"`
	Artifact                  artifactDocument `json:"artifact"`
	ProcessGeneration         string           `json:"processGeneration"`
	AppliedProjectionRevision string           `json:"appliedProjectionRevision"`
	AppliedProjectionDigest   string           `json:"appliedProjectionDigest"`
	DesiredProjectionRevision string           `json:"desiredProjectionRevision"`
	DesiredProjectionDigest   string           `json:"desiredProjectionDigest"`
	PolicyReady               bool             `json:"policyReady"`
	ApplyStatus               string           `json:"applyStatus"`
	AuthRefs                  []authRef        `json:"authRefs"`
}

type authRef struct {
	RelativePath            string   `json:"relativePath,omitempty"`
	AuthID                  string   `json:"authId"`
	Provider                string   `json:"provider,omitempty"`
	ProviderID              string   `json:"providerId,omitempty"`
	Upstream                string   `json:"upstream,omitempty"`
	RegistrationEpoch       string   `json:"registrationEpoch,omitempty"`
	Disabled                bool     `json:"disabled"`
	Status                  string   `json:"status"`
	Models                  []string `json:"models"`
	MaterialRevision        string   `json:"materialRevision,omitempty"`
	Mapped                  bool     `json:"mapped"`
	CredentialID            string   `json:"credentialId,omitempty"`
	CredentialVersion       string   `json:"credentialVersion,omitempty"`
	OpaqueRemote            bool     `json:"opaqueRemote,omitempty"`
	Quota                   string   `json:"quota,omitempty"`
	InnerIdentity           string   `json:"innerIdentity,omitempty"`
	RawProviderLabel        string   `json:"rawProviderLabel,omitempty"`
	EffectiveSubtype        string   `json:"effectiveSubtype,omitempty"`
	EffectiveMode           string   `json:"effectiveMode,omitempty"`
	EffectiveGenerationBase string   `json:"effectiveGenerationBase,omitempty"`
	EffectiveAuthKind       string   `json:"effectiveAuthKind,omitempty"`
}

const (
	codexDefaultBase     = "https://chatgpt.com/backend-api/codex"
	claudeDefaultBase    = "https://api.anthropic.com"
	antigravityDailyBase = "https://daily-cloudcode-pa.googleapis.com"
	kimiComCodingBase    = "https://api.kimi.com/coding"
	kimiAICodingBase     = "https://api.kimi.ai/coding"
	xaiDefaultAPIBase    = "https://api.x.ai/v1"
	xaiCLIChatProxyBase  = "https://cli-chat-proxy.grok.com/v1"
)

type nativeFactSet struct {
	label, subtype, mode, base, authKind string
}

func nativeEffectiveFacts(auth *coreauth.Auth) nativeFactSet {
	if auth == nil {
		return nativeFactSet{}
	}
	typeName := strings.ToLower(strings.TrimSpace(metadataText(auth, "type")))
	provider := strings.ToLower(strings.TrimSpace(auth.Provider))
	kind := resolvedAuthKind(auth)
	switch {
	case typeName == "claude" || provider == "claude":
		return nativeFactSet{label: "claude", subtype: "anthropic", mode: "claude", base: claudeGenerationBase(auth), authKind: kind}
	case typeName == "codex" || provider == "codex":
		return nativeFactSet{label: "codex", subtype: "codex", base: codexGenerationBase(auth), authKind: kind}
	case typeName == "xai" || provider == "xai":
		usingAPI := xaiUsingAPIResolved(auth)
		mode := "cli"
		if usingAPI {
			mode = "api"
		}
		return nativeFactSet{label: "xai", subtype: "xai", mode: mode, base: xaiGenerationBase(auth, usingAPI), authKind: kind}
	case typeName == "antigravity" || provider == "antigravity":
		mode, base := antigravityGenerationFacts(auth)
		return nativeFactSet{label: "antigravity", subtype: "antigravity", mode: mode, base: base, authKind: kind}
	case kimiFamily(typeName, provider):
		subtype := kimiEffectiveSubtype(auth)
		return nativeFactSet{label: firstNonEmpty(typeName, provider), subtype: subtype, base: kimiGenerationBase(auth, subtype), authKind: kind}
	default:
		return nativeFactSet{}
	}
}

func resolvedAuthKind(auth *coreauth.Auth) string {
	if raw := strings.TrimSpace(attributeText(auth, "auth_kind")); raw != "" {
		return strings.ToLower(raw)
	}
	if raw := strings.TrimSpace(metadataText(auth, "auth_kind")); raw != "" {
		return strings.ToLower(raw)
	}
	if importedOAuthMaterial(auth) {
		return "oauth"
	}
	return ""
}

func importedOAuthMaterial(auth *coreauth.Auth) bool {
	if auth == nil || auth.Metadata == nil {
		return false
	}
	for _, key := range []string{"refresh_token", "refreshToken", "access_token", "accessToken"} {
		if value, _ := auth.Metadata[key].(string); strings.TrimSpace(value) != "" {
			return true
		}
	}
	return false
}

func claudeGenerationBase(auth *coreauth.Auth) string {
	base := strings.TrimSpace(attributeText(auth, "base_url"))
	if base == "" {
		return claudeDefaultBase
	}
	return strings.TrimRight(base, "/")
}

func codexGenerationBase(auth *coreauth.Auth) string {
	base := strings.TrimSpace(attributeText(auth, "base_url"))
	if base == "" {
		return codexDefaultBase
	}
	return strings.TrimRight(base, "/")
}

func xaiUsingAPIResolved(auth *coreauth.Auth) bool {
	if auth == nil {
		return true
	}
	if raw := strings.TrimSpace(attributeText(auth, "using_api")); raw != "" {
		parsed, err := strconv.ParseBool(raw)
		if err == nil {
			return parsed
		}
	}
	if auth.Metadata != nil {
		if raw, ok := auth.Metadata["using_api"]; ok && raw != nil {
			switch value := raw.(type) {
			case bool:
				return value
			case string:
				parsed, err := strconv.ParseBool(strings.TrimSpace(value))
				if err == nil {
					return parsed
				}
			}
		}
	}
	if raw := strings.TrimSpace(attributeText(auth, "auth_kind")); raw != "" {
		return !strings.EqualFold(raw, "oauth")
	}
	return !strings.EqualFold(strings.TrimSpace(metadataText(auth, "auth_kind")), "oauth")
}

func xaiGenerationBase(auth *coreauth.Auth, usingAPI bool) string {
	base := strings.TrimSpace(attributeText(auth, "base_url"))
	if base == "" {
		base = strings.TrimSpace(metadataText(auth, "base_url"))
	}
	if usingAPI {
		if base == "" {
			return xaiDefaultAPIBase
		}
		return strings.TrimRight(base, "/")
	}
	if base != "" && strings.TrimRight(base, "/") != xaiDefaultAPIBase {
		return strings.TrimRight(base, "/")
	}
	return xaiCLIChatProxyBase
}

func antigravityGenerationFacts(auth *coreauth.Auth) (string, string) {
	base := strings.TrimSpace(attributeText(auth, "base_url"))
	if base == "" {
		base = strings.TrimSpace(metadataText(auth, "base_url"))
	}
	base = strings.TrimRight(base, "/")
	if base == "" || strings.EqualFold(urlHost(base), "daily-cloudcode-pa.googleapis.com") {
		if base == "" {
			return "daily", antigravityDailyBase
		}
		return "daily", base
	}
	return "custom", base
}

func kimiGenerationBase(auth *coreauth.Auth, subtype string) string {
	base := strings.TrimRight(strings.TrimSpace(attributeText(auth, "base_url")), "/")
	if base == "" {
		base = strings.TrimRight(strings.TrimSpace(metadataText(auth, "base_url")), "/")
	}
	if base != "" {
		return base
	}
	switch subtype {
	case "kimi.ai":
		return kimiAICodingBase
	case "kimi.com":
		return kimiComCodingBase
	default:
		return ""
	}
}

func kimiFamily(typeName, provider string) bool {
	switch typeName {
	case "kimi", "kimi-ai", "kimi.ai", "kimi.com":
		return true
	}
	return provider == "kimi"
}

// kimiEffectiveSubtype follows the pinned SDK order for explicit attributes
// and metadata: attribute domain, attribute base host, metadata domain, then
// metadata base only when no earlier domain or host matched. A specific type
// that disagrees with that host signal stays unavailable. Bare type or domain
// "kimi" is not a .com assertion, so it does not compete with an explicit .ai.
// File name, auth ID, and display label are not authority. Generic type kimi
// with no specific signal defaults to kimi.com.
func kimiEffectiveSubtype(auth *coreauth.Auth) string {
	if auth == nil {
		return ""
	}
	hostSignal := ""
	earlierHost := false
	consider := func(signal string) {
		if hostSignal == "" && signal != "" {
			hostSignal = signal
		}
	}
	if signal := kimiTokenSignal(attributeText(auth, "domain")); signal != "" {
		consider(signal)
		earlierHost = true
	}
	if signal := kimiHostSignal(attributeText(auth, "base_url")); signal != "" {
		consider(signal)
		earlierHost = true
	}
	if signal := kimiTokenSignal(metadataText(auth, "domain")); signal != "" {
		consider(signal)
		earlierHost = true
	}
	if !earlierHost {
		consider(kimiHostSignal(metadataText(auth, "base_url")))
	}
	typeSignal := kimiTokenSignal(metadataText(auth, "type"))
	if hostSignal != "" && typeSignal != "" && hostSignal != typeSignal {
		return ""
	}
	if hostSignal != "" {
		return hostSignal
	}
	if typeSignal != "" {
		return typeSignal
	}
	if strings.EqualFold(strings.TrimSpace(metadataText(auth, "type")), "kimi") {
		return "kimi.com"
	}
	return ""
}

func kimiTokenSignal(value string) string {
	value = strings.ToLower(strings.TrimSpace(value))
	switch value {
	case "", "kimi":
		return ""
	case "kimi.ai", "ai", "kimi-ai":
		return "kimi.ai"
	case "kimi.com", "com":
		return "kimi.com"
	}
	switch {
	case strings.HasSuffix(value, ".kimi.ai"):
		return "kimi.ai"
	case strings.HasSuffix(value, ".kimi.com"):
		return "kimi.com"
	default:
		return ""
	}
}

func kimiHostSignal(raw string) string {
	host := strings.ToLower(strings.TrimSpace(urlHost(raw)))
	switch {
	case host == "kimi.ai" || host == "api.kimi.ai" || host == "auth.kimi.ai" || strings.HasSuffix(host, ".kimi.ai"):
		return "kimi.ai"
	case host == "kimi.com" || host == "api.kimi.com" || host == "auth.kimi.com" || strings.HasSuffix(host, ".kimi.com"):
		return "kimi.com"
	default:
		return ""
	}
}

func attributeText(auth *coreauth.Auth, key string) string {
	if auth == nil || auth.Attributes == nil {
		return ""
	}
	return auth.Attributes[key]
}

func urlHost(raw string) string {
	parsed, err := url.Parse(strings.TrimSpace(raw))
	if err != nil {
		return ""
	}
	return parsed.Hostname()
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return strings.TrimSpace(value)
		}
	}
	return ""
}
