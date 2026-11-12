// §spec pilar-de-browser

package fence

import (
	"bufio"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

// connectStatus sends a raw CONNECT to the proxy and returns the status line and headers.
func connectStatus(t *testing.T, proxy *Proxy, target string) *http.Response {
	t.Helper()
	conn, err := net.DialTimeout("tcp", proxy.Addr(), 5*time.Second)
	if err != nil {
		t.Fatalf("dial proxy: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))
	_, _ = fmt.Fprintf(conn, "CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n", target, target)
	response, err := http.ReadResponse(bufio.NewReader(conn), &http.Request{Method: http.MethodConnect})
	if err != nil {
		t.Fatalf("read response: %v", err)
	}
	return response
}

func startEmbedded(t *testing.T, policy Policy) (*Proxy, *atomic.Int32) {
	t.Helper()
	proxy, err := NewEmbeddedProxy(policy)
	if err != nil {
		t.Fatalf("embedded proxy: %v", err)
	}
	t.Cleanup(func() { _ = proxy.Close() })
	var fired atomic.Int32
	proxy.OnRefusal(func(ProxyRefusal) { fired.Add(1) })
	return proxy, &fired
}

func TestTheEmbeddedProxySilentlyRefusesAmbientConnect(t *testing.T) {
	proxy, fired := startEmbedded(t, Policy{Profile: Ephemeral})
	for _, target := range []string{"config.edge.skype.com:443", "CONFIG.Edge.Skype.com.:443", "a.b.msedge.net:443"} {
		response := connectStatus(t, proxy, target)
		if response.StatusCode != http.StatusForbidden {
			t.Errorf("%s: status %d, want 403", target, response.StatusCode)
		}
		if got := response.Header.Get("X-NucleOS-Fence"); got != "ambient" {
			t.Errorf("%s: header %q", target, got)
		}
	}
	if got := proxy.Refusals(); len(got) != 0 {
		t.Fatalf("ambient traffic was recorded: %+v", got)
	}
	if fired.Load() != 0 {
		t.Fatal("the refusal observer fired for ambient traffic")
	}
}

func TestTheEmbeddedProxyStillTunnelsAnAdmittedHost(t *testing.T) {
	origin := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	defer origin.Close()
	address := strings.TrimPrefix(origin.URL, "http://")
	proxy, fired := startEmbedded(t, Policy{Profile: Ephemeral, Loopback: []string{"https://" + address}})
	response := connectStatus(t, proxy, address)
	if response.StatusCode != http.StatusOK {
		t.Fatalf("status %d, want 200", response.StatusCode)
	}
	if fired.Load() != 0 || len(proxy.Refusals()) != 0 {
		t.Fatal("an admitted tunnel was recorded as a refusal")
	}
}

func TestAPlainProxyDoesNotTreatAmbientHostsSpecially(t *testing.T) {
	proxy := startProxy(t, Policy{Profile: Ephemeral})
	response := connectStatus(t, proxy, "127.0.0.1:1")
	if got := response.Header.Get("X-NucleOS-Fence"); got == "ambient" {
		t.Fatal("control: a non-ambient host got the ambient answer")
	}
	// Ambient host on a plain proxy: whatever the answer, it is not the ambient rule's.
	response = connectStatus(t, proxy, "config.edge.skype.com:443")
	if got := response.Header.Get("X-NucleOS-Fence"); got == "ambient" {
		t.Fatal("the plain proxy applied the ambient rule")
	}
}

func TestIsAmbientHost(t *testing.T) {
	for host, want := range map[string]bool{
		"config.edge.skype.com":         true,
		"edge.skype.com":                true,
		"Edge.Skype.COM:443":            true,
		"edge.skype.com.":               true,
		"x.y.events.data.microsoft.com": true,
		"msedge.net":                    true,
		"windowsupdate.com":             true,
		"download.windowsupdate.com":    true,
		"notmsedge.net":                 false,
		"edge.skype.com.evil.test":      false,
		"example.com":                   false,
		"":                              false,
	} {
		if got := IsAmbientHost(host); got != want {
			t.Errorf("IsAmbientHost(%q) = %v, want %v", host, got, want)
		}
	}
}

func TestValidateEmbeddedAcceptsAnEmptyProjectListThatAdmitsNothing(t *testing.T) {
	empty := Policy{Profile: Project}
	if err := empty.Validate(); err == nil {
		t.Fatal("Validate accepted a project with no origins")
	}
	if err := empty.ValidateEmbedded(); err != nil {
		t.Fatalf("ValidateEmbedded: %v", err)
	}
	verdict := Decide(empty, Request{Method: "GET", URL: "https://example.org/", ResourceType: "Document"})
	if verdict.Allow {
		t.Fatal("an empty list admitted a document")
	}
	if err := (Policy{}).ValidateEmbedded(); err == nil {
		t.Fatal("ValidateEmbedded accepted a policy with no profile kind")
	}
	if err := (Policy{Profile: Project, Origins: []string{"not a host"}}).ValidateEmbedded(); err == nil {
		t.Fatal("ValidateEmbedded accepted a malformed origin")
	}
}

func TestSetEmbeddedPolicyIsOnlyForAnEmbeddedProxy(t *testing.T) {
	plain, err := NewProxy(Policy{Profile: Project, Origins: []string{"https://example.org"}})
	if err != nil {
		t.Fatal(err)
	}
	defer plain.Close()
	if err := plain.SetEmbeddedPolicy(Policy{Profile: Project}); err == nil {
		t.Error("a plain proxy accepted a policy swap")
	}
	embedded, err := NewEmbeddedProxy(Policy{Profile: Project})
	if err != nil {
		t.Fatal(err)
	}
	defer embedded.Close()
	if err := embedded.SetEmbeddedPolicy(Policy{Profile: Project, Origins: []string{"https://example.org"}}); err != nil {
		t.Errorf("swap: %v", err)
	}
	if err := embedded.SetEmbeddedPolicy(Policy{}); err == nil {
		t.Error("swap accepted a policy with no profile kind")
	}
}
