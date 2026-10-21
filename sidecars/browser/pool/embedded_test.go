// §spec pilar-de-browser

package pool

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// webview stands in for the person's WebView2: a CDP endpoint plus the DevTools HTTP routes.
type webview struct {
	cdp    *cdptest.Browser
	server *httptest.Server

	mu      sync.Mutex
	targets []map[string]string
}

func newWebview(t *testing.T) *webview {
	t.Helper()
	fake, err := cdptest.Start()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(fake.Close)
	// One CDP session per attached target, as Chrome does.
	fake.Handle("Target.attachToTarget", func(call cdptest.Call) (any, error) {
		var params struct {
			TargetID string `json:"targetId"`
		}
		_ = json.Unmarshal(call.Params, &params)
		return map[string]any{"sessionId": "SE-" + params.TargetID}, nil
	})
	fake.Handle("Target.getTargetInfo", func(call cdptest.Call) (any, error) {
		return map[string]any{"targetInfo": map[string]any{
			"targetId": "T1", "url": "https://example.org/landed", "title": "Example",
		}}, nil
	})

	view := &webview{cdp: fake}
	mux := http.NewServeMux()
	mux.HandleFunc("/json/list", func(w http.ResponseWriter, _ *http.Request) {
		view.mu.Lock()
		defer view.mu.Unlock()
		_ = json.NewEncoder(w).Encode(view.targets)
	})
	mux.HandleFunc("/json/version", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"webSocketDebuggerUrl": fake.URL})
	})
	view.server = httptest.NewServer(mux)
	t.Cleanup(view.server.Close)
	return view
}

func (v *webview) addPage(id, nonce string) {
	v.mu.Lock()
	defer v.mu.Unlock()
	v.targets = append(v.targets, map[string]string{"id": id, "type": "page", "url": "about:blank#" + nonce})
}

func attachRequest(v *webview, nonce string) browser.AttachRequest {
	return browser.AttachRequest{
		Project:   "p1",
		CDPHTTP:   v.server.URL,
		Nonce:     nonce,
		URL:       "https://example.org/",
		Placement: project("p1", "https://example.org"),
	}
}

func preparedPool(t *testing.T) (*Pool, string) {
	t.Helper()
	pool, _ := testPool(t, &fakeLauncher{}, 4)
	reply, err := pool.PrepareEmbedded(context.Background(), browser.PrepareRequest{
		Project: "p1", Placement: project("p1", "https://example.org"),
	})
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	return pool, reply.ProxyAddr
}

// stopsListening reports whether dialling addr starts failing within a second. Polled because a proxy
// closed right after it started can still hold its socket until its serve goroutine has run.
func stopsListening(addr string) bool {
	deadline := time.Now().Add(time.Second)
	for {
		conn, err := net.DialTimeout("tcp", addr, 200*time.Millisecond)
		if err != nil {
			return true
		}
		conn.Close()
		if time.Now().After(deadline) {
			return false
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestPrepareEmbeddedIsIdempotentPerProject(t *testing.T) {
	pool, first := preparedPool(t)
	again, err := pool.PrepareEmbedded(context.Background(), browser.PrepareRequest{
		Project: "p1", Placement: project("p1", "https://example.org"),
	})
	if err != nil {
		t.Fatal(err)
	}
	if again.ProxyAddr != first {
		t.Errorf("second prepare = %s, want %s", again.ProxyAddr, first)
	}
	other, err := pool.PrepareEmbedded(context.Background(), browser.PrepareRequest{
		Project: "p2", Placement: project("p2", "https://example.org"),
	})
	if err != nil {
		t.Fatal(err)
	}
	if other.ProxyAddr == first {
		t.Error("two projects share one proxy")
	}
}

func TestPrepareEmbeddedWithAnotherPolicyRecreatesWhenIdleAndRefusesWhenLive(t *testing.T) {
	view := newWebview(t)
	pool, first := preparedPool(t)
	changed := browser.PrepareRequest{Project: "p1", Placement: project("p1", "https://example.org", "https://other.example")}

	// Idle: recreated, and the old proxy is gone.
	second, err := pool.PrepareEmbedded(context.Background(), changed)
	if err != nil {
		t.Fatalf("idle policy change: %v", err)
	}
	if !stopsListening(first) {
		t.Error("the old proxy still listens")
	}
	if conn, err := net.DialTimeout("tcp", second.ProxyAddr, time.Second); err != nil {
		t.Errorf("the new proxy does not listen: %v", err)
	} else {
		conn.Close()
	}

	// Live: refused.
	view.addPage("T1", "n1")
	request := attachRequest(view, "n1")
	request.Placement = changed.Placement
	if _, err := pool.AttachEmbedded(context.Background(), request); err != nil {
		t.Fatalf("attach: %v", err)
	}
	_, err = pool.PrepareEmbedded(context.Background(), browser.PrepareRequest{
		Project: "p1", Placement: project("p1", "https://third.example"),
	})
	if !errors.Is(err, browser.ErrEmbeddedPolicyChanged) || !errors.Is(err, ErrPolicyChanged) {
		t.Errorf("err = %v, want ErrEmbeddedPolicyChanged", err)
	}
}

func TestAttachEmbeddedAdoptsTheTabAndTheSessionRoutesAndCloses(t *testing.T) {
	view := newWebview(t)
	view.addPage("T0", "other")
	view.addPage("T1", "n1")
	pool, _ := preparedPool(t)

	session, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n1"))
	if err != nil {
		t.Fatalf("attach: %v", err)
	}
	if session.ID == "" || session.RequestedURL != "https://example.org/" {
		t.Fatalf("session = %+v", session)
	}

	methods := view.cdp.Methods()
	attachAt, fetchAt, navigateAt := view.cdp.IndexOf("Target.attachToTarget"), view.cdp.IndexOf("Fetch.enable"), view.cdp.IndexOf("Page.navigate")
	if attachAt < 0 || fetchAt < 0 || navigateAt < 0 || fetchAt > navigateAt {
		t.Fatalf("calls out of order: %v", methods)
	}
	for _, call := range view.cdp.Calls() {
		if call.Method == "Target.attachToTarget" && !strings.Contains(string(call.Params), `"T1"`) {
			t.Errorf("attached to the wrong target: %s", call.Params)
		}
	}
	if slices.Contains(methods, "Target.createTarget") {
		t.Errorf("createTarget was sent: %v", methods)
	}

	// Routable by the pool's other verbs.
	if _, err := pool.Snapshot(context.Background(), session.ID, browser.SnapshotRequest{}); errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("snapshot did not find the session: %v", err)
	}
	if err := pool.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if err := pool.Close(context.Background(), session.ID); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("second close = %v, want ErrNoSuchSession", err)
	}
	if slices.Contains(view.cdp.Methods(), "Browser.close") {
		t.Error("the person's browser was closed")
	}
	// The embedded browser is not retired by its last session: a new attach still works.
	view.addPage("T2", "n2")
	if _, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n2")); err != nil {
		t.Errorf("attach after the last session closed: %v", err)
	}
}

