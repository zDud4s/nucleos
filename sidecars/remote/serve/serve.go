// Package serve is the remote gateway's only inbound surface.
//
// It binds to loopback; `tailscale serve` terminates TLS in front of it. Every request passes the
// security layer (security.go) before any handler sees it, and the handlers are injected so the
// auth, proxy and static parts can be developed and tested apart from that layer.
package serve

import (
	"log"
	"net/http"
	"time"

	"nucleosremote/config"
)

// HeaderTimeout bounds how long a client may take to send its headers.
const HeaderTimeout = 10 * time.Second

// IdleTimeout closes keep-alive connections a client leaves idle.
const IdleTimeout = 120 * time.Second

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
