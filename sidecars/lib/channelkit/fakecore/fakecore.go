// Package fakecore is an in-process fake of the NucleOS core's /channels/{channel}/*
// routes, for testing the kit and the adapters against the contract. It imports
// contract, contract/fixtures and clock only (never kit), so the kit's own tests
// can use it without an import cycle. Nothing here sleeps in real time: waiting is
// driven by notifications and by the injected clock.
package fakecore

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

// Routes that FailNext and LastCall understand.
const (
	RouteHello  = "hello"
	RouteEvents = "events"
	RouteOutbox = "outbox"
	RouteAck    = "ack"
	RouteRenew  = "renew"
	RouteMedia  = "media"
)

const (
	defaultChannel = "tg"
	fakeToken      = "fake-core-token"
	// defaultMediaLimit is the max_media_bytes of the Telegram manifest (annex C.5).
	defaultMediaLimit int64 = 20971520
)

// fault is one pending failure injected by FailNext.
type fault struct {
	left   int
	status int
	body   string
}

// Core is the fake core. All state is guarded by mu; changed is closed and
// replaced whenever something a waiter may care about happens.
type Core struct {
	t   testing.TB
	clk *clock.Fake
	srv *httptest.Server
	mux *http.ServeMux

	mu         sync.Mutex
	changed    chan struct{}
	down       bool
	faults     map[string][]*fault
	calls      map[string]int
	lastCall   map[string]time.Time
	hello      *contract.Hello
	helloCount int
	onHello    func(contract.Hello)
	events     []contract.Event
	seen       map[string]bool
	invalid    []string
	media      map[string]storedMedia
	mediaSeq   int
	mediaLimit int64
	outbox     *queue // guarded by mu; created by mountOutbox
	fallback   func(reason string, failed contract.Action)
}

// New starts the fake core; it is closed when the test ends. clk is the same
// clock.Fake the kit under test uses.
func New(t testing.TB, clk *clock.Fake) *Core {
	t.Helper()
	c := &Core{
		t:          t,
		clk:        clk,
		mux:        http.NewServeMux(),
		changed:    make(chan struct{}),
		faults:     map[string][]*fault{},
		calls:      map[string]int{},
		lastCall:   map[string]time.Time{},
		seen:       map[string]bool{},
		media:      map[string]storedMedia{},
		mediaLimit: defaultMediaLimit,
	}
	c.mux.HandleFunc("PUT /channels/{channel}/hello", c.authed(RouteHello, c.handleHello))
	c.mountEvents()
	c.mountMedia()
	c.mountOutbox()
	c.mountAck()
	c.srv = httptest.NewServer(http.HandlerFunc(c.serve))
	t.Cleanup(c.srv.Close)
	return c
}

// URL is the base URL of the fake core.
func (c *Core) URL() string { return c.srv.URL }

// Token is the bearer token the core accepts.
func (c *Core) Token() string { return fakeToken }

// Channel is the only channel the token may use.
func (c *Core) Channel() string { return defaultChannel }

// Clock is the shared fake clock.
func (c *Core) Clock() *clock.Fake { return c.clk }

// Hello returns the last hello received and how many were accepted.
func (c *Core) Hello() (contract.Hello, int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.hello == nil {
		return contract.Hello{}, c.helloCount
	}
	return *c.hello, c.helloCount
}

// OnHello sets a hook called (outside the lock) after each accepted hello.
func (c *Core) OnHello(fn func(contract.Hello)) {
	c.mu.Lock()
	c.onHello = fn
	c.mu.Unlock()
}

// LastCall is the fake time of the last request that reached route (after the
// middleware, even when it was answered with a fault), and ok=false if none did.
func (c *Core) LastCall(route string) (time.Time, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	at, ok := c.lastCall[route]
	return at, ok
}

// Calls counts the requests that reached route.
func (c *Core) Calls(route string) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.calls[route]
}

// FailNext makes the next n requests to route answer status with body (a
// canned JSON text; may be empty). Faults apply before routing, so routes
// mounted later honour them too. Valid routes are the Route* constants.
func (c *Core) FailNext(route string, n, status int, body string) {
	c.t.Helper()
	switch route {
	case RouteHello, RouteEvents, RouteOutbox, RouteAck, RouteRenew, RouteMedia:
	default:
		c.t.Fatalf("fakecore: unknown route %q", route)
	}
	c.mu.Lock()
	c.faults[route] = append(c.faults[route], &fault{left: n, status: status, body: body})
	c.mu.Unlock()
}

