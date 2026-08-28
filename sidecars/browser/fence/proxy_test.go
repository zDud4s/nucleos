// §spec pilar-de-browser

package fence

import (
	"bufio"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// throughProxy builds a client that goes through the fence, exactly as Chrome does with
// --proxy-server. It is a real client through a real proxy: the WebSocket handshake this file exists
// to test is a wire-level thing, and a mocked transport would not have one.
func throughProxy(t *testing.T, proxy *Proxy) *http.Client {
	t.Helper()
	address, err := url.Parse("http://" + proxy.Addr())
	if err != nil {
		t.Fatalf("proxy url: %v", err)
	}
	return &http.Client{
		Timeout:   5 * time.Second,
		Transport: &http.Transport{Proxy: http.ProxyURL(address)},
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
}

// admitting builds a policy that lets the fence reach one loopback test server and nothing else.
// Every httptest server is on 127.0.0.1, so without this the loopback rule refuses the lot — which
// is the rule working, and would make every test in this file pass for the wrong reason.
func admitting(server *httptest.Server) Policy {
	return Policy{Profile: Ephemeral, Loopback: []string{server.URL}}
}

func startProxy(t *testing.T, policy Policy) *Proxy {
	t.Helper()
	proxy, err := NewProxy(policy)
	if err != nil {
		t.Fatalf("proxy: %v", err)
	}
	t.Cleanup(func() { _ = proxy.Close() })
	return proxy
}

func TestAProxyWithoutAPolicyRefusesToListen(t *testing.T) {
	if _, err := NewProxy(Policy{}); err == nil {
		t.Fatal("a proxy started with no policy to enforce")
	}
}

// TestTheProxyForwardsAGet is the control every other test in this file needs. Without it, a proxy
// that refused everything — including because it was broken — would pass the whole file.
func TestTheProxyForwardsAGet(t *testing.T) {
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		_, _ = io.WriteString(w, "hello")
	}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))
	response, err := throughProxy(t, proxy).Get(origin.URL)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status %d", response.StatusCode)
	}
	body, _ := io.ReadAll(response.Body)
	if string(body) != "hello" {
		t.Fatalf("body %q", body)
	}
	if len(proxy.Refusals()) != 0 {
		t.Fatalf("an allowed GET was recorded as a refusal: %+v", proxy.Refusals())
	}
}

func TestTheProxyRefusesAPost(t *testing.T) {
	origin := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Error("the origin was reached; the POST left the machine")
	}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))
	response, err := throughProxy(t, proxy).Post(origin.URL, "text/plain", strings.NewReader("x"))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusForbidden {
		t.Fatalf("status %d, want 403", response.StatusCode)
	}
	if got := response.Header.Get("X-NucleOS-Fence"); got != string(browser.ConsequenceMethod) {
		t.Errorf("refusal header is %q", got)
	}
}

// TestTheProxyCarriesAPostToAnOriginWithAWriteGrant.
//
// The exception the write rule opens at this layer, and the reason it exists at all.
//
// It was found by the gate rather than reasoned out. The CDP fence allowed a form submission on a
// granted origin, the driver wrote it down as having left, and this layer answered 403 to the same
// request a moment later — so the agent got a page saying "non-get-method" from a fence that had
// already decided otherwise, with the record of the submission already written by the half that said
// yes. Two layers of one rule disagreeing, and the layer with less information winning.
//
// What this layer still refuses is the case above it: a POST to an origin nobody granted. It cannot
// tell a form from a script and it does not pretend to — the other four conditions belong to the CDP
// fence, which is the same division this layer already makes for the site allowlist.
func TestTheProxyCarriesAPostToAnOriginWithAWriteGrant(t *testing.T) {
	reached := make(chan struct{}, 1)
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodPost {
			reached <- struct{}{}
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer origin.Close()

	// A PROJECT profile, because that is the only kind a write grant exists in: a throwaway has no
	// login in it, so there is nobody for a form to be submitted as, and Validate refuses the pairing
	// outright. `admitting` builds a throwaway, which is right for every other test in this file.
	policy := Policy{
		Profile:  Project,
		Origins:  []string{"https://nucleos.invalid"},
		Loopback: []string{origin.URL},
		Writable: []string{origin.URL},
	}
	proxy := startProxy(t, policy)

	response, err := throughProxy(t, proxy).Post(origin.URL, "text/plain", strings.NewReader("x"))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status %d, want the POST carried", response.StatusCode)
	}
	select {
	case <-reached:
	case <-time.After(2 * time.Second):
		t.Fatal("the proxy answered 200 and the POST never reached the origin")
	}
}

