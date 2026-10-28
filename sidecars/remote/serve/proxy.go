package serve

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strconv"
	"strings"
	"sync/atomic"
	"time"

	"nucleosremote/config"
)

// newTransport is the one transport used towards the daemon. Compression is off so a stream is never
// held back by a codec, and the environment's proxy settings are ignored: the daemon is on loopback.
func newTransport() *http.Transport {
	return &http.Transport{
		Proxy:               nil,
		DialContext:         (&net.Dialer{Timeout: 5 * time.Second}).DialContext,
		DisableCompression:  true,
		MaxIdleConnsPerHost: 8,
		IdleConnTimeout:     60 * time.Second,
	}
}

// apiHandler serves /api/*: it authenticates nothing itself, it carries the cookie to the daemon,
// which decides on every request.
type apiHandler struct {
	cfg config.Config
	rp  *httputil.ReverseProxy
}

// NewAPI builds the /api/* reverse proxy.
func NewAPI(cfg config.Config) (http.Handler, error) {
	target, err := url.Parse(cfg.DaemonURL)
	if err != nil || target.Host == "" {
		return nil, fmt.Errorf("daemon URL: not a valid URL")
	}
	h := &apiHandler{cfg: cfg}
	h.rp = &httputil.ReverseProxy{
		Rewrite:        func(pr *httputil.ProxyRequest) { h.rewrite(pr, target) },
		ModifyResponse: h.modifyResponse,
		ErrorHandler:   h.errorHandler,
		Transport:      newTransport(),
		// -1 flushes after every write, so an SSE event reaches the browser as the daemon emits it.
		FlushInterval: -1,
	}
	return h, nil
}

func (h *apiHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	p := r.URL.Path
	// The daemon refuses /remote/* to a device anyway; the owner surface is not even forwarded.
	if p == "/api/remote" || strings.HasPrefix(p, "/api/remote/") {
		writeJSONError(w, http.StatusNotFound, "not_found")
		return
	}
	if _, ok := requestSession(r); !ok {
		// No session: there is nothing to ask the daemon.
		writeJSONError(w, http.StatusUnauthorized, refusalSessionInvalid)
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, h.cfg.MaxBodyBytes)
	h.rp.ServeHTTP(w, r)
}

// stripInbound removes the headers that must not travel to the daemon from the browser side:
// credentials, identity claims from the tailnet, forwarding hints, and every daemon-internal
// X-Nucleos-* header (e.g. X-Nucleos-Run-Id, which the daemon trusts to name the calling run).
// The gateway sets its own X-Nucleos-Device-Session afterwards.
func stripInbound(h http.Header) {
	for name := range h {
		switch {
		case strings.HasPrefix(name, "Tailscale-"),
			strings.HasPrefix(name, "X-Forwarded-"),
			strings.HasPrefix(name, "X-Nucleos-"):
			delete(h, name)
		}
	}
	for _, name := range []string{
		"Authorization", "Proxy-Authorization", "Cookie", DaemonSessionHeader, CSRFHeader,
		"Forwarded", "X-Real-Ip",
	} {
		h.Del(name)
	}
}

func (h *apiHandler) rewrite(pr *httputil.ProxyRequest, target *url.URL) {
	secret, _ := requestSession(pr.In)
	pr.SetURL(target)
	out := pr.Out.URL
	base := strings.TrimRight(target.Path, "/")
	out.Path = base + strings.TrimPrefix(pr.In.URL.Path, "/api")
	out.RawPath = ""
	if raw := pr.In.URL.RawPath; raw != "" {
		out.RawPath = base + strings.TrimPrefix(raw, "/api")
	}
	stripInbound(pr.Out.Header)
	pr.Out.Header.Set("Authorization", "Bearer "+h.cfg.DaemonToken)
	pr.Out.Header.Set(DaemonSessionHeader, secret)
}

func (h *apiHandler) modifyResponse(resp *http.Response) error {
	resp.Header.Del("Set-Cookie")
	resp.Header.Del("Www-Authenticate")
	resp.Header.Del(DaemonSessionHeader)
	resp.Header.Set("X-Accel-Buffering", "no")

	if resp.StatusCode == http.StatusUnauthorized {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, maxDaemonAnswer))
		if refusalOf(body) == refusalSessionInvalid {
			synthesize(resp, http.StatusUnauthorized, refusalSessionInvalid, h.cfg, true)
		} else {
			// The daemon refused the gateway's own key: not something the browser can fix.
			synthesize(resp, http.StatusBadGateway, "gateway_credential_rejected", h.cfg, false)
		}
		return nil
	}
	if resp.ContentLength < 0 && resp.Body != nil {
		resp.Body = capBody(resp.Body, h.cfg.StreamMax)
	}
	return nil
}

func (h *apiHandler) errorHandler(w http.ResponseWriter, _ *http.Request, err error) {
	var tooBig *http.MaxBytesError
	if errors.As(err, &tooBig) {
		writeJSONError(w, http.StatusRequestEntityTooLarge, "too_large")
		return
	}
	// The error text names the daemon's address, never a credential.
	log.Printf("proxy: daemon request failed: %v", err)
	writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
}

// synthesize replaces an upstream response with a gateway-authored JSON error. Nothing of the
// upstream headers survives.
func synthesize(resp *http.Response, status int, code string, cfg config.Config, clear bool) {
	_ = resp.Body.Close()
	body, _ := json.Marshal(map[string]string{"error": code})
	body = append(body, '\n')
	resp.StatusCode = status
	resp.Status = fmt.Sprintf("%d %s", status, http.StatusText(status))
	resp.Header = http.Header{
		"Content-Type":   {"application/json"},
		"Content-Length": {strconv.Itoa(len(body))},
	}
	if clear {
		resp.Header.Add("Set-Cookie", clearedCookie(cfg).String())
	}
	resp.ContentLength = int64(len(body))
	resp.TransferEncoding = nil
	resp.Trailer = nil
	resp.Body = io.NopCloser(bytes.NewReader(body))
}

// cappedBody ends a response body cleanly (io.EOF) once its lifetime is up, so the browser sees a
// normal end of stream and reconnects, re-authenticating as it does.
type cappedBody struct {
	rc      io.ReadCloser
	expired atomic.Bool
	timer   *time.Timer
}

func capBody(rc io.ReadCloser, limit time.Duration) io.ReadCloser {
	c := &cappedBody{rc: rc}
	c.timer = time.AfterFunc(limit, func() {
		// The flag first, so a Read unblocked by the Close below already knows why.
		c.expired.Store(true)
		_ = rc.Close()
	})
	return c
}

func (c *cappedBody) Read(p []byte) (int, error) {
	n, err := c.rc.Read(p)
	if err != nil && err != io.EOF && c.expired.Load() {
		err = io.EOF
	}
	return n, err
}

func (c *cappedBody) Close() error {
	c.timer.Stop()
	return c.rc.Close()
}
