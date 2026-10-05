//go:build windows

package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestProcessReloadRestoresWithoutRevivingSecrets(t *testing.T) {
	exe := hostExecutable(t)
	var mu sync.Mutex
	var auths []string
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		auths = append(auths, r.Header.Get("Authorization"))
		mu.Unlock()
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(okBody))
	}))
	defer upstream.Close()
	var readyMu sync.Mutex
	var readyRevisions []string
	policy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		req, _, err := readPolicy(r)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		switch req.Operation {
		case "ready":
			readyMu.Lock()
			readyRevisions = append(readyRevisions, req.ProjectionRevision)
			readyMu.Unlock()
			if req.ProjectionRevision == "8" {
				http.Error(w, "ready rejected", http.StatusInternalServerError)
				return
			}
			writeReady(w, req)
		case "admit":
			writeDecision(w, "allow", "eligible")
		case "result":
			writeDecision(w, "stop", "recorded")
		default:
			http.Error(w, "op", http.StatusBadRequest)
		}
	}))
	defer policy.Close()

	dir := testWorkDir(t)
	if err := os.MkdirAll(filepath.FromSlash(absoluteAuthDir(t, dir)), 0o700); err != nil {
		t.Fatal(err)
	}
	port := freeListenPort(t)
	configPath := writeConfig(t, dir, apiProcessConfig(t, dir, port, "7", policy.URL, upstream.URL, "secret-key-alpha"))
	originalDigest := fileSHA256(t, configPath)
	base := "http://127.0.0.1:" + strconv.Itoa(port)
	cmd, output := startHostProcess(t, exe, configPath)
	_ = cmd
	initial := waitProcessReady(t, base, output, 30*time.Second, func(doc readyDocument) bool {
		return doc.PolicyReady && doc.ApplyStatus == "applied" && doc.AppliedProjectionRevision == "7" && doc.AppliedProjectionDigest == originalDigest
	})
	if strings.Contains(initial, "secret-key-alpha") || strings.Contains(initial, "secret-key-beta") {
		t.Fatal("ready exposed an upstream key")
	}
	first := processChat(base, "7")
	if !sawAuth("Bearer secret-key-alpha", &mu, &auths) || first.status >= 500 {
		t.Fatalf("initial chat status=%d body=%s auths=%v", first.status, first.body, snapshotAuths(&mu, &auths))
	}

	replaceConfig(t, configPath, "[\n")
	time.Sleep(700 * time.Millisecond)
	kept := waitProcessReady(t, base, output, 3*time.Second, func(doc readyDocument) bool {
		return doc.PolicyReady && doc.ApplyStatus == "applied" && doc.AppliedProjectionDigest == originalDigest
	})
	if strings.Contains(kept, "secret-key-alpha") {
		t.Fatal("ready exposed an upstream key after invalid yaml")
	}
	beforeInvalid := len(snapshotAuths(&mu, &auths))
	invalidChat := processChat(base, "7")
	authsNow := snapshotAuths(&mu, &auths)
	if invalidChat.status >= 500 || len(authsNow) != beforeInvalid+1 || authsNow[len(authsNow)-1] != "Bearer secret-key-alpha" {
		t.Fatalf("invalid yaml changed admission status=%d body=%s auths=%v stderr=%s", invalidChat.status, invalidChat.body, authsNow, output.String())
	}

	blank := strings.Replace(apiProcessConfig(t, dir, port, "7", policy.URL, upstream.URL, "secret-key-alpha"), "api-key: secret-key-alpha", "api-key: \"\"", 1)
	replaceConfig(t, configPath, blank)
	_ = waitProcessReady(t, base, output, 8*time.Second, func(doc readyDocument) bool {
		return doc.PolicyReady && doc.ApplyStatus == "unsupported" && doc.AppliedProjectionRevision == "7" && doc.AppliedProjectionDigest == originalDigest
	})
	blankChat := processChat(base, "7")
	authsNow = snapshotAuths(&mu, &auths)
	if blankChat.status >= 500 || authsNow[len(authsNow)-1] != "Bearer secret-key-alpha" {
		t.Fatalf("blank bearer was admitted status=%d auths=%v", blankChat.status, authsNow)
	}

	missing := strings.Replace(apiProcessConfig(t, dir, port, "7", policy.URL, upstream.URL, "secret-key-alpha"), "  oauth-bindings: []\n", `  oauth-bindings:
    - relative-path: missing.json
      credential-id: oauth-missing
      credential-version: "4"
      priority: "1"
      provider-id: canonical-oauth
      models: ["public-model"]
`, 1)
	replaceConfig(t, configPath, missing)
	_ = waitProcessReady(t, base, output, 15*time.Second, func(doc readyDocument) bool {
		return doc.PolicyReady && doc.ApplyStatus == "rollback" && doc.AppliedProjectionRevision == "7" && doc.AppliedProjectionDigest == originalDigest
	})
	regChat := processChat(base, "7")
	authsNow = snapshotAuths(&mu, &auths)
	if regChat.status >= 500 || authsNow[len(authsNow)-1] != "Bearer secret-key-alpha" {
		t.Fatalf("registration failure did not restore the previous key status=%d auths=%v", regChat.status, authsNow)
	}

	sameKey := apiProcessConfig(t, dir, port, "8", policy.URL, upstream.URL, "secret-key-alpha")
	replaceConfig(t, configPath, sameKey)
	waitReadyRevision(t, &readyMu, &readyRevisions, "8", 8*time.Second)
	_ = waitProcessReady(t, base, output, 12*time.Second, func(doc readyDocument) bool {
		return doc.PolicyReady && doc.ApplyStatus == "rollback" && doc.AppliedProjectionRevision == "7" && doc.AppliedProjectionDigest == originalDigest
	})
	readyChat := processChat(base, "7")
	authsNow = snapshotAuths(&mu, &auths)
	if readyChat.status >= 500 || authsNow[len(authsNow)-1] != "Bearer secret-key-alpha" {
		t.Fatalf("rejected ready revived a new admission status=%d auths=%v", readyChat.status, authsNow)
	}

	beforeRotate := len(snapshotAuths(&mu, &auths))
	rotated := apiProcessConfig(t, dir, port, "8", policy.URL, upstream.URL, "secret-key-beta")
	replaceConfig(t, configPath, rotated)
	failed := waitProcessReady(t, base, output, 12*time.Second, func(doc readyDocument) bool {
		return !doc.PolicyReady && doc.ApplyStatus == "apply_failure"
	})
	if strings.Contains(failed, "secret-key-alpha") || strings.Contains(failed, "secret-key-beta") {
		t.Fatal("ready exposed a rotated key")
	}
	blocked := processChat(base, "7")
	authsNow = snapshotAuths(&mu, &auths)
	if blocked.status != http.StatusServiceUnavailable || len(authsNow) != beforeRotate {
		t.Fatalf("rotated secret stayed admissible status=%d body=%s auths=%v", blocked.status, blocked.body, authsNow)
	}

	recoveredBody := apiProcessConfig(t, dir, port, "9", policy.URL, upstream.URL, "secret-key-beta")
	replaceConfig(t, configPath, recoveredBody)
	recoveredDigest := fileSHA256(t, configPath)
	_ = waitProcessReady(t, base, output, 12*time.Second, func(doc readyDocument) bool {
		return doc.PolicyReady && doc.ApplyStatus == "applied" && doc.AppliedProjectionRevision == "9" && doc.AppliedProjectionDigest == recoveredDigest
	})
	recovered := processChat(base, "9")
	authsNow = snapshotAuths(&mu, &auths)
	if recovered.status >= 500 || authsNow[len(authsNow)-1] != "Bearer secret-key-beta" {
		t.Fatalf("recovered chat status=%d body=%s auths=%v stderr=%s", recovered.status, recovered.body, authsNow, output.String())
	}
}

