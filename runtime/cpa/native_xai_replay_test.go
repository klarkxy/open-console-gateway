package main

import (
	"bufio"
	"bytes"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestNativeXAIWarmedConnectionDoesNotReplay(t *testing.T) {
	t.Run("idempotency-key", func(t *testing.T) {
		assertXAIWarmedDropIsSingle(t, "Idempotency-Key")
	})
	t.Run("x-idempotency-key", func(t *testing.T) {
		assertXAIWarmedDropIsSingle(t, "X-Idempotency-Key")
	})
}

func assertXAIWarmedDropIsSingle(t *testing.T, headerName string) {
	t.Helper()
	server := startPhysicalServer(t)
	var otherHits atomicInt32
	other := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			otherHits.add(1)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(codexSSE))
	}))
	defer other.Close()
	policy, results := strictPolicy(t, func(policyRequest) string { return "stop" })
	defer policy.Close()
	host := startNative(t, policy.URL, nativeSpec{
		strategy: "fill-first",
		bindings: []nativeBinding{
			{
				file: "xai-a.json", id: "oauth-a", version: "4", priority: "10", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "token-a", baseURL: server.url, kind: "xai", usingAPI: "false",
				headers: map[string]string{headerName: "drop-key"},
			},
			{
				file: "xai-b.json", id: "oauth-b", version: "4", priority: "1", provider: "canonical-oauth",
				models: []string{nativeModel}, access: "token-b", baseURL: other.URL, kind: "xai", usingAPI: "false",
			},
		},
	})
	t.Cleanup(func() {
		if transport, ok := http.DefaultTransport.(*http.Transport); ok {
			transport.CloseIdleConnections()
		}
	})
	warm := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"ocg-warm-marker"}`)
	if server.warm.Load() != 1 || warm.status >= 500 {
		t.Fatalf("warm status=%d body=%s hits=%+v", warm.status, warm.body, server.snapshot())
	}
	drop := postPolicy(t, host, "/v1/responses", `{"model":"`+nativeModel+`","input":"ocg-drop-marker"}`)
	_ = drop
	deadline := time.Now().Add(2 * time.Second)
	for server.drops.Load() < 1 && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	got := server.snapshot()
	if got.drops != 1 || got.warmConn == 0 || got.dropConn != got.warmConn || got.dropHeader != headerName || otherHits.load() != 0 {
		t.Fatalf("physical=%+v other=%d status=%d", got, otherHits.load(), drop.status)
	}
	results.mu.Lock()
	defer results.mu.Unlock()
	if len(results.admits) != 2 || len(results.items) != 2 || results.items[0].Outcome != "success" || results.items[1].Outcome != "uncertain" {
		t.Fatalf("admits=%d items=%+v", len(results.admits), results.items)
	}
	requireMatchedPin(t, results.items[1], server.url+"/responses", "responses")
}

type physicalSnapshot struct {
	accepts, drops, warmConn, dropConn int
	dropHeader                         string
	bodies                             []string
}

type physicalServer struct {
	url        string
	accepts    *atomicInt32
	warm       *atomicInt32
	drops      *atomicInt32
	warmConn   *atomicInt32
	dropConn   *atomicInt32
	dropHeader atomicText
	bodies     *bodyLog
}

func (s *physicalServer) snapshot() physicalSnapshot {
	return physicalSnapshot{
		accepts: int(s.accepts.load()), drops: int(s.drops.load()),
		warmConn: int(s.warmConn.load()), dropConn: int(s.dropConn.load()),
		dropHeader: s.dropHeader.load(), bodies: s.bodies.copy(),
	}
}

func startPhysicalServer(t *testing.T) *physicalServer {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	server := &physicalServer{
		url:     "http://" + listener.Addr().String(),
		accepts: new(atomicInt32), warm: new(atomicInt32), drops: new(atomicInt32),
		warmConn: new(atomicInt32), dropConn: new(atomicInt32), bodies: &bodyLog{},
	}
	var conns sync.Mutex
	open := map[net.Conn]struct{}{}
	t.Cleanup(func() {
		_ = listener.Close()
		conns.Lock()
		defer conns.Unlock()
		for conn := range open {
			_ = conn.Close()
		}
	})
	go func() {
		var next int32
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			next++
			id := next
			server.accepts.add(1)
			conns.Lock()
			open[conn] = struct{}{}
			conns.Unlock()
			go func() {
				defer func() {
					_ = conn.Close()
					conns.Lock()
					delete(open, conn)
					conns.Unlock()
				}()
				reader := bufio.NewReader(conn)
				for {
					body, header, err := readPhysicalRequest(reader)
					if err != nil {
						return
					}
					server.bodies.add(body)
					switch {
					case strings.Contains(body, "ocg-warm-marker"):
						server.warm.add(1)
						server.warmConn.store(id)
						if !writePhysicalOK(conn, codexSSE, "text/event-stream") {
							return
						}
					case strings.Contains(body, "ocg-drop-marker"):
						server.drops.add(1)
						server.dropConn.store(id)
						server.dropHeader.store(physicalIdempotency(header))
						return
					default:
						if !writePhysicalOK(conn, `{"models":{}}`, "application/json") {
							return
						}
					}
				}
			}()
		}
	}()
	return server
}

func physicalIdempotency(header http.Header) string {
	if _, ok := header["Idempotency-Key"]; ok {
		return "Idempotency-Key"
	}
	if _, ok := header["X-Idempotency-Key"]; ok {
		return "X-Idempotency-Key"
	}
	return ""
}

func readPhysicalRequest(reader *bufio.Reader) (string, http.Header, error) {
	if _, err := readPhysicalLine(reader); err != nil {
		return "", nil, err
	}
	header := http.Header{}
	for {
		line, err := readPhysicalLine(reader)
		if err != nil {
			return "", nil, err
		}
		if line == "" {
			break
		}
		name, value, ok := strings.Cut(line, ":")
		if ok {
			header.Add(strings.TrimSpace(name), strings.TrimSpace(value))
		}
	}
	if strings.Contains(strings.ToLower(header.Get("Transfer-Encoding")), "chunked") {
		body, err := readPhysicalChunks(reader)
		return string(body), header, err
	}
	n, _ := strconv.Atoi(header.Get("Content-Length"))
	if n <= 0 {
		return "", header, nil
	}
	buf := make([]byte, n)
	_, err := io.ReadFull(reader, buf)
	return string(buf), header, err
}

func readPhysicalChunks(reader *bufio.Reader) ([]byte, error) {
	var body bytes.Buffer
	for {
		line, err := readPhysicalLine(reader)
		if err != nil {
			return nil, err
		}
		size, err := strconv.ParseInt(strings.TrimSpace(strings.SplitN(line, ";", 2)[0]), 16, 64)
		if err != nil {
			return nil, err
		}
		if size == 0 {
			for {
				trailer, err := readPhysicalLine(reader)
				if err != nil || trailer == "" {
					return body.Bytes(), err
				}
			}
		}
		buf := make([]byte, size)
		if _, err := io.ReadFull(reader, buf); err != nil {
			return nil, err
		}
		body.Write(buf)
		if _, err := readPhysicalLine(reader); err != nil {
			return nil, err
		}
	}
}

func readPhysicalLine(reader *bufio.Reader) (string, error) {
	line, err := reader.ReadString('\n')
	return strings.TrimRight(line, "\r\n"), err
}

func writePhysicalOK(conn net.Conn, body, contentType string) bool {
	payload := []byte(body)
	var buf bytes.Buffer
	buf.WriteString("HTTP/1.1 200 OK\r\nContent-Type: ")
	buf.WriteString(contentType)
	buf.WriteString("\r\nContent-Length: ")
	buf.WriteString(strconv.Itoa(len(payload)))
	buf.WriteString("\r\nConnection: keep-alive\r\n\r\n")
	buf.Write(payload)
	_, err := conn.Write(buf.Bytes())
	return err == nil
}

type atomicInt32 struct {
	mu sync.Mutex
	n  int32
}

func (a *atomicInt32) add(d int32) {
	a.mu.Lock()
	a.n += d
	a.mu.Unlock()
}

func (a *atomicInt32) store(n int32) {
	a.mu.Lock()
	a.n = n
	a.mu.Unlock()
}

func (a *atomicInt32) load() int32 {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.n
}

func (a *atomicInt32) Load() int32 { return a.load() }

type atomicText struct {
	mu sync.Mutex
	v  string
}

func (a *atomicText) store(v string) {
	a.mu.Lock()
	a.v = v
	a.mu.Unlock()
}

func (a *atomicText) load() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.v
}

type bodyLog struct {
	mu    sync.Mutex
	items []string
}

func (b *bodyLog) add(body string) {
	b.mu.Lock()
	b.items = append(b.items, body)
	b.mu.Unlock()
}

func (b *bodyLog) copy() []string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]string(nil), b.items...)
}
