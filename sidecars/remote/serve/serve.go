// Package serve is the remote gateway's only inbound surface.
//
// It binds to loopback; `tailscale serve` terminates TLS in front of it. Every request passes the
// security layer (security.go) before any handler sees it, and the handlers are injected so the
// auth, proxy and static parts can be developed and tested apart from that layer.
package serve

import (
	"errors"
	"io"
	"log"
	"net/http"
	"time"

	"nucleosremote/config"
)

// HeaderTimeout bounds how long a client may take to send its headers.
const HeaderTimeout = 10 * time.Second

// IdleTimeout closes keep-alive connections a client leaves idle.
const IdleTimeout = 120 * time.Second

// BodyReadTimeout bounds how long a client may take to deliver a request body to /auth/* and /api/*.
// The auth bodies are a few hundred bytes and the API bodies are capped (MaxBodyBytes), so 30s is
// generous for a phone on a bad link yet stops a slow-loris trickle. It covers the request body only:
// a response (an SSE stream) is not bound by it.
const BodyReadTimeout = 30 * time.Second

// deadlineBody is a request body whose read deadline is lifted once it has been read to the end.
type deadlineBody struct {
	io.ReadCloser
	rc *http.ResponseController
}

func (b *deadlineBody) lift() { _ = b.rc.SetReadDeadline(time.Time{}) }

func (b *deadlineBody) Read(p []byte) (int, error) {
	n, err := b.ReadCloser.Read(p)
	// Only a complete body lifts the deadline: after a failed read the deadline must stay, or the
	// server's drain of the unread rest would wait on the slow client again.
	if err == io.EOF {
		b.lift()
	}
	return n, err
}

// boundBodyRead makes the request body arrive within d. A request without a body is left alone: the
// connection's background read (which watches for a client hang-up while a stream is served) would
// otherwise trip on the deadline and cancel a long-lived response. A transport that cannot set a
// deadline (ErrNotSupported) is tolerated: the header and idle timeouts still apply.
func boundBodyRead(w http.ResponseWriter, r *http.Request, d time.Duration) {
	if r.Body == nil || r.Body == http.NoBody {
		return
	}
	rc := http.NewResponseController(w)
	if err := rc.SetReadDeadline(time.Now().Add(d)); err != nil {
		if !errors.Is(err, http.ErrNotSupported) {
			log.Printf("serve: cannot bound request body read: %v", err)
		}
		return
	}
	r.Body = &deadlineBody{ReadCloser: r.Body, rc: rc}
}

// Handlers are the three parts of the public surface.
type Handlers struct {
	// Auth serves /auth/*: pairing, session, logout.
	Auth http.Handler
	// API serves /api/*: the authenticated reverse proxy to the daemon.
	API http.Handler
	// Static serves everything else: the built web shell.
	Static http.Handler
}

// notImplemented is the placeholder for a handler that has not been wired in.
var notImplemented = http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
	writeJSONError(w, http.StatusNotImplemented, "not_implemented")
})

// Build assembles the handler tree. A nil member is replaced by a 501 stub.
func Build(cfg config.Config, h Handlers) http.Handler {
	if h.Auth == nil {
		h.Auth = notImplemented
	}
	if h.API == nil {
		h.API = notImplemented
	}
	if h.Static == nil {
		h.Static = notImplemented
	}
	pol := Policy{PublicOrigin: cfg.PublicOrigin, PublicHost: cfg.PublicHost}
	mux := http.NewServeMux()
	mux.Handle("/auth/", guard(pol, true, h.Auth))
	mux.Handle("/api/", guard(pol, true, h.API))
	// The bare prefixes are not part of either surface and must not fall through to the static files.
	missing := guard(pol, true, http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		writeJSONError(w, http.StatusNotFound, "not_found")
	}))
	mux.Handle("/auth", missing)
	mux.Handle("/api", missing)
	mux.Handle("/", guard(pol, false, h.Static))
	return securityHeaders(mux)
}

// Serve blocks, answering browsers until the process ends. A nil member of h is replaced by the
// gateway's real handler (auth, proxy, static).
func Serve(cfg config.Config, h Handlers) error {
	if h.Auth == nil {
		h.Auth = NewAuth(cfg)
	}
	if h.API == nil {
		api, err := NewAPI(cfg)
		if err != nil {
			return err
		}
		h.API = api
	}
	if h.Static == nil {
		h.Static = NewStatic(cfg)
	}
	server := &http.Server{
		Addr:              cfg.Addr,
		Handler:           Build(cfg, h),
		ReadHeaderTimeout: HeaderTimeout,
		// No WriteTimeout: it would cut SSE streams, which capBody bounds instead.
		IdleTimeout: IdleTimeout,
	}
	log.Printf("serving remote gateway on %s for %s", cfg.Addr, cfg.PublicOrigin)
	return server.ListenAndServe()
}
