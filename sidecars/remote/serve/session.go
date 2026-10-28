package serve

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	"nucleosremote/config"
)

// Names fixed by the contract.
const (
	// CookieName is the session cookie. The __Host- prefix makes browsers insist on Secure, Path=/
	// and no Domain.
	CookieName = "__Host-nucleos_session"
	// DaemonSessionHeader carries the cookie's value to the daemon, which is what decides.
	DaemonSessionHeader = "X-Nucleos-Device-Session"

	maxAuthBody     = 4 << 10
	maxDaemonAnswer = 64 << 10
	pairLimit       = 10
	pairWindow      = time.Minute
	daemonTimeout   = 15 * time.Second
)

// refusalSessionInvalid and refusalInvalidCode are the daemon's refusal words.
const (
	refusalSessionInvalid = "session_invalid"
	refusalInvalidCode    = "invalid_code"
)

// writeJSON answers a JSON body with the given status.
func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

// sessionCookie builds the cookie of the contract. maxAge < 0 clears it.
func sessionCookie(cfg config.Config, value string, maxAge int) *http.Cookie {
	return &http.Cookie{
		Name:     CookieName,
		Value:    value,
		Path:     "/",
		MaxAge:   maxAge,
		Secure:   true,
		HttpOnly: true,
		SameSite: http.SameSiteStrictMode,
	}
}

// clearedCookie expires the session cookie in the browser.
func clearedCookie(cfg config.Config) *http.Cookie { return sessionCookie(cfg, "", -1) }

// validSecret reports whether a session secret is safe to place in a header and a cookie.
func validSecret(s string) bool {
	if s == "" || len(s) > 256 {
		return false
	}
	for _, c := range s {
		switch {
		case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9', c == '_', c == '-', c == '.':
		default:
			return false
		}
	}
	return true
}

// requestSession returns the session secret the browser presented, if any.
func requestSession(r *http.Request) (string, bool) {
	c, err := r.Cookie(CookieName)
	if err != nil || !validSecret(c.Value) {
		return "", false
	}
	return c.Value, true
}

// refusalOf extracts {"refusal": "..."} from a daemon body ("" if absent).
func refusalOf(body []byte) string {
	var v struct {
		Refusal string `json:"refusal"`
	}
	if json.Unmarshal(body, &v) != nil {
		return ""
	}
	return v.Refusal
}

// limiter allows at most `limit` events per sliding `window`, in memory. Every browser reaches
// the gateway from the same loopback peer, so the limit is global by design.
type limiter struct {
	mu     sync.Mutex
	limit  int
	window time.Duration
	now    func() time.Time
	events []time.Time
}

func newLimiter(limit int, window time.Duration) *limiter {
	return &limiter{limit: limit, window: window, now: time.Now}
}

// allow records an event and reports whether it fits; when it does not, wait is how long until it would.
func (l *limiter) allow() (ok bool, wait time.Duration) {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := l.now()
	kept := l.events[:0]
	for _, t := range l.events {
		if now.Sub(t) < l.window {
			kept = append(kept, t)
		}
	}
	l.events = kept
	if len(l.events) >= l.limit {
		return false, l.window - now.Sub(l.events[0])
	}
	l.events = append(l.events, now)
	return true, 0
}

// daemonClient calls the three /remote routes the gateway uses, with its own key.
type daemonClient struct {
	base  string
	token string
	hc    *http.Client
}

func newDaemonClient(cfg config.Config) *daemonClient {
	return &daemonClient{
		base:  cfg.DaemonURL,
		token: cfg.DaemonToken,
		hc: &http.Client{
			Transport: newTransport(),
			Timeout:   daemonTimeout,
			// A redirect would carry the key somewhere the gateway did not choose.
			CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
		},
	}
}

// call performs one daemon request. session is sent only when non-empty.
func (d *daemonClient) call(ctx context.Context, method, path string, body []byte, session string) (int, []byte, error) {
	var rd io.Reader
	if body != nil {
		rd = bytes.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, d.base+path, rd)
	if err != nil {
		return 0, nil, err
	}
	req.Header.Set("Authorization", "Bearer "+d.token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if session != "" {
		req.Header.Set(DaemonSessionHeader, session)
	}
	resp, err := d.hc.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxDaemonAnswer))
	if err != nil {
		return 0, nil, err
	}
	return resp.StatusCode, data, nil
}

