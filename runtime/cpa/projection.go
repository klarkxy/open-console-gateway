package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"net/url"
	"os"
	"strconv"
	"strings"

	"github.com/router-for-me/CLIProxyAPI/v8/sdk/config"
	"gopkg.in/yaml.v3"
)

type ocgDocument struct {
	ProtocolVersion    int             `yaml:"protocol-version"`
	ProcessGeneration  decimalString   `yaml:"process-generation"`
	ProjectionRevision decimalString   `yaml:"projection-revision"`
	ReadyKey           string          `yaml:"ready-key"`
	Policy             ocgPolicy       `yaml:"policy"`
	Routing            ocgRouting      `yaml:"routing"`
	ProxyList          *ocgProxyList   `yaml:"proxy-list"`
	Credentials        []ocgCredential `yaml:"credentials"`
	OAuthBindings      []OAuthBinding  `yaml:"oauth-bindings"`
}

type ocgPolicy struct {
	URL    string `yaml:"url"`
	Token  string `yaml:"token"`
	Origin string `yaml:"origin"`
}

type ocgRouting struct {
	StickyGlobal       bool       `yaml:"sticky-global"`
	ConversationSticky bool       `yaml:"conversation-sticky"`
	ConversationTTL    decimalU64 `yaml:"conversation-ttl-seconds"`
}

type ocgProxyList struct {
	Direction string   `yaml:"direction"`
	Models    []string `yaml:"models"`
	ProxyURL  string   `yaml:"proxy-url"`
}

type ocgCredential struct {
	Namespace         string        `yaml:"namespace"`
	AuthID            string        `yaml:"auth-id"`
	CredentialID      string        `yaml:"credential-id"`
	CredentialVersion decimalString `yaml:"credential-version"`
	BindingID         string        `yaml:"binding-id"`
	MaterialRevision  string        `yaml:"material-revision"`
	ProviderID        string        `yaml:"provider-id"`
	OpaqueRemote      bool          `yaml:"opaque-remote"`
	Routes            []ocgRoute    `yaml:"routes"`
}

type ocgRoute struct {
	PublicModel     string `yaml:"public-model"`
	UpstreamModel   string `yaml:"upstream-model"`
	Protocol        string `yaml:"protocol"`
	Endpoint        string `yaml:"endpoint"`
	AuthScheme      string `yaml:"auth-scheme"`
	RequestIdentity string `yaml:"request-identity"`
	Wire            string `yaml:"wire"`
	ValidationOnly  bool   `yaml:"validation-only"`
}

// stampedRoute is the internal compat route JSON. validationOnly is a boolean
// and matches the YAML validation-only flag. Omitted YAML is false.
type stampedRoute struct {
	Protocol       string `json:"protocol"`
	Endpoint       string `json:"endpoint"`
	AuthScheme     string `json:"authScheme"`
	UpstreamModel  string `json:"upstreamModel"`
	PublicAlias    string `json:"publicAlias"`
	Identity       string `json:"identity"`
	Wire           string `json:"wire"`
	ValidationOnly bool   `json:"validationOnly"`
}

type OAuthBinding struct {
	RelativePath      string        `yaml:"relative-path" json:"relativePath,omitempty"`
	AuthID            string        `yaml:"auth-id" json:"authId,omitempty"`
	CredentialID      string        `yaml:"credential-id" json:"credentialId"`
	CredentialVersion decimalString `yaml:"credential-version" json:"credentialVersion"`
	MaterialRevision  string        `yaml:"material-revision" json:"materialRevision,omitempty"`
	ProviderID        string        `yaml:"provider-id" json:"providerId,omitempty"`
	Models            []string      `yaml:"models" json:"models,omitempty"`
	Priority          decimalString `yaml:"priority" json:"priority,omitempty"`
}

type decimalString struct {
	value string
}

func (d decimalString) String() string { return d.value }

func (d *decimalString) UnmarshalYAML(node *yaml.Node) error {
	value, err := decimalScalar(node)
	if err != nil {
		return err
	}
	d.value = value
	return nil
}

type decimalU64 uint64

func (d *decimalU64) UnmarshalYAML(node *yaml.Node) error {
	value, err := decimalScalar(node)
	if err != nil {
		return err
	}
	parsed, err := strconv.ParseUint(value, 10, 64)
	if err != nil {
		return err
	}
	*d = decimalU64(parsed)
	return nil
}