// Down simulates a stopped core: every new request fails at the connection level.
func (c *Core) Down() {
	c.mu.Lock()
	c.down = true
	c.mu.Unlock()
}

// Up undoes Down.
func (c *Core) Up() {
	c.mu.Lock()
	c.down = false
	c.mu.Unlock()
}

// notifyLocked wakes every waiter. The caller holds c.mu.
func (c *Core) notifyLocked() {
	close(c.changed)
	c.changed = make(chan struct{})
}

// serve is the middleware in front of the mux: connection drop, then faults.
func (c *Core) serve(w http.ResponseWriter, r *http.Request) {
	route := routeOf(r.URL.Path)
	c.mu.Lock()
	if c.down {
		c.mu.Unlock()
		if hj, ok := w.(http.Hijacker); ok {
			if conn, _, err := hj.Hijack(); err == nil {
				conn.Close()
			}
		}
		return
	}
	if route != "" {
		c.calls[route]++
		c.lastCall[route] = c.clk.Now()
	}
	var hit *fault
	for _, f := range c.faults[route] {
		if f.left > 0 {
			f.left--
			hit = f
			break
		}
	}
	c.mu.Unlock()
	if hit != nil {
		if hit.body != "" {
			w.Header().Set("Content-Type", "application/json")
		}
		w.WriteHeader(hit.status)
		_, _ = w.Write([]byte(hit.body))
		return
	}
	c.mux.ServeHTTP(w, r)
}

// routeOf maps /channels/{channel}/... to a Route* name ("" if unknown).
func routeOf(path string) string {
	rest, ok := strings.CutPrefix(path, "/channels/")
	if !ok {
		return ""
	}
	parts := strings.Split(rest, "/")
	if len(parts) < 2 {
		return ""
	}
	switch parts[1] {
	case "hello":
		return RouteHello
	case "events":
		return RouteEvents
	case "media":
		return RouteMedia
	case "outbox":
		switch {
		case len(parts) == 2:
			return RouteOutbox
		case len(parts) == 4 && parts[3] == "ack":
			return RouteAck
		case len(parts) == 4 && parts[3] == "renew":
			return RouteRenew
		}
	}
	return ""
}

// authed wraps a handler with the Bearer check and the channel isolation: the
// token is bound to one channel, any other {channel} gets 403 channel_mismatch.
// A bad or missing token gets a bare 403 (the core's require_token).
func (c *Core) authed(route string, h http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer "+fakeToken {
			w.WriteHeader(http.StatusForbidden)
			return
		}
		if r.PathValue("channel") != defaultChannel {
			c.writeFixture(w, http.StatusForbidden, "errors/channel-mismatch.json")
			return
		}
		h(w, r)
	}
}

func (c *Core) writeFixture(w http.ResponseWriter, status int, name string) {
	b, err := fs.ReadFile(fixtures.FS(), name)
	if err != nil {
		c.t.Errorf("fakecore: fixture %s: %v", name, err)
		w.WriteHeader(http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(b)
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	b, _ := json.Marshal(v)
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(b)
}

// writeContractError answers 400 with the body the core generates for a
// contract violation.
func writeContractError(w http.ResponseWriter, err error) {
	body := map[string]string{"error": "decode"}
	if ce, ok := err.(*contract.Error); ok {
		body["error"] = string(ce.Code)
		body["at"] = ce.At
	}
	writeJSON(w, http.StatusBadRequest, body)
}

func (c *Core) handleHello(w http.ResponseWriter, r *http.Request) {
	body, err := readBody(r, 1<<20)
	if err != nil {
		w.WriteHeader(http.StatusBadRequest)
		return
	}
	h, err := contract.DecodeStrict[contract.Hello](body)
	if err != nil {
		writeContractError(w, err)
		return
	}
	if !contract.Compatible(h.Version) {
		writeJSON(w, http.StatusConflict, map[string]string{
			"error":            contract.ErrContractVersion,
			"contract_version": contract.Version,
			"detail":           fmt.Sprintf("adapter speaks %s", h.Version),
		})
		return
	}
	c.mu.Lock()
	c.hello = &h
	c.helloCount++
	// A new adapter process owns no lease: release them all without counting a
	// delivery again (the 10th delivery without ok fails instead of re-pending).
	c.outbox.ReleaseAll()
	hook := c.onHello
	c.notifyLocked()
	c.mu.Unlock()
	if hook != nil {
		hook(h)
	}
	writeJSON(w, http.StatusOK, map[string]string{"contract_version": contract.Version})
}
