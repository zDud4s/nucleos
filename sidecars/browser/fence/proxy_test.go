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

	proxy := startProxy(t, Policy{Profile: Ephemeral})
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

	proxy := startProxy(t, Policy{Profile: Ephemeral})
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

	proxy := startProxy(t, Policy{Profile: Ephemeral})

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

	proxy := startProxy(t, Policy{Profile: Ephemeral})
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

// connect speaks CONNECT by hand. Go's client will not issue one for an http:// url, and the port
// rule is the only thing this layer buys from CONNECT at all.
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

// TestATunnelToAnyPortButHttpsIsRefused. What CONNECT gives this layer is the port, and nothing
// else: a tunnel to any other port carries whatever the page wants in both directions, invisibly to
// every other part of the fence.
func TestATunnelToAnyPortButHttpsIsRefused(t *testing.T) {
	proxy := startProxy(t, Policy{Profile: Ephemeral})

	for _, target := range []string{"example.org:8443", "example.org:22", "example.org:80", "example.org:1337"} {
		response := connect(t, proxy, target)
		if response.StatusCode != http.StatusForbidden {
			t.Errorf("CONNECT %s answered %d, want 403", target, response.StatusCode)
		}
		_ = response.Body.Close()
	}
}

// TestATunnelTo443GetsPastThePolicy is that test's control, and it distinguishes the two failures
// that look alike: 403 is the fence refusing, 502 is the fence allowing and the network saying no.
// Without it, a proxy that refused every CONNECT would pass the test above.
func TestATunnelTo443GetsPastThePolicy(t *testing.T) {
	// A listener that is closed immediately, so the port is dead but the address is well-formed and
	// nothing leaves this machine.
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	dead := listener.Addr().String()
	_ = listener.Close()

	proxy := startProxy(t, Policy{Profile: Ephemeral})

	// Port 443 on a host that will not answer. The policy is what is under test, not the dial.
	refused := connect(t, proxy, "127.0.0.1:443")
	_ = refused.Body.Close()
	if refused.StatusCode == http.StatusForbidden {
		t.Fatal("CONNECT to 443 was refused by policy; the port rule refuses everything")
	}

	// And the same address on a non-443 port is refused by us, not by the network.
	byPolicy := connect(t, proxy, dead)
	_ = byPolicy.Body.Close()
	if byPolicy.StatusCode != http.StatusForbidden {
		t.Fatalf("CONNECT %s answered %d, want a policy refusal", dead, byPolicy.StatusCode)
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

	proxy := startProxy(t, Policy{Profile: Ephemeral})
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

	proxy := startProxy(t, Policy{Profile: Ephemeral})
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

	proxy := startProxy(t, Policy{Profile: Ephemeral})
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

	proxy := startProxy(t, Policy{Profile: Ephemeral})
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
