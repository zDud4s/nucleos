// Package serve is this sidecar's only inbound surface.
//
// It binds to loopback, requires the daemon's token, and writes nothing anywhere. The núcleo asks
// for the current quota; this process answers and forgets. The one piece of state it keeps is a
// time-boxed cache of the last answer, and that exists to protect the vendor's endpoint from the
// núcleo's polling — not to remember anything across a restart.
package serve

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"log"
	"net/http"
	"strings"
	"sync"
	"time"

	"nucleosquota/claude"
	"nucleosquota/codex"
	"nucleosquota/config"
	"nucleosquota/gemini"
	"nucleosquota/reading"
)

// HeaderTimeout bounds how long a client may take to send its headers. Small, because the only
// legitimate client is on the same machine.
const HeaderTimeout = 10 * time.Second

// Response is what the núcleo gets from /quota.
type Response struct {
	Providers []reading.Provider `json:"providers"`
	// Cached says the answer came from the TTL window rather than from a fresh read. The núcleo
	// shows the age of a reading, and a caller that cannot tell a cache hit from a fresh call
	// cannot do that honestly.
	Cached bool `json:"cached"`
	// Accounts holds one reading per account once the núcleo has pushed an account list (PUT
	// /accounts), in the list's order, each with account_id set. Providers is then empty: the
	// legacy default-dir readings are not repeated beside the per-account ones.
	Accounts []reading.Provider `json:"accounts"`
}

// Readers are the per-provider sources. An interface pair rather than concrete types so the tests
// can serve fixtures without a network or a home directory.
type Readers struct {
	Claude func(ctx context.Context, now time.Time) reading.Provider
	Codex  func(now time.Time) reading.Provider
	// Gemini answers false when there is no subscription to report on, and the provider is left
	// out of the answer rather than drawn dashed (package gemini). Nil reads as never present.
	Gemini func(ctx context.Context, now time.Time) (reading.Provider, bool)
	// Account reads one pool account from its engine and config directory. Nil reads every account
	// as Unmeasured.
	Account func(ctx context.Context, a Account, now time.Time) reading.Provider
}

// Live builds the real readers from config.
func Live(cfg config.Config, home string) Readers {
	c := claude.New(cfg.FetchTimeout, home)
	x := codex.New(home)
	g := gemini.New(cfg.FetchTimeout, home)
	return Readers{Claude: c.Read, Codex: x.Read, Gemini: g.Read, Account: liveAccount(cfg)}
}

type cache struct {
	mu     sync.Mutex
	at     time.Time
	answer []reading.Provider
	// accountAnswer is the per-account half of the held answer; valid while answer is non-nil.
	accountAnswer []reading.Provider
	// accounts is the list the núcleo last pushed. accountsSet tells "never pushed" (legacy
	// behaviour: default account per provider) from "pushed an empty list" (no accounts at all).
	accounts    []Account
	accountsSet bool
	// ttl is the window the held answer is good for. Kept alongside the answer because it differs
	// by outcome: a failed read is retried sooner than a good one is refreshed.
	ttl time.Duration
}

// Serve blocks, answering the núcleo until the process ends.
func Serve(cfg config.Config, readers Readers) error {
	c := &cache{}
	mux := http.NewServeMux()
	mux.HandleFunc("/quota", authorized(cfg.DaemonToken, quotaHandler(cfg, readers, c, time.Now)))
	mux.HandleFunc("/accounts", authorized(cfg.DaemonToken, accountsHandler(c)))

	server := &http.Server{
		Addr:              cfg.Addr,
		Handler:           mux,
		ReadHeaderTimeout: HeaderTimeout,
	}
	log.Printf("serving quota on %s", cfg.Addr)
	return server.ListenAndServe()
}

func quotaHandler(
	cfg config.Config,
	readers Readers,
	c *cache,
	now func() time.Time,
) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}

		at := now()
		c.mu.Lock()
		defer c.mu.Unlock()

		if c.answer != nil && at.Sub(c.at) < c.ttl {
			writeJSON(w, Response{Providers: c.answer, Accounts: c.accountAnswer, Cached: true})
			return
		}

		read := []reading.Provider{}
		var accounts []reading.Provider
		if c.accountsSet {
			accounts = onTheWire(readAccounts(r.Context(), readers, c.accounts, at))
		} else {
			read = append(read, readers.Claude(r.Context(), at), readers.Codex(at))
			if readers.Gemini != nil {
				if p, present := readers.Gemini(r.Context(), at); present {
					read = append(read, p)
				}
			}
		}
		providers := onTheWire(read)
		if accounts == nil {
			accounts = []reading.Provider{}
		}

		// The shorter TTL wins whenever anything failed: a provider that is down should be retried
		// on the error cadence even while the other one is happily cached. Holding a bad answer for
		// a full minute is how a transient 502 becomes a minute of dashed rings.
		ttl := cfg.SuccessTTL
		for _, p := range append(append([]reading.Provider{}, providers...), accounts...) {
			if p.Fidelity == reading.Unmeasured {
				ttl = cfg.ErrorTTL
				break
			}
		}

		c.at, c.answer, c.accountAnswer, c.ttl = at, providers, accounts, ttl
		writeJSON(w, Response{Providers: providers, Accounts: accounts, Cached: false})
	}
}

// onTheWire gives every provider an empty window list where it has none, so it is sent as `[]`.
//
// encoding/json writes a nil slice as null, and reading.Unavailable — every provider that could not
// be read — leaves Windows nil. The núcleo decodes the field as a list, and a null there failed its
// whole decode: one rate-limited provider made the other one's live figures read as last-known
// (2026-09-24, while the usage endpoint answered 429). Done here, once, because this is the one
// place every reader's answer passes through on its way out, and before the answer is cached so a
// cache hit carries the same bytes as the fresh read that filled it.
func onTheWire(providers []reading.Provider) []reading.Provider {
	for i := range providers {
		if providers[i].Windows == nil {
			providers[i].Windows = []reading.Window{}
		}
	}
	return providers
}

// authorized wraps a handler with the bearer check. Constant-time, like the web and email sidecars'.
func authorized(token string, next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !hasToken(r, token) {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		next(w, r)
	}
}

func hasToken(r *http.Request, token string) bool {
	const prefix = "Bearer "
	header := r.Header.Get("Authorization")
	if !strings.HasPrefix(header, prefix) {
		return false
	}
	presented := strings.TrimPrefix(header, prefix)
	return subtle.ConstantTimeCompare([]byte(presented), []byte(token)) == 1
}

func writeJSON(w http.ResponseWriter, payload any) {
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(payload); err != nil {
		log.Printf("writing response: %v", err)
	}
}
