package serve

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"nucleosremote/config"
)

// nativeClient represents plugin-http: it speaks TLS directly and does not add browser fetch
// metadata or an Origin header. nativeRequest attaches its already-paired session explicitly.
func nativeClient(t *testing.T, gw *httptest.Server) *http.Client {
	t.Helper()
	client := gw.Client()
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	return client
}

func nativeRequest(t *testing.T, client *http.Client, gw *httptest.Server, method, path string, csrf bool) *http.Response {
	t.Helper()
	req, err := http.NewRequest(method, gw.URL+path, nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Host = testHost
	for name, value := range g2Cookie(g2Session) {
		req.Header.Set(name, value)
	}
	if csrf {
		req.Header.Set(CSRFHeader, csrfValue)
	}
	if req.Header.Get("Origin") != "" {
		t.Fatal("native request unexpectedly has an Origin header")
	}
	resp, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	return resp
}

func nativeGateway(t *testing.T, cfg config.Config) *httptest.Server {
	t.Helper()
	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewTLSServer(Build(cfg, Handlers{Auth: NewAuth(cfg), API: api, Static: ok200}))
	t.Cleanup(srv.Close)
	return srv
}

func TestNativeClientWithoutOrigin(t *testing.T) {
	policy := Policy{PublicOrigin: testOrigin, PublicHost: testHost}
	for _, tc := range []struct {
		name   string
		method string
		code   string
	}{
		{name: "GET is safe", method: http.MethodGet},
		{name: "HEAD is safe", method: http.MethodHead},
		{name: "POST changes state", method: http.MethodPost, code: "bad_origin"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			req := httptest.NewRequest(tc.method, "https://"+testHost+"/api/runs", nil)
			req.Host = testHost
			req.Header.Set(CSRFHeader, csrfValue)
			if req.Header.Get("Origin") != "" {
				t.Fatal("request unexpectedly has an Origin header")
			}
			ref := checkRequest(req, policy, true)
			if tc.code == "" {
				if ref != nil {
					t.Fatalf("checkRequest refused safe request: %+v", ref)
				}
				return
			}
			if ref == nil || ref.code != tc.code {
				t.Fatalf("refusal = %+v, want code %q", ref, tc.code)
			}
		})
	}
}

func TestNativeClientSessionAndCsrf(t *testing.T) {
	var daemonCalls atomic.Int32
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		daemonCalls.Add(1)
		if r.URL.Path != "/remote/session" {
			t.Errorf("daemon path = %q, want /remote/session", r.URL.Path)
		}
		if got := r.Header.Get(DaemonSessionHeader); got != g2Session {
			t.Errorf("daemon session = %q, want %q", got, g2Session)
		}
		if got := r.Header.Get("Origin"); got != "" {
			t.Errorf("daemon Origin = %q, want absent", got)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"device_id":1,"name":"laptop","access_level":"admin","expires_at":"2026-12-01T00:00:00Z"}`))
	})
	gw := nativeGateway(t, g2Config(d.URL))
	client := nativeClient(t, gw)

	resp := nativeRequest(t, client, gw, http.MethodGet, "/auth/session", true)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("session status = %d, want 200", resp.StatusCode)
	}
	_ = resp.Body.Close()
	if got := daemonCalls.Load(); got != 1 {
		t.Fatalf("daemon calls = %d, want 1", got)
	}

	resp = nativeRequest(t, client, gw, http.MethodGet, "/auth/session", false)
	body, err := io.ReadAll(resp.Body)
	resp.Body.Close()
	if err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusForbidden || !strings.Contains(string(body), `"error":"csrf"`) {
		t.Fatalf("missing CSRF response = %d %q, want 403 csrf", resp.StatusCode, body)
	}
	if got := daemonCalls.Load(); got != 1 {
		t.Fatalf("missing CSRF reached daemon %d times", got)
	}

	resp = nativeRequest(t, client, gw, http.MethodPost, "/api/runs/1/cancel", true)
	body, err = io.ReadAll(resp.Body)
	resp.Body.Close()
	if err != nil {
		t.Fatal(err)
	}
	if resp.StatusCode != http.StatusForbidden || !strings.Contains(string(body), `"error":"bad_origin"`) {
		t.Fatalf("originless write response = %d %q, want 403 bad_origin", resp.StatusCode, body)
	}
	if got := daemonCalls.Load(); got != 1 {
		t.Fatalf("originless write reached daemon %d times", got)
	}
}

func TestNativeClientStreamCutAndReconnect(t *testing.T) {
	var streams atomic.Int32
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		streams.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		for {
			if _, err := w.Write([]byte("data: tick\n\n")); err != nil {
				return
			}
			w.(http.Flusher).Flush()
			select {
			case <-r.Context().Done():
				return
			case <-time.After(5 * time.Millisecond):
			}
		}
	})
	cfg := g2Config(d.URL)
	cfg.StreamMax = 75 * time.Millisecond
	gw := nativeGateway(t, cfg)
	client := nativeClient(t, gw)

	readStream := func() []byte {
		started := time.Now()
		resp := nativeRequest(t, client, gw, http.MethodGet, "/api/fleet/stream", true)
		if resp.StatusCode != http.StatusOK {
			resp.Body.Close()
			t.Fatalf("stream status = %d, want 200", resp.StatusCode)
		}
		body, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil {
			t.Fatalf("stream ended with %v, want clean EOF", err)
		}
		if elapsed := time.Since(started); elapsed < 50*time.Millisecond || elapsed > time.Second {
			t.Fatalf("stream lifetime = %v, want the injected cap", elapsed)
		}
		return body
	}

	if body := readStream(); !strings.Contains(string(body), "data: tick") {
		t.Fatalf("first stream body = %q, want at least one event", body)
	}
	if body := readStream(); !strings.Contains(string(body), "data: tick") {
		t.Fatalf("reconnected stream body = %q, want at least one event", body)
	}
	if got := streams.Load(); got != 2 {
		t.Fatalf("upstream streams = %d, want immediate reconnect to open a second stream", got)
	}
}
