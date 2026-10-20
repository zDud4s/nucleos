// §spec pilar-de-browser

package fence

import (
	"net"
	"net/http"
	"strings"
)

// ambientSuffixes is WebView2 housekeeping, not browsing: telemetry, config, update and SmartScreen
// endpoints the embedded runtime talks to on its own, whatever the agent is doing. None of it is a
// page an agent asked for, so none of it may be attributed to an agent session.
var ambientSuffixes = []string{
	"edge.skype.com",
	"msedge.net",
	"events.data.microsoft.com",
	"smartscreen.microsoft.com",
	"edge.microsoft.com",
	"edgeservices.bing.com",
	"windowsupdate.com",
	"update.microsoft.com",
}

// NewEmbeddedProxy is NewProxy for the proxy of an embedded WebView2 environment. WebView2 sends its
// own ambient traffic through the proxy; this variant refuses it silently (403, no audit, no
// ProxyRefusal, no observer call) so it is never attributed to an agent. Everything else behaves as
// under NewProxy.
func NewEmbeddedProxy(policy Policy) (*Proxy, error) { return newProxy(policy, true) }

// IsAmbientHost reports whether host (optionally with a port and a trailing dot, any case) is one of
// WebView2's own housekeeping hosts or a subdomain of one.
func IsAmbientHost(host string) bool {
	host = strings.ToLower(strings.TrimSpace(host))
	if h, _, err := net.SplitHostPort(host); err == nil {
		host = h
	}
	host = strings.TrimSuffix(host, ".")
	for _, suffix := range ambientSuffixes {
		if host == suffix || strings.HasSuffix(host, "."+suffix) {
			return true
		}
	}
	return false
}

// refuseAmbient answers 403 and reports true when this is an embedded proxy and host is ambient. It
// deliberately never calls record: the refusal belongs to no agent session.
func (p *Proxy) refuseAmbient(w http.ResponseWriter, host string) bool {
	if !p.embedded || !IsAmbientHost(host) {
		return false
	}
	w.Header().Set("X-NucleOS-Fence", "ambient")
	http.Error(w, "refused: embedded runtime housekeeping", http.StatusForbidden)
	return true
}