func apiProcessConfig(t *testing.T, dir string, port int, revision, policyURL, upstream, key string) string {
	t.Helper()
	body := baseConfig(port, policyURL, "fill-first", false, false, "")
	body = strings.ReplaceAll(body, `projection-revision: "7"`, `projection-revision: "`+revision+`"`)
	body = strings.ReplaceAll(body, "AUTHDIR", yamlQuote(absoluteAuthDir(t, dir)))
	body = strings.Replace(body, "openai-compatibility: []\n", "openai-compatibility:\n  - name: auth-ns\n    priority: 1\n    base-url: "+upstream+"/v1\n    api-key-entries:\n      - api-key: "+key+"\n    models:\n      - name: upstream-model\n        alias: public-model\n", 1)
	body = strings.Replace(body, "  credentials: []\n", "  credentials:\n    - namespace: auth-ns\n      auth-id: auth-ns\n      credential-id: cred-1\n      credential-version: \"3\"\n      binding-id: bind-1\n      material-revision: material-1\n      provider-id: canonical-provider\n      opaque-remote: false\n      routes:\n        - public-model: public-model\n          upstream-model: upstream-model\n          protocol: chat_completions\n          endpoint: "+upstream+"/v1/chat/completions\n          auth-scheme: bearer\n          request-identity: none\n          wire: none\n", 1)
	return body
}