func TestAttachEmbeddedRefusesWhatItShould(t *testing.T) {
	view := newWebview(t)
	view.addPage("T1", "n1")
	pool, _ := preparedPool(t)

	t.Run("nonce mismatch", func(t *testing.T) {
		_, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "nope"))
		if !errors.Is(err, browser.ErrNoSuchTarget) {
			t.Errorf("err = %v, want ErrNoSuchTarget", err)
		}
	})
	t.Run("empty nonce", func(t *testing.T) {
		_, err := pool.AttachEmbedded(context.Background(), attachRequest(view, ""))
		if !errors.Is(err, browser.ErrBadAttach) {
			t.Errorf("err = %v, want ErrBadAttach", err)
		}
	})
	t.Run("non-loopback cdp_http", func(t *testing.T) {
		for _, bad := range []string{
			"http://example.com:9222", "http://192.168.1.2:9222", "http://127.0.0.1", "https://127.0.0.1:9222",
			"http://user@127.0.0.1:9222", "http://127.0.0.1:9222/json", "http://127.0.0.1:9222?x=1",
			"http://127.0.0.1.evil.test:9222", "ftp://127.0.0.1:9222", "", "127.0.0.1:9222",
		} {
			request := attachRequest(view, "n1")
			request.CDPHTTP = bad
			if _, err := pool.AttachEmbedded(context.Background(), request); !errors.Is(err, browser.ErrBadCDPAddress) {
				t.Errorf("%q: err = %v, want ErrBadCDPAddress", bad, err)
			}
		}
		if len(view.cdp.Calls()) != 0 {
			t.Errorf("a rejected address still reached the browser: %v", view.cdp.Methods())
		}
	})
	t.Run("placement mismatch", func(t *testing.T) {
		request := attachRequest(view, "n1")
		request.Placement = project("p1", "https://elsewhere.example")
		if _, err := pool.AttachEmbedded(context.Background(), request); !errors.Is(err, browser.ErrPlacementMismatch) {
			t.Errorf("err = %v, want ErrPlacementMismatch", err)
		}
	})
	t.Run("not prepared", func(t *testing.T) {
		request := attachRequest(view, "n1")
		request.Project = "unknown"
		if _, err := pool.AttachEmbedded(context.Background(), request); !errors.Is(err, browser.ErrNotPrepared) {
			t.Errorf("err = %v, want ErrNotPrepared", err)
		}
	})
}

