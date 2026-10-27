package serve

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"strings"
)

// CSRFHeader must be present, with value "1", on every /api and /auth request. A cross-site form or
// simple fetch cannot add a custom header without a CORS preflight the gateway never answers.
const (
	CSRFHeader = "X-Nucleos-Csrf"
	csrfValue  = "1"
)

type ctxKey int

const nonceKey ctxKey = 0

// newNonce returns 128 random bits, base64url without padding.
func newNonce() string {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		// No entropy means no safe response; crypto/rand failing is not recoverable.
		panic("remote gateway: crypto/rand failed: " + err.Error())
	}
	return base64.RawURLEncoding.EncodeToString(b)
}

// NonceFrom returns the CSP nonce securityHeaders minted for this request ("" outside it).
func NonceFrom(ctx context.Context) string {
	n, _ := ctx.Value(nonceKey).(string)
	return n
}

// csp builds the Content-Security-Policy of the contract for one nonce.
func csp(nonce string) string {
	return "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'nonce-" + nonce +
		"'; font-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; " +
		"worker-src 'self' blob:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'"
}

// isDynamic reports whether a path is the authenticated surface, which must never be cached.
func isDynamic(path string) bool {
	return path == "/api" || path == "/auth" || strings.HasPrefix(path, "/api/") || strings.HasPrefix(path, "/auth/")
}

// securityHeaders sets the contract's headers before the handler runs, so they are on every
// response, error responses included, and exposes the per-request nonce through the context.
func securityHeaders(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		nonce := newNonce()
		h := w.Header()
		h.Set("Content-Security-Policy", csp(nonce))
		h.Set("Referrer-Policy", "no-referrer")
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("Strict-Transport-Security", "max-age=63072000; includeSubDomains")
		h.Set("X-Frame-Options", "DENY")
		h.Set("Cross-Origin-Resource-Policy", "same-origin")
		if isDynamic(r.URL.Path) {
			h.Set("Cache-Control", "no-store")
		}
		next.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), nonceKey, nonce)))
	})
}

// Policy is what a request is checked against.
type Policy struct {
	// PublicOrigin is the one origin allowed in the Origin header.
	PublicOrigin string
	// PublicHost is the one value allowed in the Host header.
	PublicHost string
}

// refusal is a rejected request: the status and the JSON error code.
type refusal struct {
	status int
	code   string
}

// checkRequest applies the gateway rules. Host is always checked; Sec-Fetch-Site, Origin and the
// CSRF header only when strict (the /api and /auth surface - static files are exempt). A nil result
// means the request may proceed.
//
// Nothing here reads a Tailscale-* identity header: who is on the tailnet is not who is allowed in.
func checkRequest(r *http.Request, p Policy, strict bool) *refusal {
	if !strings.EqualFold(r.Host, p.PublicHost) {
		return &refusal{http.StatusMisdirectedRequest, "bad_host"}
	}
	if !strict {
		return nil
	}
	if site := r.Header.Get("Sec-Fetch-Site"); site != "" && site != "same-origin" {
		return &refusal{http.StatusForbidden, "cross_site"}
	}
	if origin, present := r.Header["Origin"]; present {
		if len(origin) != 1 || origin[0] != p.PublicOrigin {
			return &refusal{http.StatusForbidden, "bad_origin"}
		}
	} else if r.Method != http.MethodGet && r.Method != http.MethodHead {
		return &refusal{http.StatusForbidden, "bad_origin"}
	}
	if r.Header.Get(CSRFHeader) != csrfValue {
		return &refusal{http.StatusForbidden, "csrf"}
	}
	return nil
}

// writeJSONError answers {"error": code} with the given status.
func writeJSONError(w http.ResponseWriter, status int, code string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]string{"error": code})
}

// guard runs checkRequest in front of next.
func guard(p Policy, strict bool, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if ref := checkRequest(r, p, strict); ref != nil {
			writeJSONError(w, ref.status, ref.code)
			return
		}
		next.ServeHTTP(w, r)
	})
}