// deviceInfo is what a browser may learn about its own device; the session secret is not in it.
type deviceInfo struct {
	DeviceID    json.RawMessage `json:"device_id"`
	Name        json.RawMessage `json:"name"`
	AccessLevel json.RawMessage `json:"access_level"`
	ExpiresAt   json.RawMessage `json:"expires_at"`
}

// authHandler serves /auth/*.
type authHandler struct {
	cfg config.Config
	d   *daemonClient
	lim *limiter
}

// NewAuth builds the /auth/* handler.
func NewAuth(cfg config.Config) http.Handler {
	return &authHandler{cfg: cfg, d: newDaemonClient(cfg), lim: newLimiter(pairLimit, pairWindow)}
}

func (a *authHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	switch r.URL.Path {
	case "/auth/pair":
		a.pair(w, r)
	case "/auth/session":
		a.session(w, r)
	case "/auth/logout":
		a.logout(w, r)
	default:
		writeJSONError(w, http.StatusNotFound, "not_found")
	}
}

func (a *authHandler) sessionLost(w http.ResponseWriter) {
	http.SetCookie(w, clearedCookie(a.cfg))
	writeJSONError(w, http.StatusUnauthorized, refusalSessionInvalid)
}

func (a *authHandler) pair(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeJSONError(w, http.StatusMethodNotAllowed, "method_not_allowed")
		return
	}
	if ok, wait := a.lim.allow(); !ok {
		secs := int((wait + time.Second - 1) / time.Second)
		if secs < 1 {
			secs = 1
		}
		w.Header().Set("Retry-After", strconv.Itoa(secs))
		writeJSONError(w, http.StatusTooManyRequests, "slow_down")
		return
	}
	var in struct {
		Code       string `json:"code"`
		DeviceName string `json:"device_name,omitempty"`
	}
	dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, maxAuthBody))
	if err := dec.Decode(&in); err != nil || strings.TrimSpace(in.Code) == "" {
		writeJSONError(w, http.StatusBadRequest, "bad_request")
		return
	}
	payload, _ := json.Marshal(in)
	status, data, err := a.d.call(r.Context(), http.MethodPost, "/remote/pair", payload, "")
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return
	}
	switch status {
	case http.StatusOK:
		var out struct {
			Session string `json:"session"`
			deviceInfo
		}
		if json.Unmarshal(data, &out) != nil || !validSecret(out.Session) {
			writeJSONError(w, http.StatusBadGateway, "daemon_error")
			return
		}
		// The secret travels only in this header; the body below is rebuilt field by field.
		http.SetCookie(w, sessionCookie(a.cfg, out.Session, a.cfg.CookieMaxAge))
		writeJSON(w, http.StatusOK, out.deviceInfo)
	case http.StatusUnauthorized:
		if refusalOf(data) == refusalInvalidCode {
			writeJSONError(w, http.StatusUnauthorized, refusalInvalidCode)
			return
		}
		writeJSONError(w, http.StatusBadGateway, "gateway_credential_rejected")
	default:
		writeJSONError(w, http.StatusBadGateway, "daemon_error")
	}
}

func (a *authHandler) session(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeJSONError(w, http.StatusMethodNotAllowed, "method_not_allowed")
		return
	}
	secret, ok := requestSession(r)
	if !ok {
		a.sessionLost(w)
		return
	}
	status, data, err := a.d.call(r.Context(), http.MethodGet, "/remote/session", nil, secret)
	if err != nil {
		writeJSONError(w, http.StatusBadGateway, "daemon_unreachable")
		return
	}
	switch status {
	case http.StatusOK:
		var info deviceInfo
		if json.Unmarshal(data, &info) != nil {
			writeJSONError(w, http.StatusBadGateway, "daemon_error")
			return
		}
		writeJSON(w, http.StatusOK, info)
	case http.StatusUnauthorized:
		if refusalOf(data) == refusalSessionInvalid {
			a.sessionLost(w)
			return
		}
		writeJSONError(w, http.StatusBadGateway, "gateway_credential_rejected")
	default:
		writeJSONError(w, http.StatusBadGateway, "daemon_error")
	}
}

func (a *authHandler) logout(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeJSONError(w, http.StatusMethodNotAllowed, "method_not_allowed")
		return
	}
	if secret, ok := requestSession(r); ok {
		// Best effort: the cookie is cleared whatever the daemon says, and a device that could not
		// be revoked here can still be revoked from the desktop.
		_, _, _ = a.d.call(r.Context(), http.MethodPost, "/remote/logout", nil, secret)
	}
	http.SetCookie(w, clearedCookie(a.cfg))
	w.WriteHeader(http.StatusNoContent)
}