func TestAttachEmbeddedReusesTheConnection(t *testing.T) {
	view := newWebview(t)
	view.addPage("T1", "n1")
	view.addPage("T2", "n2")
	pool, _ := preparedPool(t)

	first, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n1"))
	if err != nil {
		t.Fatal(err)
	}
	second, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n2"))
	if err != nil {
		t.Fatal(err)
	}
	if first.ID == second.ID {
		t.Error("two attaches share a session id")
	}
	// The browser-level fence is armed once per connection; a second connection would arm it again.
	fetch := 0
	for _, call := range view.cdp.Calls() {
		if call.Method == "Fetch.enable" && call.Session == "" {
			fetch++
		}
	}
	if fetch != 1 {
		t.Errorf("browser-level Fetch.enable sent %d times, want 1 (one connection)", fetch)
	}
}

func TestForgetEmbeddedClosesTheProxyTheConnectionAndTheSessions(t *testing.T) {
	view := newWebview(t)
	view.addPage("T1", "n1")
	pool, addr := preparedPool(t)

	session, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n1"))
	if err != nil {
		t.Fatal(err)
	}
	if err := pool.ForgetEmbedded(context.Background(), "p1"); err != nil {
		t.Fatal(err)
	}
	if !stopsListening(addr) {
		t.Error("the proxy still listens after forget")
	}
	if _, err := pool.Snapshot(context.Background(), session.ID, browser.SnapshotRequest{}); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("snapshot after forget = %v, want ErrNoSuchSession", err)
	}
	if _, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n1")); !errors.Is(err, browser.ErrNotPrepared) {
		t.Errorf("attach after forget = %v, want ErrNotPrepared", err)
	}
	if slices.Contains(view.cdp.Methods(), "Target.closeTarget") {
		t.Error("forget closed the person's tab")
	}
	// Forgetting an unknown project is fine, and so is forgetting twice.
	if err := pool.ForgetEmbedded(context.Background(), "p1"); err != nil {
		t.Errorf("second forget = %v", err)
	}
	if err := pool.ForgetEmbedded(context.Background(), "never"); err != nil {
		t.Errorf("forget unknown = %v", err)
	}
}

func TestShutdownClosesEmbeddedProxies(t *testing.T) {
	view := newWebview(t)
	view.addPage("T1", "n1")
	pool, addr := preparedPool(t)
	if _, err := pool.AttachEmbedded(context.Background(), attachRequest(view, "n1")); err != nil {
		t.Fatal(err)
	}
	pool.Shutdown(context.Background())
	if !stopsListening(addr) {
		t.Error("the proxy still listens after shutdown")
	}
}

func TestDetachAndReattachEmbeddedKeepTheSessionAndTheFence(t *testing.T) {
	view := newWebview(t)
	view.addPage("T1", "n1")
	pool, _ := preparedPool(t)
	ctx := context.Background()

	session, err := pool.AttachEmbedded(ctx, attachRequest(view, "n1"))
	if err != nil {
		t.Fatalf("attach: %v", err)
	}

	if err := pool.DetachEmbedded(ctx, session.ID); err != nil {
		t.Fatalf("detach: %v", err)
	}
	methods := view.cdp.Methods()
	if !slices.Contains(methods, "Target.detachFromTarget") {
		t.Errorf("no Target.detachFromTarget: %v", methods)
	}
	if slices.Contains(methods, "Fetch.disable") {
		t.Errorf("the fence was disarmed by a detach: %v", methods)
	}
	if _, err := pool.lookup(session.ID); err != nil {
		t.Errorf("session unregistered by detach: %v", err)
	}

	attachesBefore := 0
	for _, call := range view.cdp.Calls() {
		if call.Method == "Target.attachToTarget" {
			attachesBefore++
		}
	}
	if _, err := pool.ReattachEmbedded(ctx, session.ID); err != nil {
		t.Fatalf("reattach: %v", err)
	}
	var last string
	attaches := 0
	for _, call := range view.cdp.Calls() {
		if call.Method == "Target.attachToTarget" {
			attaches++
			last = string(call.Params)
		}
	}
	if attaches != attachesBefore+1 {
		t.Fatalf("attachToTarget calls = %d, want %d", attaches, attachesBefore+1)
	}
	if !strings.Contains(last, `"T1"`) || !strings.Contains(last, `"flatten":true`) {
		t.Errorf("reattach params = %s", last)
	}
	if _, err := pool.lookup(session.ID); err != nil {
		t.Errorf("session unregistered by reattach: %v", err)
	}
}

func TestDetachAndReattachEmbeddedRefuseAnUnknownSession(t *testing.T) {
	pool, _ := preparedPool(t)
	if err := pool.DetachEmbedded(context.Background(), "s99"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("detach = %v, want ErrNoSuchSession", err)
	}
	if _, err := pool.ReattachEmbedded(context.Background(), "s99"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("reattach = %v, want ErrNoSuchSession", err)
	}
}
