package auth

import (
	"bufio"
	"bytes"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestOCGNativeDispatchGuardStdlibReplay(t *testing.T) {
	t.Run("bytes reader and empty body", func(t *testing.T) {
		payload := []byte("generation-body")
		req, err := http.NewRequest(http.MethodPost, "http://127.0.0.1:9/v1/responses", bytes.NewReader(payload))
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("Idempotency-Key", "same-key")
		if req.GetBody == nil || req.ContentLength != int64(len(payload)) || req.Body == nil || req.Body == http.NoBody {
			t.Fatalf("bytes reader was not rewindable: len=%d body=%v", req.ContentLength, req.Body)
		}
		suppressStdlibPhysicalReplay(req)
		if req.GetBody != nil || req.ContentLength != int64(len(payload)) || req.Header.Get("Idempotency-Key") != "same-key" {
			t.Fatalf("non-empty rewind clear changed framing or header: %+v", req)
		}
		got, err := io.ReadAll(req.Body)
		if err != nil || string(got) != string(payload) {
			t.Fatalf("body = %q %v", got, err)
		}

		empty, err := http.NewRequest(http.MethodPost, "http://127.0.0.1:9/v1/responses", bytes.NewReader(nil))
		if err != nil {
			t.Fatal(err)
		}
		empty.Header.Set("X-Idempotency-Key", "")
		if _, ok := empty.Header["X-Idempotency-Key"]; !ok || empty.Body != http.NoBody || empty.GetBody == nil {
			t.Fatalf("empty reader body=%v get=%v header=%v", empty.Body, empty.GetBody != nil, empty.Header)
		}
		suppressStdlibPhysicalReplay(empty)
		if empty.GetBody != nil || empty.Body == nil || empty.Body == http.NoBody || empty.Header.Get("X-Idempotency-Key") != "" {
			t.Fatal("empty replayable body stayed rewindable or lost its header")
		}
		if got, err := io.ReadAll(empty.Body); err != nil || len(got) != 0 {
			t.Fatalf("empty body = %q %v", got, err)
		}

		plain, err := http.NewRequest(http.MethodPost, "http://127.0.0.1:9/v1/responses", io.NopCloser(bytes.NewReader(payload)))
		if err != nil {
			t.Fatal(err)
		}
		plain.ContentLength = int64(len(payload))
		plain.Header.Set("Idempotency-Key", "plain")
		suppressStdlibPhysicalReplay(plain)
		if plain.GetBody != nil || plain.ContentLength != int64(len(payload)) {
			t.Fatal("one-shot body was rewritten")
		}
	})

	t.Run("central generation calls", func(t *testing.T) {
		_, file, _, ok := runtime.Caller(0)
		if !ok {
			t.Fatal("caller")
		}
		root := filepath.Clean(filepath.Join(filepath.Dir(file), "..", "..", ".."))
		names := []string{
			"codex_executor_execute.go",
			"codex_executor_stream.go",
			"codex_executor_request.go",
			"codex_openai_images.go",
			"antigravity_executor.go",
			"antigravity_executor_execute.go",
			"antigravity_executor_stream.go",
			"antigravity_executor_tokens.go",
			"claude_executor.go",
			"claude_executor_request.go",
			"openai_compat_executor.go",
			"kimi_executor.go",
			"xai_executor.go",
			"xai_executor_execute.go",
			"xai_executor_stream.go",
			"xai_executor_media.go",
		}
		for _, name := range names {
			text, err := os.ReadFile(filepath.Join(root, "internal", "runtime", "executor", name))
			if err != nil {
				t.Fatalf("%s: %v", name, err)
			}
			if missed := generationDoWithoutGuard(string(text)); len(missed) != 0 {
				t.Fatalf("%s Do without BeforeOCGHTTPDispatch: %s", name, strings.Join(missed, "; "))
			}
		}
	})

	t.Run("ordinary post replays on a warmed connection", func(t *testing.T) {
		hits := replayPhysicalPosts(t, "Idempotency-Key", []byte("ocg-drop-marker"), false)
		if hits.drops != 2 || hits.dropConn != hits.warmConn || hits.warmConn == 0 || hits.replayConn == hits.warmConn || hits.header != "Idempotency-Key" {
			t.Fatalf("ordinary replay = %+v", hits)
		}
	})

	t.Run("admitted idempotency key is one physical send", func(t *testing.T) {
		hits := replayPhysicalPosts(t, "Idempotency-Key", []byte("ocg-drop-marker"), true)
		if hits.drops != 1 || hits.dropConn != hits.warmConn || hits.accepts < 1 || hits.header != "Idempotency-Key" {
			t.Fatalf("admitted replay = %+v", hits)
		}
	})

	t.Run("admitted x idempotency key is one physical send", func(t *testing.T) {
		hits := replayPhysicalPosts(t, "X-Idempotency-Key", []byte("ocg-drop-marker"), true)
		if hits.drops != 1 || hits.dropConn != hits.warmConn || hits.header != "X-Idempotency-Key" {
			t.Fatalf("x-idempotency replay = %+v", hits)
		}
	})

	t.Run("admitted empty body is one physical send", func(t *testing.T) {
		hits := replayPhysicalPosts(t, "Idempotency-Key", nil, true)
		if hits.drops != 1 || hits.dropConn != hits.warmConn || hits.emptyChunked != 1 {
			t.Fatalf("empty replay = %+v", hits)
		}
	})

	t.Run("two admitted successes share one connection", func(t *testing.T) {
		server := startReplayServer(t)
		client := replayClient()
		raw := server.url + "/v1/responses"
		for _, marker := range []string{"ocg-warm-marker", "ocg-next-marker"} {
			ctx, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{dispatchPin(t, raw)})
			req, err := http.NewRequest(http.MethodPost, raw, bytes.NewReader([]byte(marker)))
			if err != nil {
				t.Fatal(err)
			}
			req.Header.Set("Idempotency-Key", marker)
			if err := BeforeOCGHTTPDispatch(ctx, req); err != nil {
				t.Fatal(err)
			}
			resp, err := client.Do(req)
			if err != nil {
				t.Fatal(err)
			}
			_, _ = io.ReadAll(resp.Body)
			_ = resp.Body.Close()
		}
		if server.accepts.Load() != 1 || server.warm.Load() != 1 || server.next.Load() != 1 {
			t.Fatalf("accepts=%d warm=%d next=%d", server.accepts.Load(), server.warm.Load(), server.next.Load())
		}
	})
}