func decimalScalar(node *yaml.Node) (string, error) {
	if node == nil || node.Kind != yaml.ScalarNode {
		return "", fmt.Errorf("decimal scalar required")
	}
	if node.Tag == "!!float" {
		return "", fmt.Errorf("float version rejected")
	}
	value := strings.TrimSpace(node.Value)
	if value == "" || strings.ContainsAny(value, ".eE+") {
		return "", fmt.Errorf("float version rejected")
	}
	if _, err := strconv.ParseUint(value, 10, 64); err != nil {
		return "", fmt.Errorf("decimal version rejected")
	}
	return value, nil
}

func loadPrivateConfig(path string) ([]byte, ocgDocument, *config.Config, string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, ocgDocument{}, nil, "", err
	}
	var root yaml.Node
	if err := yaml.Unmarshal(raw, &root); err != nil {
		return nil, ocgDocument{}, nil, "", err
	}
	doc, err := decodeOCG(root)
	if err != nil {
		return nil, ocgDocument{}, nil, "", err
	}
	cfg, err := config.ParseConfigBytes(raw)
	if err != nil {
		return nil, ocgDocument{}, nil, "", err
	}
	sum := sha256.Sum256(raw)
	return raw, doc, cfg, hex.EncodeToString(sum[:]), nil
}

func decodeOCG(root yaml.Node) (ocgDocument, error) {
	mapping := root.Content
	if root.Kind == yaml.DocumentNode && len(root.Content) == 1 {
		mapping = root.Content[0].Content
		if root.Content[0].Kind != yaml.MappingNode {
			return ocgDocument{}, fmt.Errorf("config root must be a mapping")
		}
	}
	var ocgNode *yaml.Node
	for i := 0; i+1 < len(mapping); i += 2 {
		if mapping[i].Value == "ocg" {
			ocgNode = mapping[i+1]
			break
		}
	}
	if ocgNode == nil {
		return ocgDocument{}, fmt.Errorf("ocg root is required")
	}
	var doc ocgDocument
	if err := ocgNode.Decode(&doc); err != nil {
		return ocgDocument{}, err
	}
	return doc, nil
}

func validatePrivateConfig(doc ocgDocument, cfg *config.Config) error {
	if cfg == nil {
		return fmt.Errorf("config is required")
	}
	if doc.ProtocolVersion != 1 {
		return fmt.Errorf("unsupported protocol version")
	}
	if doc.ProcessGeneration.String() == "" || doc.ProjectionRevision.String() == "" || doc.ReadyKey == "" {
		return fmt.Errorf("process generation, projection revision, and ready key are required")
	}
	if doc.Policy.URL == "" || doc.Policy.Token == "" || doc.Policy.Origin == "" {
		return fmt.Errorf("policy url, token, and origin are required")
	}
	if _, err := url.ParseRequestURI(doc.Policy.URL); err != nil {
		return fmt.Errorf("policy url: %w", err)
	}
	if cfg.Host != "127.0.0.1" || cfg.Port <= 0 || cfg.Port > 65535 {
		return fmt.Errorf("listener must be 127.0.0.1 with a fixed port")
	}
	if strings.TrimSpace(cfg.AuthDir) == "" || strings.Contains(cfg.AuthDir, "..") {
		return fmt.Errorf("private auth-dir is required")
	}
	if len(cfg.APIKeys) != 1 || strings.TrimSpace(cfg.APIKeys[0]) == "" {
		return fmt.Errorf("exactly one private hop key is required")
	}
	if strings.TrimSpace(cfg.RemoteManagement.SecretKey) != "" || cfg.RemoteManagement.AllowRemote {
		return fmt.Errorf("remote management must stay closed")
	}
	switch strings.ToLower(strings.TrimSpace(cfg.Routing.Strategy)) {
	case "", "round-robin", "fill-first", "weighted-round-robin":
	default:
		return fmt.Errorf("unsupported_routing_strategy")
	}
	if err := validateProxyList(doc.ProxyList); err != nil {
		return err
	}
	if len(doc.Credentials) != len(cfg.OpenAICompatibility) {
		return fmt.Errorf("credential namespace mismatch")
	}
	seen := map[string]struct{}{}
	for _, credential := range doc.Credentials {
		if err := validateCredential(credential, cfg, seen); err != nil {
			return err
		}
	}
	for i := range cfg.OpenAICompatibility {
		entry := cfg.OpenAICompatibility[i]
		if _, ok := seen[strings.TrimSpace(entry.Name)]; !ok {
			return fmt.Errorf("openai-compatibility entry %s has no ocg credential", entry.Name)
		}
		if len(entry.APIKeyEntries) != 1 {
			return fmt.Errorf("credential %s must encode one upstream api key", entry.Name)
		}
		if strings.TrimSpace(entry.APIKeyEntries[0].APIKey) == "" && !credentialAllowsBlankKey(doc, entry.Name) {
			return fmt.Errorf("credential %s must encode one upstream api key", entry.Name)
		}
		if strings.TrimSpace(entry.APIKeyEntries[0].ProxyURL) != "" {
			if err := validateProxyURL(entry.APIKeyEntries[0].ProxyURL); err != nil {
				return err
			}
		}
		if len(entry.Models) == 0 {
			return fmt.Errorf("credential %s requires models", entry.Name)
		}
		for _, model := range entry.Models {
			if strings.TrimSpace(model.Name) == "" || strings.TrimSpace(model.Alias) == "" {
				return fmt.Errorf("credential %s model name and alias are required", entry.Name)
			}
		}
	}
	for _, binding := range doc.OAuthBindings {
		if err := validateOAuthBinding(binding); err != nil {
			return err
		}
	}
	return nil
}

