package serve

import (
	"bufio"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

// trickleStatus sends a POST whose body stops short of its Content-Length and reports the status the
// server answers with ("" if it just closed the connection), and how long that took.
func trickleStatus(t *testing.T, addr, path string) (string, time.Duration) {
	t.Helper()
	conn, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	start := time.Now()
	_, _ = conn.Write([]byte("POST " + path + " HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"code\":"))
	_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
	line, _ := bufio.NewReader(conn).ReadString('\n')
	return strings.TrimSpace(line), time.Since(start)
}

func TestSlowRequestBodyIsAborted(t *testing.T) {
	const bound = 200 * time.Millisecond
	var calls atomic.Int32
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) {
		// Only pairing is counted: the proxied (aborted) API request may legitimately reach the daemon.
		if r.URL.Path == "/remote/pair" {
			calls.Add(1)
		}
		w.WriteHeader(401)
		_, _ = w.Write([]byte(`{"refusal":"invalid_code"}`))
	})
	cfg := g2Config(d.URL)
	auth := NewAuth(cfg).(*authHandler)
	auth.bodyTimeout = bound
	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatal(err)
	}
	api.(*apiHandler).bodyTimeout = bound

	mux := http.NewServeMux()
	mux.Handle("/auth/", auth)
	mux.Handle("/api/", api)
	srv := httptest.NewServer(mux)
	defer srv.Close()
	addr := srv.Listener.Addr().String()

	// A body that never completes is cut off at the bound instead of holding the connection.
	status, took := trickleStatus(t, addr, "/auth/pair")
	if took > 3*time.Second {
		t.Fatalf("auth: slow body held the connection for %v", took)
	}
	if status != "" && !strings.Contains(status, "400") {
		t.Fatalf("auth: status line = %q, want 400 or a closed connection", status)
	}
	if n := calls.Load(); n != 0 {
		t.Fatalf("auth: daemon reached %d times by an incomplete body", n)
	}

	// The proxy path needs a session cookie to get as far as the body.
	conn, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	start := time.Now()
	_, _ = conn.Write([]byte("POST /api/x HTTP/1.1\r\nHost: x\r\nCookie: " + CookieName + "=abc\r\nContent-Length: 100\r\n\r\nab"))
	_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, _ = bufio.NewReader(conn).ReadString('\n')
	if took := time.Since(start); took > 3*time.Second {
		t.Fatalf("api: slow body held the connection for %v", took)
	}

	// A normal POST, with the bound still in force, goes through.
	resp, err := http.Post(srv.URL+"/auth/pair", "application/json", strings.NewReader(`{"code":"WRONG"}`))
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != 401 {
		t.Fatalf("normal POST: status = %d, want 401", resp.StatusCode)
	}
	if n := calls.Load(); n != 1 {
		t.Fatalf("daemon calls = %d, want 1", n)
	}
}

// closesWithin sends a POST whose body stops short of its Content-Length, reads the whole answer, and
// reports whether the server then closed the connection within limit instead of draining the body
// forever.
func closesWithin(t *testing.T, addr, request string, limit time.Duration) bool {
	t.Helper()
	conn, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	_, _ = conn.Write([]byte(request))
	_ = conn.SetReadDeadline(time.Now().Add(limit))
	_, err = io.ReadAll(conn)
	return err == nil // EOF: the server closed; a timeout means it still held the connection
}

func TestRefusedRequestWithSlowBodyDoesNotHoldTheConnection(t *testing.T) {
	const bound = 200 * time.Millisecond
	d := g2Daemon(t, func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(500) })
	cfg := g2Config(d.URL)
	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatal(err)
	}
	api.(*apiHandler).bodyTimeout = bound
	old := guardBodyTimeout
	guardBodyTimeout = bound
	defer func() { guardBodyTimeout = old }()

	mux := http.NewServeMux()
	mux.Handle("/api/", api)
	mux.Handle("/guarded/", guard(Policy{PublicOrigin: "https://never.example", PublicHost: "never.example"}, true, api))
	srv := httptest.NewServer(mux)
	defer srv.Close()
	addr := srv.Listener.Addr().String()

	// No session: 401 without reading the body; the drain afterwards must be bounded.
	if !closesWithin(t, addr, "POST /api/x HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\nab", 3*time.Second) {
		t.Fatal("api without a session: the slow body held the connection")
	}
	// Refused by guard (wrong Host) before any handler: same.
	if !closesWithin(t, addr, "POST /guarded/x HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\nab", 3*time.Second) {
		t.Fatal("guard refusal: the slow body held the connection")
	}
}