func generationDoWithoutGuard(text string) []string {
	lines := strings.Split(text, "\n")
	var missed []string
	name := ""
	guard := false
	for _, line := range lines {
		if strings.HasPrefix(line, "func ") {
			name = strings.TrimSpace(line)
			guard = false
		}
		trim := strings.TrimSpace(line)
		if strings.HasPrefix(trim, "//") {
			continue
		}
		if strings.Contains(line, "BeforeOCGHTTPDispatch") {
			guard = true
		}
		if strings.Contains(line, ".Do(") && !guard {
			missed = append(missed, name)
		}
	}
	return missed
}

type replayHits struct {
	drops, accepts, emptyChunked int
	warmConn, dropConn           int
	replayConn                   int
	header                       string
}

func replayPhysicalPosts(t *testing.T, headerName string, dropBody []byte, guard bool) replayHits {
	t.Helper()
	server := startReplayServer(t)
	client := replayClient()
	warmURL := server.url + "/v1/warm"
	warm, err := http.NewRequest(http.MethodPost, warmURL, bytes.NewReader([]byte("ocg-warm-marker")))
	if err != nil {
		t.Fatal(err)
	}
	warm.Header.Set(headerName, "warm-key")
	resp, err := client.Do(warm)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.ReadAll(resp.Body)
	_ = resp.Body.Close()

	dropURL := server.url + "/v1/responses"
	var body io.Reader
	if dropBody == nil {
		body = bytes.NewReader(nil)
	} else {
		body = bytes.NewReader(dropBody)
	}
	drop, err := http.NewRequest(http.MethodPost, dropURL, body)
	if err != nil {
		t.Fatal(err)
	}
	drop.Header.Set(headerName, "drop-key")
	if guard {
		ctx, _ := admitDispatch(t, true, GenerationKindExecute, []EndpointPin{dispatchPin(t, dropURL)})
		length := drop.ContentLength
		if err := BeforeOCGHTTPDispatch(ctx, drop); err != nil {
			t.Fatal(err)
		}
		if drop.GetBody != nil || drop.Header.Get(headerName) != "drop-key" {
			t.Fatal("guard removed the rewind badly or the header")
		}
		if dropBody != nil && drop.ContentLength != length {
			t.Fatalf("content length %d want %d", drop.ContentLength, length)
		}
		facts := generationFactsFrom(ctx)
		if facts == nil || !facts.dispatchConsumed || facts.matched == nil {
			t.Fatal("admitted send did not consume its pin")
		}
	}
	if _, err := client.Do(drop); err == nil {
		t.Fatal("closed response returned a body")
	}
	want := int32(1)
	if !guard {
		want = 2
	}
	server.wait(t, want)
	return replayHits{
		drops:        int(server.drops.Load()),
		accepts:      int(server.accepts.Load()),
		emptyChunked: int(server.emptyChunked.Load()),
		warmConn:     int(server.warmConn.Load()),
		dropConn:     int(server.dropConn.Load()),
		replayConn:   int(server.replayConn.Load()),
		header:       server.dropHeader.Load().(string),
	}
}

func replayClient() *http.Client {
	return &http.Client{Timeout: 5 * time.Second, Transport: &http.Transport{DisableKeepAlives: false}}
}

type replayServer struct {
	url          string
	accepts      interface{ Load() int32 }
	warm         interface{ Load() int32 }
	next         interface{ Load() int32 }
	drops        interface{ Load() int32 }
	emptyChunked interface{ Load() int32 }
	warmConn     interface{ Load() int32 }
	dropConn     interface{ Load() int32 }
	replayConn   interface{ Load() int32 }
	dropHeader   interface{ Load() any }
	wait         func(t *testing.T, drops int32)
}