func hostExecutable(t *testing.T) string {
	t.Helper()
	if exe := strings.TrimSpace(os.Getenv("OCG_CPA_HOST_EXE")); exe != "" {
		info, err := os.Stat(exe)
		if err != nil || info.IsDir() {
			t.Fatalf("OCG_CPA_HOST_EXE is not a file: %s", exe)
		}
		return exe
	}
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("caller")
	}
	out := filepath.Join(t.TempDir(), "ocg-cpa-host.exe")
	cmd := exec.Command("go", "build", "-o", out, ".")
	cmd.Dir = filepath.Dir(file)
	cmd.Env = os.Environ()
	raw, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("go build host: %v\n%s", err, raw)
	}
	return out
}

func startHostProcess(t *testing.T, exe, configPath string) (*exec.Cmd, *lockedBuf) {
	t.Helper()
	cmd := exec.Command(exe, "--config", configPath)
	output := &lockedBuf{}
	cmd.Stdout = output
	cmd.Stderr = output
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	done := make(chan struct{})
	go func() {
		_ = cmd.Wait()
		close(done)
	}()
	t.Cleanup(func() {
		if cmd.Process != nil {
			_ = cmd.Process.Kill()
		}
		select {
		case <-done:
		case <-time.After(5 * time.Second):
		}
	})
	deadline := time.Now().Add(40 * time.Second)
	for time.Now().Before(deadline) {
		if strings.Contains(output.String(), "ocg-cpa-host ready ") {
			return cmd, output
		}
		select {
		case <-done:
			t.Fatalf("host exited before ready: %s", output.String())
		case <-time.After(50 * time.Millisecond):
		}
	}
	t.Fatalf("host did not become ready: %s", output.String())
	return nil, nil
}

func waitProcessReady(t *testing.T, base string, output *lockedBuf, timeout time.Duration, ok func(readyDocument) bool) string {
	t.Helper()
	deadline := time.Now().Add(timeout)
	var last string
	for time.Now().Before(deadline) {
		status, doc, raw := fetchReady(base)
		last = raw
		if status == http.StatusOK && ok(doc) && !strings.Contains(raw, "secret-key-alpha") && !strings.Contains(raw, "secret-key-beta") {
			return raw
		}
		time.Sleep(100 * time.Millisecond)
	}
	t.Fatalf("ready timeout: %s\nstderr: %s", last, output.String())
	return ""
}

func waitReadyRevision(t *testing.T, mu *sync.Mutex, revisions *[]string, want string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		mu.Lock()
		seen := append([]string(nil), (*revisions)...)
		mu.Unlock()
		for _, revision := range seen {
			if revision == want {
				return
			}
		}
		time.Sleep(100 * time.Millisecond)
	}
	mu.Lock()
	defer mu.Unlock()
	t.Fatalf("policy did not observe ready revision %s: %v", want, *revisions)
}

func fetchReady(base string) (int, readyDocument, string) {
	req, err := http.NewRequest(http.MethodGet, base+"/_internal/ocg/ready", nil)
	if err != nil {
		return 0, readyDocument{}, err.Error()
	}
	req.Header.Set("X-OCG-Ready-Token", "ready-secret")
	req.Header.Set("Origin", "http://127.0.0.1")
	resp, err := directClient().Do(req)
	if err != nil {
		return 0, readyDocument{}, err.Error()
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	var doc readyDocument
	_ = json.Unmarshal(raw, &doc)
	return resp.StatusCode, doc, string(raw)
}

func processChat(base, revision string) chatResult {
	req, err := http.NewRequest(http.MethodPost, base+"/v1/chat/completions", strings.NewReader(chatPayload))
	if err != nil {
		return chatResult{body: err.Error()}
	}
	req.Header.Set("Authorization", "Bearer hop-secret")
	stampPolicyIdentity(req, newID(), "41", revision)
	resp, err := directClient().Do(req)
	if err != nil {
		return chatResult{body: err.Error()}
	}
	raw, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	return chatResult{status: resp.StatusCode, body: string(raw)}
}

func fileSHA256(t *testing.T, path string) string {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:])
}

func sawAuth(want string, mu *sync.Mutex, auths *[]string) bool {
	mu.Lock()
	defer mu.Unlock()
	for _, item := range *auths {
		if item == want {
			return true
		}
	}
	return false
}

func snapshotAuths(mu *sync.Mutex, auths *[]string) []string {
	mu.Lock()
	defer mu.Unlock()
	return append([]string(nil), (*auths)...)
}

type lockedBuf struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (l *lockedBuf) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.b.Write(p)
}

func (l *lockedBuf) String() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.b.String()
}
