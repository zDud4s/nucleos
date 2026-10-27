package serve

import (
	"net/http"
	"net/http/httptest"
	"regexp"
	"strings"
	"testing"

	"nucleosremote/config"
)

const (
	testOrigin = "https://box.tail1234.ts.net"
	testHost   = "box.tail1234.ts.net"
)

func testConfig() config.Config {
	return config.Config{PublicOrigin: testOrigin, PublicHost: testHost}
}

var ok200 = http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
	w.WriteHeader(http.StatusOK)
})

func do(h http.Handler, method, path string, hdr map[string]string) *httptest.ResponseRecorder {
	req := httptest.NewRequest(method, path, nil)
	req.Host = testHost
	for k, v := range hdr {
		req.Header.Set(k, v)
	}
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	return rec
}

func handler() http.Handler {
	return Build(testConfig(), Handlers{Auth: ok200, API: ok200, Static: ok200})
}

func TestOriginAndCsrfRefusal(t *testing.T) {
	h := handler()
	csrf := map[string]string{CSRFHeader: "1"}
	with := func(extra map[string]string) map[string]string {
		m := map[string]string{CSRFHeader: "1"}
		for k, v := range extra {
			m[k] = v
		}
		return m
	}
	cases := []struct {
		name   string
		method string
		path   string
		hdr    map[string]string
		status int
		code   string
	}{
		{"get same origin", "GET", "/api/x", csrf, 200, ""},
		{"get with matching origin", "GET", "/api/x", with(map[string]string{"Origin": testOrigin}), 200, ""},
		{"post with matching origin", "POST", "/auth/pair", with(map[string]string{"Origin": testOrigin}), 200, ""},
		{"post without origin", "POST", "/auth/pair", csrf, 403, "bad_origin"},
		{"delete without origin", "DELETE", "/api/x", csrf, 403, "bad_origin"},
		{"foreign origin", "GET", "/api/x", with(map[string]string{"Origin": "https://evil.example"}), 403, "bad_origin"},
		{"http origin", "POST", "/api/x", with(map[string]string{"Origin": "http://box.tail1234.ts.net"}), 403, "bad_origin"},
		{"null origin", "POST", "/api/x", with(map[string]string{"Origin": "null"}), 403, "bad_origin"},
		{"missing csrf on get", "GET", "/api/x", nil, 403, "csrf"},
		{"missing csrf on auth", "GET", "/auth/session", nil, 403, "csrf"},
		{"wrong csrf value", "GET", "/api/x", map[string]string{CSRFHeader: "0"}, 403, "csrf"},
		{"static needs no csrf", "GET", "/index.html", nil, 200, ""},
	}
	for _, c := range cases {
		rec := do(h, c.method, c.path, c.hdr)
		if rec.Code != c.status {
			t.Errorf("%s: status %d, want %d", c.name, rec.Code, c.status)
			continue
		}
		if c.code != "" && !strings.Contains(rec.Body.String(), `"error":"`+c.code+`"`) {
			t.Errorf("%s: body %q lacks error %q", c.name, rec.Body.String(), c.code)
		}
	}

	// A wrong Host is 421 on every surface, static included.
	req := httptest.NewRequest("GET", "/", nil)
	req.Host = "evil.example"
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	if rec.Code != http.StatusMisdirectedRequest {
		t.Errorf("wrong host: status %d, want 421", rec.Code)
	}
}

func TestCrossSiteFetchMetadataRefused(t *testing.T) {
	h := handler()
	for _, site := range []string{"cross-site", "same-site", "none"} {
		rec := do(h, "GET", "/api/x", map[string]string{CSRFHeader: "1", "Sec-Fetch-Site": site})
		if rec.Code != 403 || !strings.Contains(rec.Body.String(), `"error":"cross_site"`) {
			t.Errorf("Sec-Fetch-Site %q: %d %q", site, rec.Code, rec.Body.String())
		}
	}
	rec := do(h, "GET", "/api/x", map[string]string{CSRFHeader: "1", "Sec-Fetch-Site": "same-origin"})
	if rec.Code != 200 {
		t.Errorf("same-origin refused: %d", rec.Code)
	}
}

func TestSecurityHeadersOnEveryResponse(t *testing.T) {
	h := handler()
	responses := []*httptest.ResponseRecorder{
		do(h, "GET", "/", nil),
		do(h, "GET", "/api/x", map[string]string{CSRFHeader: "1"}),
		do(h, "GET", "/api/x", nil),
		do(Build(testConfig(), Handlers{}), "GET", "/", nil),
	}
	bad := httptest.NewRequest("GET", "/", nil)
	bad.Host = "evil.example"
	badRec := httptest.NewRecorder()
	h.ServeHTTP(badRec, bad) // 421
	responses = append(responses, badRec)

	want := map[string]string{
		"Referrer-Policy":              "no-referrer",
		"X-Content-Type-Options":       "nosniff",
		"Strict-Transport-Security":    "max-age=63072000; includeSubDomains",
		"X-Frame-Options":              "DENY",
		"Cross-Origin-Resource-Policy": "same-origin",
	}
	for i, rec := range responses {
		for k, v := range want {
			if got := rec.Header().Get(k); got != v {
				t.Errorf("response %d: %s = %q, want %q", i, k, got, v)
			}
		}
		policy := rec.Header().Get("Content-Security-Policy")
		for _, part := range []string{
			"default-src 'none'", "script-src 'self' 'wasm-unsafe-eval'", "font-src 'self'",
			"img-src 'self' data: blob:", "media-src 'self' blob:", "connect-src 'self'",
			"worker-src 'self' blob:", "object-src 'none'", "base-uri 'self'",
			"frame-ancestors 'none'", "form-action 'self'",
		} {
			if !strings.Contains(policy, part) {
				t.Errorf("response %d: CSP lacks %q: %s", i, part, policy)
			}
		}
		if !regexp.MustCompile(`style-src 'self' 'nonce-[A-Za-z0-9_-]{22}'`).MatchString(policy) {
			t.Errorf("response %d: CSP style-src has no 128-bit nonce: %s", i, policy)
		}
	}
	if cc := responses[1].Header().Get("Cache-Control"); cc != "no-store" {
		t.Errorf("/api Cache-Control = %q", cc)
	}
	if cc := do(h, "GET", "/auth/session", map[string]string{CSRFHeader: "1"}).Header().Get("Cache-Control"); cc != "no-store" {
		t.Errorf("/auth Cache-Control = %q", cc)
	}
	if cc := responses[0].Header().Get("Cache-Control"); cc == "no-store" {
		t.Errorf("static must not be forced no-store")
	}
}

func TestCspNonceIsFreshPerRequest(t *testing.T) {
	var seen []string
	h := Build(testConfig(), Handlers{Static: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = append(seen, NonceFrom(r.Context()))
	})})
	re := regexp.MustCompile(`'nonce-([A-Za-z0-9_-]+)'`)
	headerNonces := map[string]bool{}
	for i := 0; i < 50; i++ {
		rec := do(h, "GET", "/", nil)
		m := re.FindStringSubmatch(rec.Header().Get("Content-Security-Policy"))
		if m == nil {
			t.Fatal("no nonce in CSP")
		}
		headerNonces[m[1]] = true
		if seen[i] != m[1] {
			t.Fatalf("context nonce %q differs from the header's %q", seen[i], m[1])
		}
	}
	if len(headerNonces) != 50 {
		t.Errorf("only %d distinct nonces in 50 requests", len(headerNonces))
	}
	if NonceFrom(httptest.NewRequest("GET", "/", nil).Context()) != "" {
		t.Error("a nonce appeared outside securityHeaders")
	}
}