// TestTheProxyRefusesAWebSocketHandshake is spec §11 test 3, at the layer that can actually see it.
//
// The spike measured Fetch never receiving a ws: url and Network.setBlockedURLs completing the
// handshake anyway with ["*"] set. This is the mechanism that is left, so this test is the only
// evidence that the channel is closed at all.
func TestTheProxyRefusesAWebSocketHandshake(t *testing.T) {
	reached := make(chan struct{}, 1)
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		reached <- struct{}{}
		w.WriteHeader(http.StatusSwitchingProtocols)
	}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))

	request, err := http.NewRequest(http.MethodGet, origin.URL+"/live", nil)
	if err != nil {
		t.Fatalf("request: %v", err)
	}
	request.Header.Set("Connection", "Upgrade")
	request.Header.Set("Upgrade", "websocket")
	request.Header.Set("Sec-WebSocket-Version", "13")
	request.Header.Set("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")

	response, err := throughProxy(t, proxy).Do(request)
	if err != nil {
		t.Fatalf("handshake: %v", err)
	}
	defer response.Body.Close()

	if response.StatusCode != http.StatusForbidden {
		t.Fatalf("status %d, want 403", response.StatusCode)
	}
	select {
	case <-reached:
		t.Fatal("the handshake reached the origin")
	default:
	}

	refusals := proxy.Refusals()
	if len(refusals) != 1 || refusals[0].Consequence != browser.ConsequenceChannel {
		t.Fatalf("recorded %+v", refusals)
	}
}

// TestTheSameRequestWithoutTheUpgradePasses is the control §11 demands beside the test above. Three
// spike results came back inverted because the control was missing, and "the handshake did not
// arrive" is exactly the shape of claim that a broken proxy also produces.
func TestTheSameRequestWithoutTheUpgradePasses(t *testing.T) {
	reached := make(chan struct{}, 1)
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		reached <- struct{}{}
		w.WriteHeader(http.StatusOK)
	}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))
	response, err := throughProxy(t, proxy).Get(origin.URL + "/live")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status %d", response.StatusCode)
	}
	select {
	case <-reached:
	default:
		t.Fatal("the control never reached the origin either; the proxy is broken, not fencing")
	}
}

