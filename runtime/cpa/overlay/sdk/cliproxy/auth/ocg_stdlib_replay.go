package auth

import (
	"bytes"
	"io"
	"net/http"
)

// suppressStdlibPhysicalReplay runs only after this attempt is allowed to Do.
// net/http retries a reused connection when GetBody is set, and it also treats
// a nil body or NoBody as replayable once Idempotency-Key or X-Idempotency-Key
// is present. Clearing GetBody keeps a non-empty Content-Length. An empty
// replayable body has to become a one-shot reader; NoBody alone stays
// replayable. The headers and the process transport are left alone.
func suppressStdlibPhysicalReplay(req *http.Request) {
	if req == nil {
		return
	}
	if req.Body != nil && req.Body != http.NoBody && req.GetBody == nil {
		return
	}
	if req.GetBody != nil {
		req.GetBody = nil
	}
	if req.Body != nil && req.Body != http.NoBody {
		return
	}
	if !requestHasIdempotencyHeader(req.Header) {
		return
	}
	req.Body = io.NopCloser(bytes.NewReader(nil))
	req.GetBody = nil
	req.ContentLength = 0
}

func requestHasIdempotencyHeader(header http.Header) bool {
	if len(header) == 0 {
		return false
	}
	if _, ok := header["Idempotency-Key"]; ok {
		return true
	}
	_, ok := header["X-Idempotency-Key"]
	return ok
}