func validateCredential(credential ocgCredential, cfg *config.Config, seen map[string]struct{}) error {
	namespace := strings.TrimSpace(credential.Namespace)
	if namespace == "" || credential.AuthID != namespace || credential.CredentialID == "" || credential.CredentialVersion.String() == "" || credential.BindingID == "" || credential.MaterialRevision == "" || credential.ProviderID == "" {
		return fmt.Errorf("credential namespace, auth id, version, binding, material, and provider are required")
	}
	if credential.ProviderID == namespace {
		return fmt.Errorf("provider id must be separate from the cpa namespace")
	}
	if _, ok := seen[namespace]; ok {
		return fmt.Errorf("duplicate credential namespace %s", namespace)
	}
	seen[namespace] = struct{}{}
	entry := compatByName(cfg, namespace)
	if entry == nil {
		return fmt.Errorf("credential %s is missing its openai-compatibility entry", namespace)
	}
	if err := rejectSelfLoop(entry.BaseURL, cfg.Host, cfg.Port); err != nil {
		return err
	}
	if len(credential.Routes) == 0 {
		return fmt.Errorf("credential %s requires routes", namespace)
	}
	for _, route := range credential.Routes {
		if err := validateRoute(route, cfg.Host, cfg.Port); err != nil {
			return fmt.Errorf("credential %s: %w", namespace, err)
		}
	}
	return nil
}

func validateRoute(route ocgRoute, listenHost string, listenPort int) error {
	switch strings.ToLower(strings.TrimSpace(route.Protocol)) {
	case "chat_completions", "messages", "responses":
	default:
		return fmt.Errorf("unsupported protocol")
	}
	switch strings.ToLower(strings.TrimSpace(route.AuthScheme)) {
	case "bearer", "x-api-key", "api-key", "none":
	default:
		return fmt.Errorf("unsupported auth scheme")
	}
	switch strings.ToLower(strings.TrimSpace(route.RequestIdentity)) {
	case "", "none", "opencode-session", "opencode-zen":
	default:
		return fmt.Errorf("unsupported request identity")
	}
	switch strings.ToLower(strings.TrimSpace(route.Wire)) {
	case "", "none", "ollama-reasoning":
	default:
		return fmt.Errorf("unsupported wire")
	}
	if strings.TrimSpace(route.PublicModel) == "" || strings.TrimSpace(route.UpstreamModel) == "" {
		return fmt.Errorf("route models are required")
	}
	parsed, err := url.ParseRequestURI(strings.TrimSpace(route.Endpoint))
	if err != nil || (parsed.Scheme != "http" && parsed.Scheme != "https") {
		return fmt.Errorf("route endpoint must be an absolute url")
	}
	return rejectSelfLoop(route.Endpoint, listenHost, listenPort)
}