func startReplayServer(t *testing.T) *replayServer {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	var accepts, warm, next, drops, emptyChunked, warmConn, dropConn, replayConn syncInt
	var dropHeader atomicString
	var conns sync.WaitGroup
	go func() {
		connID := int32(0)
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			connID++
			id := connID
			accepts.add(1)
			conns.Add(1)
			go func() {
				defer conns.Done()
				defer conn.Close()
				reader := bufio.NewReader(conn)
				for {
					received, err := readReplayRequest(reader)
					if err != nil {
						return
					}
					switch {
					case strings.Contains(received.body, "ocg-warm-marker"):
						warm.add(1)
						warmConn.store(id)
						if !writeReplayOK(conn, "warm") {
							return
						}
					case strings.Contains(received.body, "ocg-next-marker"):
						next.add(1)
						if !writeReplayOK(conn, "next") {
							return
						}
					default:
						if drops.add(1) == 1 {
							dropConn.store(id)
						}
						replayConn.store(id)
						if received.header != "" {
							dropHeader.store(received.header)
						}
						if received.chunked && received.body == "" {
							emptyChunked.add(1)
						}
						return
					}
				}
			}()
		}
	}()
	return &replayServer{
		url:          "http://" + listener.Addr().String(),
		accepts:      &accepts,
		warm:         &warm,
		next:         &next,
		drops:        &drops,
		emptyChunked: &emptyChunked,
		warmConn:     &warmConn,
		dropConn:     &dropConn,
		replayConn:   &replayConn,
		dropHeader:   &dropHeader,
		wait: func(t *testing.T, want int32) {
			t.Helper()
			deadline := time.Now().Add(2 * time.Second)
			for drops.load() < want && time.Now().Before(deadline) {
				time.Sleep(10 * time.Millisecond)
			}
		},
	}
}

type replayRequest struct {
	body    string
	header  string
	chunked bool
}

func readReplayRequest(reader *bufio.Reader) (replayRequest, error) {
	if _, err := readReplayLine(reader); err != nil {
		return replayRequest{}, err
	}
	header := http.Header{}
	for {
		line, err := readReplayLine(reader)
		if err != nil {
			return replayRequest{}, err
		}
		if line == "" {
			break
		}
		name, value, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		header.Add(strings.TrimSpace(name), strings.TrimSpace(value))
	}
	received := replayRequest{}
	if _, ok := header["Idempotency-Key"]; ok {
		received.header = "Idempotency-Key"
	} else if _, ok := header["X-Idempotency-Key"]; ok {
		received.header = "X-Idempotency-Key"
	}
	if strings.Contains(strings.ToLower(header.Get("Transfer-Encoding")), "chunked") {
		body, err := readReplayChunks(reader)
		received.chunked = true
		received.body = string(body)
		return received, err
	}
	n, _ := strconv.Atoi(header.Get("Content-Length"))
	if n > 0 {
		buf := make([]byte, n)
		_, err := io.ReadFull(reader, buf)
		received.body = string(buf)
		return received, err
	}
	return received, nil
}

func readReplayChunks(reader *bufio.Reader) ([]byte, error) {
	var body bytes.Buffer
	for {
		line, err := readReplayLine(reader)
		if err != nil {
			return nil, err
		}
		sizeText := strings.TrimSpace(strings.SplitN(line, ";", 2)[0])
		size, err := strconv.ParseInt(sizeText, 16, 64)
		if err != nil {
			return nil, err
		}
		if size == 0 {
			for {
				trailer, err := readReplayLine(reader)
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
		if _, err := readReplayLine(reader); err != nil {
			return nil, err
		}
	}
}

func readReplayLine(reader *bufio.Reader) (string, error) {
	line, err := reader.ReadString('\n')
	return strings.TrimRight(line, "\r\n"), err
}

func writeReplayOK(conn net.Conn, body string) bool {
	payload := []byte(body)
	var buf bytes.Buffer
	buf.WriteString("HTTP/1.1 200 OK\r\nContent-Length: ")
	buf.WriteString(strconv.Itoa(len(payload)))
	buf.WriteString("\r\nConnection: keep-alive\r\n\r\n")
	buf.Write(payload)
	_, err := conn.Write(buf.Bytes())
	return err == nil
}

type syncInt struct {
	mu sync.Mutex
	n  int32
}

func (s *syncInt) add(d int32) int32 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.n += d
	return s.n
}

func (s *syncInt) store(n int32) {
	s.mu.Lock()
	s.n = n
	s.mu.Unlock()
}

func (s *syncInt) load() int32 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.n
}

func (s *syncInt) Load() int32 { return s.load() }

type atomicString struct {
	mu sync.Mutex
	v  string
}

func (s *atomicString) store(v string) {
	s.mu.Lock()
	s.v = v
	s.mu.Unlock()
}

func (s *atomicString) Load() any {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.v
}
