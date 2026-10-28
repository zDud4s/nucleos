package serve

import (
	"bufio"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"nucleosremote/config"
)

const (
	g2Key     = "remote.KEYSECRET123"
	g2Session = "nos_device_SESSIONSECRET456"
)

// g2Daemon starts a fake daemon on loopback.
func g2Daemon(t *testing.T, h http.HandlerFunc) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	return srv
}

func g2Config(daemonURL string) config.Config {
	return config.Config{
		DaemonURL:    daemonURL,
		DaemonToken:  g2Key,
		PublicOrigin: testOrigin,
		PublicHost:   testHost,
		DistDir:      "",
		StreamMax:    30 * time.Second,
		CookieMaxAge: 2592000,
		MaxBodyBytes: 1 << 20,
	}
}

// g2Gateway runs the real handler tree on a loopback listener.
func g2Gateway(t *testing.T, cfg config.Config) *httptest.Server {
	t.Helper()
	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(Build(cfg, Handlers{Auth: NewAuth(cfg), API: api, Static: NewStatic(cfg)}))
	t.Cleanup(srv.Close)
	return srv
}

var g2Client = &http.Client{
	CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
}

// g2Do sends a browser-like request: right Host, CSRF header, and Origin on unsafe methods.
func g2Do(t *testing.T, gw *httptest.Server, method, path, body string, hdr map[string]string) *http.Response {
	t.Helper()
	req, err := http.NewRequest(method, gw.URL+path, strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	req.Host = testHost
	req.Header.Set(CSRFHeader, "1")
	if method != http.MethodGet && method != http.MethodHead {
		req.Header.Set("Origin", testOrigin)
	}
	for k, v := range hdr {
		req.Header.Set(k, v)
	}
	resp, err := g2Client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

func g2Body(t *testing.T, resp *http.Response) string {
	t.Helper()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func g2Cookie(session string) map[string]string {
	return map[string]string{"Cookie": CookieName + "=" + session}
}

func TestMissingCookieIs401WithoutCallingDaemon(t *testing.T) {
	var calls atomic.Int32
	d := g2Daemon(t, func(http.ResponseWriter, *http.Request) { calls.Add(1) })
	gw := g2Gateway(t, g2Config(d.URL))

	resp := g2Do(t, gw, "GET", "/api/runs", "", nil)
	if resp.StatusCode != 401 {
		t.Fatalf("status = %d, want 401", resp.StatusCode)
	}
	if body := g2Body(t, resp); !strings.Contains(body, `"session_invalid"`) {
		t.Fatalf("body = %q", body)
	}
	// A cookie of another name is no cookie.
	resp = g2Do(t, gw, "GET", "/api/runs", "", map[string]string{"Cookie": "nucleos_session=" + g2Session})
	if resp.StatusCode != 401 {
		t.Fatalf("wrong-name cookie status = %d, want 401", resp.StatusCode)
	}
	if n := calls.Load(); n != 0 {
		t.Fatalf("daemon was called %d times, want 0", n)
	}
}

func TestInvalidSessionIs401AndClearsCookie(t *testing.T) {
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Set-Cookie", "evil=1")
		w.Header().Set("Www-Authenticate", "Bearer realm=daemon")
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(401)
		_, _ = w.Write([]byte(`{"refusal":"session_invalid"}`))
	})
	gw := g2Gateway(t, g2Config(d.URL))

	for _, path := range []string{"/api/runs", "/auth/session"} {
		resp := g2Do(t, gw, "GET", path, "", g2Cookie(g2Session))
		if resp.StatusCode != 401 {
			t.Fatalf("%s: status = %d, want 401", path, resp.StatusCode)
		}
		if body := g2Body(t, resp); !strings.Contains(body, `"error":"session_invalid"`) || strings.Contains(body, "refusal") {
			t.Fatalf("%s: body = %q", path, body)
		}
		cookies := resp.Cookies()
		if len(cookies) != 1 || cookies[0].Name != CookieName || cookies[0].MaxAge != -1 || cookies[0].Value != "" {
			t.Fatalf("%s: cookies = %+v, want the session cookie cleared and nothing else", path, cookies)
		}
		if resp.Header.Get("Www-Authenticate") != "" {
			t.Fatalf("%s: WWW-Authenticate leaked", path)
		}
	}
}

func TestPairSetsHostCookieWithFlags(t *testing.T) {
	var gotAuth, gotSession, gotBody string
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/remote/pair" || r.Method != "POST" {
			t.Errorf("daemon got %s %s", r.Method, r.URL.Path)
		}
		gotAuth = r.Header.Get("Authorization")
		gotSession = r.Header.Get(DaemonSessionHeader)
		b, _ := io.ReadAll(r.Body)
		gotBody = string(b)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"session":"` + g2Session + `","device_id":7,"name":"Phone","access_level":"admin","expires_at":"2026-12-01T00:00:00Z"}`))
	})
	gw := g2Gateway(t, g2Config(d.URL))

	resp := g2Do(t, gw, "POST", "/auth/pair", `{"code":"ABCDE-FGHJK","device_name":"Phone"}`, nil)
	if resp.StatusCode != 200 {
		t.Fatalf("status = %d, want 200", resp.StatusCode)
	}
	if gotAuth != "Bearer "+g2Key || gotSession != "" {
		t.Fatalf("daemon saw Authorization=%q session=%q", gotAuth, gotSession)
	}
	if !strings.Contains(gotBody, "ABCDE-FGHJK") || !strings.Contains(gotBody, "Phone") {
		t.Fatalf("daemon body = %q", gotBody)
	}
	cookies := resp.Cookies()
	if len(cookies) != 1 {
		t.Fatalf("cookies = %+v, want exactly one", cookies)
	}
	c := cookies[0]
	if c.Name != "__Host-nucleos_session" || c.Value != g2Session {
		t.Fatalf("cookie = %s=%s", c.Name, c.Value)
	}
	if !c.Secure || !c.HttpOnly || c.SameSite != http.SameSiteStrictMode || c.Path != "/" || c.Domain != "" || c.MaxAge != 2592000 {
		t.Fatalf("cookie flags wrong: %+v", c)
	}
	body := g2Body(t, resp)
	for _, want := range []string{`"device_id":7`, `"name":"Phone"`, `"access_level":"admin"`, `"expires_at"`} {
		if !strings.Contains(body, want) {
			t.Fatalf("body %q lacks %s", body, want)
		}
	}
	if strings.Contains(body, g2Session) || strings.Contains(body, "session") {
		t.Fatalf("session secret in the body: %q", body)
	}

	// A bad code stays a plain 401 invalid_code, with no cookie.
	d2 := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(401)
		_, _ = w.Write([]byte(`{"refusal":"invalid_code"}`))
	})
	gw2 := g2Gateway(t, g2Config(d2.URL))
	resp = g2Do(t, gw2, "POST", "/auth/pair", `{"code":"WRONG"}`, nil)
	if resp.StatusCode != 401 || !strings.Contains(g2Body(t, resp), `"invalid_code"`) || len(resp.Cookies()) != 0 {
		t.Fatalf("bad code: status=%d cookies=%v", resp.StatusCode, resp.Cookies())
	}
}

func TestCredentialNeverInABrowserResponse(t *testing.T) {
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		// A careless daemon that echoes what it was given and tries to set a cookie.
		w.Header().Set("Set-Cookie", "leak="+g2Key)
		w.Header().Set(DaemonSessionHeader, g2Session)
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/remote/pair":
			_, _ = w.Write([]byte(`{"session":"` + g2Session + `","device_id":1,"name":"n","access_level":"admin","expires_at":"x"}`))
		case "/remote/session":
			_, _ = w.Write([]byte(`{"session":"` + g2Session + `","device_id":1,"name":"n","access_level":"admin","expires_at":"x"}`))
		default:
			_, _ = w.Write([]byte(`{"ok":true}`))
		}
	})
	gw := g2Gateway(t, g2Config(d.URL))

	dump := func(resp *http.Response) (headers, body string) {
		var h strings.Builder
		for k, vs := range resp.Header {
			if k == "Set-Cookie" {
				continue
			}
			h.WriteString(k + ": " + strings.Join(vs, ",") + "\n")
		}
		return h.String(), g2Body(t, resp)
	}

	pair := g2Do(t, gw, "POST", "/auth/pair", `{"code":"X"}`, nil)
	headers, body := dump(pair)
	if strings.Contains(headers+body, g2Key) || strings.Contains(headers+body, g2Session) {
		t.Fatalf("pair leaked outside Set-Cookie: %s %s", headers, body)
	}
	sc := pair.Header.Values("Set-Cookie")
	if len(sc) != 1 || !strings.Contains(sc[0], g2Session) || strings.Contains(sc[0], g2Key) {
		t.Fatalf("pair Set-Cookie = %v", sc)
	}

	for _, path := range []string{"/auth/session", "/api/runs", "/api/remote/devices", "/nothing", "/auth/pair"} {
		resp := g2Do(t, gw, "GET", path, "", g2Cookie(g2Session))
		headers, body := dump(resp)
		if strings.Contains(headers+body, g2Key) || strings.Contains(headers+body, g2Session) {
			t.Fatalf("%s leaked: %s %s", path, headers, body)
		}
		if len(resp.Header.Values("Set-Cookie")) != 0 {
			t.Fatalf("%s set a cookie: %v", path, resp.Header.Values("Set-Cookie"))
		}
	}
}

func TestProxyStripsInboundCredentialsAndTailscaleHeaders(t *testing.T) {
	var got http.Header
	var gotPath, gotQuery, gotMethod, gotBody string
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		got = r.Header.Clone()
		gotPath, gotQuery, gotMethod = r.URL.Path, r.URL.RawQuery, r.Method
		b, _ := io.ReadAll(r.Body)
		gotBody = string(b)
		_, _ = w.Write([]byte("{}"))
	})
	gw := g2Gateway(t, g2Config(d.URL))

	resp := g2Do(t, gw, "POST", "/api/runs/3/cancel?x=1", `{"a":1}`, map[string]string{
		"Cookie":                   CookieName + "=" + g2Session + "; other=tracker",
		"Authorization":            "Bearer browser-supplied-control-token",
		"X-Nucleos-Device-Session": "forged",
		"Tailscale-User-Login":     "someone@example.com",
		"Tailscale-User-Name":      "Someone",
		"X-Forwarded-For":          "1.2.3.4",
		"X-Forwarded-Host":         "evil.example",
		"Forwarded":                "for=1.2.3.4",
		"X-Real-Ip":                "1.2.3.4",
		"X-Nucleos-Run-Id":         "42",
		"X-Nucleos-Chat-Open":      "1",
	})
	if resp.StatusCode != 200 {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	if gotMethod != "POST" || gotPath != "/runs/3/cancel" || gotQuery != "x=1" || gotBody != `{"a":1}` {
		t.Fatalf("daemon saw %s %s ?%s body=%q", gotMethod, gotPath, gotQuery, gotBody)
	}
	if got.Get("Authorization") != "Bearer "+g2Key {
		t.Fatalf("Authorization = %q, want the gateway key", got.Get("Authorization"))
	}
	if got.Get(DaemonSessionHeader) != g2Session {
		t.Fatalf("session header = %q, want the cookie value", got.Get(DaemonSessionHeader))
	}
	if len(got.Values("Authorization")) != 1 || len(got.Values(DaemonSessionHeader)) != 1 {
		t.Fatalf("duplicated credential headers: %v", got)
	}
	for name := range got {
		lower := strings.ToLower(name)
		if lower == "cookie" || strings.HasPrefix(lower, "tailscale-") || strings.HasPrefix(lower, "x-forwarded-") ||
			lower == "forwarded" || lower == "x-real-ip" || lower == strings.ToLower(CSRFHeader) ||
			(strings.HasPrefix(lower, "x-nucleos-") && lower != strings.ToLower(DaemonSessionHeader)) {
			t.Errorf("daemon received %s: %v", name, got[name])
		}
	}
}

func TestSSEPassthroughIsNotBuffered(t *testing.T) {
	release := make(chan struct{})
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(200)
		_, _ = w.Write([]byte("data: one\n\n"))
		w.(http.Flusher).Flush()
		select {
		case <-release:
		case <-r.Context().Done():
		}
	})
	gw := g2Gateway(t, g2Config(d.URL))
	defer close(release)

	resp := g2Do(t, gw, "GET", "/api/events", "", g2Cookie(g2Session))
	if resp.StatusCode != 200 {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	if resp.Header.Get("X-Accel-Buffering") != "no" {
		t.Errorf("X-Accel-Buffering = %q", resp.Header.Get("X-Accel-Buffering"))
	}
	first := make(chan string, 1)
	go func() {
		line, _ := bufio.NewReader(resp.Body).ReadString('\n')
		first <- line
	}()
	select {
	case line := <-first:
		if line != "data: one\n" {
			t.Fatalf("first line = %q", line)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the first event did not reach the client while the daemon handler was still running")
	}
}

func TestStreamIsCutAtTheLifetimeCap(t *testing.T) {
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(200)
		for {
			if _, err := w.Write([]byte("data: tick\n\n")); err != nil {
				return
			}
			w.(http.Flusher).Flush()
			select {
			case <-r.Context().Done():
				return
			case <-time.After(100 * time.Millisecond):
			}
		}
	})
	cfg := g2Config(d.URL)
	cfg.StreamMax = time.Second
	gw := g2Gateway(t, cfg)

	start := time.Now()
	resp := g2Do(t, gw, "GET", "/api/events", "", g2Cookie(g2Session))
	done := make(chan error, 1)
	var read []byte
	go func() {
		var err error
		read, err = io.ReadAll(resp.Body)
		done <- err
	}()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("the cut must look like a clean end of body, got %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the stream was not cut")
	}
	if elapsed := time.Since(start); elapsed < 900*time.Millisecond {
		t.Fatalf("cut after %v, before the cap", elapsed)
	}
	if !strings.Contains(string(read), "data: tick") {
		t.Fatalf("no events before the cut: %q", read)
	}
}

func TestUpstreamDownIs502(t *testing.T) {
	d := httptest.NewServer(http.NotFoundHandler())
	url := d.URL
	d.Close()
	gw := g2Gateway(t, g2Config(url))

	for _, path := range []string{"/api/runs", "/auth/session"} {
		resp := g2Do(t, gw, "GET", path, "", g2Cookie(g2Session))
		if resp.StatusCode != 502 || !strings.Contains(g2Body(t, resp), `"daemon_unreachable"`) {
			t.Fatalf("%s: status = %d", path, resp.StatusCode)
		}
	}
	resp := g2Do(t, gw, "POST", "/auth/pair", `{"code":"X"}`, nil)
	if resp.StatusCode != 502 {
		t.Fatalf("pair: status = %d", resp.StatusCode)
	}
}

func TestUpstreamPlain401Is502(t *testing.T) {
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Www-Authenticate", "Bearer")
		w.Header().Set("Set-Cookie", "evil=1")
		w.WriteHeader(401)
		_, _ = w.Write([]byte(`{"refusal":"unauthorized"}`))
	})
	gw := g2Gateway(t, g2Config(d.URL))

	for _, path := range []string{"/api/runs", "/auth/session"} {
		resp := g2Do(t, gw, "GET", path, "", g2Cookie(g2Session))
		if resp.StatusCode != 502 {
			t.Fatalf("%s: status = %d, want 502", path, resp.StatusCode)
		}
		if body := g2Body(t, resp); !strings.Contains(body, `"gateway_credential_rejected"`) {
			t.Fatalf("%s: body = %q", path, body)
		}
		if resp.Header.Get("Www-Authenticate") != "" || len(resp.Header.Values("Set-Cookie")) != 0 {
			t.Fatalf("%s: upstream headers leaked: %v", path, resp.Header)
		}
	}
}

func TestRemotePrefixIsNotProxied(t *testing.T) {
	var calls atomic.Int32
	d := g2Daemon(t, func(http.ResponseWriter, *http.Request) { calls.Add(1) })
	gw := g2Gateway(t, g2Config(d.URL))

	for _, tc := range [][2]string{
		{"GET", "/api/remote/devices"},
		{"POST", "/api/remote/pairing-codes"},
		{"DELETE", "/api/remote/devices/1"},
		{"GET", "/api/remote"},
		{"POST", "/api/remote/pair"},
		{"GET", "/api/%72emote/devices"},
		{"GET", "/api"},
		{"GET", "/auth"},
	} {
		resp := g2Do(t, gw, tc[0], tc[1], "", g2Cookie(g2Session))
		if resp.StatusCode != 404 {
			t.Errorf("%s %s: status = %d, want 404", tc[0], tc[1], resp.StatusCode)
		}
	}
	if n := calls.Load(); n != 0 {
		t.Fatalf("daemon was called %d times, want 0", n)
	}
}