// connect speaks CONNECT by hand, because Go's client will not issue one for an http:// url.
func connect(t *testing.T, proxy *Proxy, target string) *http.Response {
	t.Helper()
	conn, err := net.DialTimeout("tcp", proxy.Addr(), 5*time.Second)
	if err != nil {
		t.Fatalf("dial proxy: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))

	if _, err := io.WriteString(conn, "CONNECT "+target+" HTTP/1.1\r\nHost: "+target+"\r\n\r\n"); err != nil {
		t.Fatalf("write CONNECT: %v", err)
	}
	response, err := http.ReadResponse(bufio.NewReader(conn), &http.Request{Method: http.MethodConnect})
	if err != nil {
		t.Fatalf("read CONNECT reply: %v", err)
	}
	return response
}

// deadLoopbackPort returns an address on this machine that is well-formed and will not answer, so a
// dial attempt fails locally and nothing leaves the machine.
func deadLoopbackPort(t *testing.T) string {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	address := listener.Addr().String()
	_ = listener.Close()
	return address
}

// TestATunnelToLoopbackIsRefused is the boundary that survived.
//
// A rule refusing every CONNECT to a port other than 443 was here first, and was removed rather than
// kept for comfort: a page that wants to reach a host of its choosing does it on 443, so the rule
// stopped nothing, while breaking the case browser_policy.rs supports on purpose — an origin with
// its own port. This is what is left, and it is real: 9222 is the browser's own debugging port, and
// whoever reaches it owns every profile on the machine.
func TestATunnelToLoopbackIsRefused(t *testing.T) {
	proxy := startProxy(t, Policy{Profile: Ephemeral})

	for _, target := range []string{"127.0.0.1:9222", "localhost:8795", "[::1]:443", "127.0.0.1:443"} {
		response := connect(t, proxy, target)
		if response.StatusCode != http.StatusForbidden {
			t.Errorf("CONNECT %s answered %d, want 403", target, response.StatusCode)
		}
		_ = response.Body.Close()
	}

	refusals := proxy.Refusals()
	if len(refusals) == 0 || refusals[0].Consequence != browser.ConsequenceLoopback {
		t.Fatalf("recorded %+v", refusals)
	}
}

// TestAnAdmittedLoopbackTunnelGetsPastThePolicy is the control, and it separates the two failures
// that look identical from outside: 403 is the fence refusing, 502 is the fence allowing and the
// dial failing. Without it, a proxy that refused every CONNECT would pass the test above.
func TestAnAdmittedLoopbackTunnelGetsPastThePolicy(t *testing.T) {
	dead := deadLoopbackPort(t)
	proxy := startProxy(t, Policy{Profile: Ephemeral, Loopback: []string{"https://" + dead}})

	allowed := connect(t, proxy, dead)
	_ = allowed.Body.Close()
	if allowed.StatusCode == http.StatusForbidden {
		t.Fatalf("an admitted loopback origin was refused; the rule refuses everything")
	}

	// And a neighbouring port on the same host is not admitted by that entry.
	other := deadLoopbackPort(t)
	refused := connect(t, proxy, other)
	_ = refused.Body.Close()
	if refused.StatusCode != http.StatusForbidden {
		t.Fatalf("CONNECT %s answered %d; the entry admitted a port it does not name", other, refused.StatusCode)
	}
}

// TestAPlainHtmlDocumentGetsTheFenceCSP. Only plain http reaches this layer as a document — an https
// one arrives inside a CONNECT tunnel the proxy cannot read, and gets its CSP from the CDP response
// stage instead. The local-server tests of §11 run over plain http, so this is the path they take.
func TestAPlainHtmlDocumentGetsTheFenceCSP(t *testing.T) {
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = io.WriteString(w, "<html></html>")
	}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))
	response, err := throughProxy(t, proxy).Get(origin.URL)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer response.Body.Close()

	policies := response.Header.Values(HeaderName)
	if len(policies) == 0 {
		t.Fatal("an html document came back with no fence on it")
	}
	var found bool
	for _, policy := range policies {
		if policy == Directives {
			found = true
		}
	}
	if !found {
		t.Fatalf("the CSP is not the fence's: %v", policies)
	}
}

func TestANonDocumentResponseIsNotGivenACSP(t *testing.T) {
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"ok":true}`)
	}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))
	response, err := throughProxy(t, proxy).Get(origin.URL)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer response.Body.Close()
	if got := response.Header.Values(HeaderName); len(got) != 0 {
		t.Errorf("a json response was given a document's policy: %v", got)
	}
}

// TestThePagesOwnCSPSurvivesTheProxy. Two CSP headers intersect; replacing the page's would be the
// fence quietly relaxing a site that was stricter than we are.
func TestThePagesOwnCSPSurvivesTheProxy(t *testing.T) {
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		w.Header().Set(HeaderName, "default-src 'self'")
		_, _ = io.WriteString(w, "<html></html>")
	}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))
	response, err := throughProxy(t, proxy).Get(origin.URL)
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer response.Body.Close()

	policies := response.Header.Values(HeaderName)
	if len(policies) != 2 {
		t.Fatalf("expected the page's policy and ours, got %v", policies)
	}
}

// TestTheRefusalRecordIsBounded. A page in a loop generates refusals faster than anything reads
// them, and a slice that only grows turns a blocked page into a memory leak.
func TestTheRefusalRecordIsBounded(t *testing.T) {
	proxy := startProxy(t, Policy{Profile: Ephemeral})
	for i := 0; i < proxyRefusalCap+50; i++ {
		proxy.record(ProxyRefusal{Target: "x", Consequence: browser.ConsequenceChannel})
	}
	if got := len(proxy.Refusals()); got != proxyRefusalCap {
		t.Fatalf("kept %d refusals, cap is %d", got, proxyRefusalCap)
	}
}

func TestARefusalReachesTheObserver(t *testing.T) {
	origin := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	defer origin.Close()

	proxy := startProxy(t, admitting(origin))
	seen := make(chan ProxyRefusal, 1)
	proxy.OnRefusal(func(refusal ProxyRefusal) { seen <- refusal })

	response, err := throughProxy(t, proxy).Post(origin.URL, "text/plain", strings.NewReader("x"))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	_ = response.Body.Close()

	select {
	case refusal := <-seen:
		if refusal.Consequence != browser.ConsequenceMethod {
			t.Errorf("observed %+v", refusal)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the refusal never reached the observer, so no act could report it")
	}
}
