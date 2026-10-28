package serve

import (
	"net/http"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestPairIsRateLimited(t *testing.T) {
	var calls atomic.Int32
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		w.WriteHeader(401)
		_, _ = w.Write([]byte(`{"refusal":"invalid_code"}`))
	})
	gw := g2Gateway(t, g2Config(d.URL))

	for i := 0; i < 10; i++ {
		resp := g2Do(t, gw, "POST", "/auth/pair", `{"code":"WRONG"}`, nil)
		if resp.StatusCode != 401 {
			t.Fatalf("attempt %d: status = %d, want 401", i+1, resp.StatusCode)
		}
	}
	resp := g2Do(t, gw, "POST", "/auth/pair", `{"code":"WRONG"}`, nil)
	if resp.StatusCode != 429 {
		t.Fatalf("11th attempt: status = %d, want 429", resp.StatusCode)
	}
	if secs, err := strconv.Atoi(resp.Header.Get("Retry-After")); err != nil || secs < 1 || secs > 60 {
		t.Fatalf("Retry-After = %q", resp.Header.Get("Retry-After"))
	}
	if body := g2Body(t, resp); !strings.Contains(body, `"slow_down"`) {
		t.Fatalf("body = %q", body)
	}
	if n := calls.Load(); n != 10 {
		t.Fatalf("daemon was called %d times, want 10 (the limited attempt must not reach it)", n)
	}
}

func TestLimiterWindowSlides(t *testing.T) {
	l := newLimiter(2, time.Minute)
	now := time.Unix(1000, 0)
	l.now = func() time.Time { return now }
	for i := 0; i < 2; i++ {
		if ok, _ := l.allow(); !ok {
			t.Fatalf("event %d refused", i)
		}
	}
	if ok, wait := l.allow(); ok || wait != time.Minute {
		t.Fatalf("third event: ok=%v wait=%v", ok, wait)
	}
	now = now.Add(61 * time.Second)
	if ok, _ := l.allow(); !ok {
		t.Fatal("the window did not slide")
	}
}

func TestLogoutAlwaysClearsTheCookie(t *testing.T) {
	var logoutSession string
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/remote/logout" {
			logoutSession = r.Header.Get(DaemonSessionHeader)
		}
		w.WriteHeader(204)
	})
	gw := g2Gateway(t, g2Config(d.URL))

	resp := g2Do(t, gw, "POST", "/auth/logout", "", g2Cookie(g2Session))
	if resp.StatusCode != 204 {
		t.Fatalf("status = %d, want 204", resp.StatusCode)
	}
	if logoutSession != g2Session {
		t.Fatalf("daemon logout saw session %q", logoutSession)
	}
	if c := resp.Cookies(); len(c) != 1 || c[0].MaxAge != -1 {
		t.Fatalf("cookies = %+v", c)
	}

	// Without a cookie there is nothing to revoke, and the answer is the same.
	logoutSession = ""
	resp = g2Do(t, gw, "POST", "/auth/logout", "", nil)
	if resp.StatusCode != 204 || logoutSession != "" {
		t.Fatalf("no-cookie logout: status=%d session=%q", resp.StatusCode, logoutSession)
	}
}