func validateOAuthBinding(binding OAuthBinding) error {
	if strings.TrimSpace(binding.AuthID) == "" && strings.TrimSpace(binding.RelativePath) == "" {
		return fmt.Errorf("oauth binding requires auth id or relative path")
	}
	if binding.CredentialID == "" || binding.CredentialVersion.String() == "" || binding.ProviderID == "" {
		return fmt.Errorf("oauth binding id, version, and provider are required")
	}
	priority, err := strconv.ParseUint(binding.Priority.String(), 10, 64)
	if err != nil || priority == 0 {
		return fmt.Errorf("oauth binding priority must be a positive integer")
	}
	rel := strings.TrimSpace(binding.RelativePath)
	if rel != "" {
		if strings.Contains(rel, "..") || strings.HasPrefix(rel, "/") || strings.Contains(rel, ":\\") || strings.Contains(rel, ":/") {
			return fmt.Errorf("oauth relative path must stay inside auth-dir")
		}
	}
	return nil
}

func validateProxyList(list *ocgProxyList) error {
	if list == nil {
		return nil
	}
	switch strings.ToLower(strings.TrimSpace(list.Direction)) {
	case "whitelist", "blacklist":
	default:
		return fmt.Errorf("proxy list direction must be whitelist or blacklist")
	}
	if strings.TrimSpace(list.ProxyURL) == "" {
		return fmt.Errorf("proxy list url is required")
	}
	return validateProxyURL(list.ProxyURL)
}

func validateProxyURL(raw string) error {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "direct", "none":
		return nil
	}
	parsed, err := url.Parse(raw)
	if err != nil || parsed.Host == "" {
		return fmt.Errorf("proxy url is invalid")
	}
	switch strings.ToLower(parsed.Scheme) {
	case "http", "https", "socks5", "socks5h":
		return nil
	default:
		return fmt.Errorf("proxy url scheme is unsupported")
	}
}

func credentialAllowsBlankKey(doc ocgDocument, name string) bool {
	var credential *ocgCredential
	for i := range doc.Credentials {
		if doc.Credentials[i].Namespace == strings.TrimSpace(name) {
			credential = &doc.Credentials[i]
			break
		}
	}
	if credential == nil || credential.CredentialID == "" || len(credential.Routes) == 0 {
		return false
	}
	for _, route := range credential.Routes {
		if strings.ToLower(strings.TrimSpace(route.AuthScheme)) != "none" {
			return false
		}
	}
	return true
}

func compatByName(cfg *config.Config, name string) *config.OpenAICompatibility {
	if cfg == nil {
		return nil
	}
	for i := range cfg.OpenAICompatibility {
		if strings.TrimSpace(cfg.OpenAICompatibility[i].Name) == name {
			return &cfg.OpenAICompatibility[i]
		}
	}
	return nil
}

func rejectSelfLoop(rawURL, listenHost string, listenPort int) error {
	parsed, err := url.Parse(rawURL)
	if err != nil {
		return fmt.Errorf("url: %w", err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return fmt.Errorf("url scheme is unsupported")
	}
	host := parsed.Hostname()
	port := parsed.Port()
	if port == "" {
		if parsed.Scheme == "https" {
			port = "443"
		} else {
			port = "80"
		}
	}
	if port != strconv.Itoa(listenPort) {
		return nil
	}
	if host == "127.0.0.1" || host == "localhost" || host == "::1" || strings.EqualFold(host, listenHost) {
		return fmt.Errorf("self_loop")
	}
	return nil
}

func applyInMemory(cfg *config.Config, doc ocgDocument) {
	if cfg == nil {
		return
	}
	cfg.RequestRetry = 0
	cfg.RequestLog = false
	cfg.LoggingToFile = false
	cfg.CommercialMode = true
	cfg.UsageStatisticsEnabled = false
	cfg.RemoteManagement.AllowRemote = false
	cfg.RemoteManagement.DisableControlPanel = true
	cfg.RemoteManagement.DisableAutoUpdatePanel = true
	cfg.RemoteManagement.SecretKey = ""
	cfg.WebsocketAuth = false
	if doc.Routing.StickyGlobal || doc.Routing.ConversationSticky {
		cfg.Routing.SessionAffinity = true
		ttl := uint64(doc.Routing.ConversationTTL)
		if ttl == 0 {
			ttl = 1800
		}
		cfg.Routing.SessionAffinityTTL = strconv.FormatUint(ttl, 10) + "s"
	}
}
