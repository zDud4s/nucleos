package fence

import (
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"sync"
	"time"

	"nucleosbrowser/browser"
)

// Proxy is the fence's third layer: a loopback HTTP proxy every agent-mode request goes through.
//
// It exists because CDP is blind to one channel. The spike measured `Fetch.requestPaused` never
// arriving for a ws: url, and measured Network.setBlockedURLs — the mechanism the first version of
// the spec named — completing the handshake anyway with ["*"] set. A proxy sees that handshake as an
// ordinary GET carrying `Upgrade: websocket`, and refuses it.
//
// # What it does not close, said plainly
//
// `wss://` reaches it as `CONNECT host:443`, byte-identical to the CONNECT for any https sub-resource,
// and everything inside that tunnel is TLS to a host the page chose. There is nothing here to
// distinguish them, so this layer does not try; the injected `connect-src 'none'` closes it instead
// (see [Directives]). That makes the proxy a thin layer, and it is described as one — the pillar's
// security does not rest on it.
//
// # The one thing that IS a boundary here
//
// Loopback. Agent mode is launched with --proxy-bypass-list=<-loopback> so that this process sees
// requests to 127.0.0.1 at all, and the consequence is that a page can address the núcleo's API, the
// other sidecars, and the browser's own debugging port. [DecideTunnel] refuses them unless the
// profile's Loopback list names them. The CDP layer refuses the same thing for plain HTTP, so a
// change to one of the two is not a hole in the other.
//
// # Why it is not an open relay worth worrying about
//
// It listens on loopback and forwards GET, HEAD, and a POST to an origin the profile has a write
// grant for — and refuses loopback destinations it was not told about. Any local process that could
// reach it could already make the same request directly, so it hands out no reach that was not
// already there.
//
// The POST is the one thing here that a person's decision opens, and this layer answers only the
// part of the write rule it is able to: whether the origin was granted. It cannot see whether a
// request is a document, and it has no idea what an act is doing, so the other four conditions are
// the CDP fence's — which is the same division this layer already makes for the site allowlist, for
// the same reason. See decideWrite.
type Proxy struct {
	policy    Policy
	listener  net.Listener
	server    *http.Server
	transport *http.Transport

	mu       sync.Mutex
	refusals []ProxyRefusal
	observer func(ProxyRefusal)
}

// ProxyRefusal is one thing the proxy stopped. Target is the host or url, never a body.
type ProxyRefusal struct {
	Target      string
	Consequence browser.Consequence
	Detail      string
}

// proxyRefusalCap bounds the record. A page in a loop can generate refusals faster than anything
// reads them, and a slice that only grows turns a blocked page into a memory leak.
const proxyRefusalCap = 256

// NewProxy validates the policy, listens on loopback and starts serving.
//
// It refuses to exist without a valid policy for the same reason chrome.Connect refuses to return a
// Driver without an attached fence: a proxy running with a default policy is indistinguishable, from
// every call site, from one running with the right one.
func NewProxy(policy Policy) (*Proxy, error) {
	if err := policy.Validate(); err != nil {
		return nil, err
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, fmt.Errorf("fence: listening on loopback: %w", err)
	}
	proxy := &Proxy{
		policy:   policy,
		listener: listener,
		transport: &http.Transport{
			// Explicitly nil: a transport that honoured the environment's proxy settings would send
			// the fence's own traffic back through whatever HTTP_PROXY happens to say, which is both
			// a loop and a way out of the fence that nobody wrote down.
			Proxy:               nil,
			DialContext:         (&net.Dialer{Timeout: 10 * time.Second}).DialContext,
			TLSHandshakeTimeout: 10 * time.Second,
		},
	}
	proxy.server = &http.Server{
		Handler:           http.HandlerFunc(proxy.handle),
		ReadHeaderTimeout: 15 * time.Second,
	}
	go func() { _ = proxy.server.Serve(listener) }()
	return proxy, nil
}

// Addr is the "127.0.0.1:port" to put on Chrome's command line.
func (p *Proxy) Addr() string { return p.listener.Addr().String() }

// Close stops the proxy.
func (p *Proxy) Close() error { return p.server.Close() }

// OnRefusal registers a callback, so the driver can report a refusal against the act that caused it.
func (p *Proxy) OnRefusal(observer func(ProxyRefusal)) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.observer = observer
}

// Refusals returns what the proxy has stopped, oldest first.
func (p *Proxy) Refusals() []ProxyRefusal {
	p.mu.Lock()
	defer p.mu.Unlock()
	return append([]ProxyRefusal(nil), p.refusals...)
}

func (p *Proxy) record(refusal ProxyRefusal) {
	p.mu.Lock()
	p.refusals = append(p.refusals, refusal)
	if len(p.refusals) > proxyRefusalCap {
		p.refusals = p.refusals[len(p.refusals)-proxyRefusalCap:]
	}
	observer := p.observer
	p.mu.Unlock()
	if observer != nil {
		observer(refusal)
	}
}

func (p *Proxy) handle(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodConnect {
		p.tunnel(w, r)
		return
	}

	// ResourceType is left empty on purpose, and it is READ as a signal rather than merely absent.
	// This layer sees bytes, not Chrome's classification, so it cannot tell a document from a
	// sub-resource — and two of Decide's rules turn on exactly that. The allowlist applies only to
	// documents (spec §5.4, §5.5), and the write rule's other four conditions need to know both which
	// requests are documents and which act is in flight. Both are left to the layer that can see, and
	// Decide branches on the empty string to say so. See decideWrite for what this layer still
	// answers, and for the gate run that found the two layers disagreeing.
	verdict := Decide(p.policy, Request{
		Method:  r.Method,
		URL:     r.URL.String(),
		Headers: flattenHeaders(r.Header),
	})
	if !verdict.Allow {
		p.refuse(w, r.URL.String(), verdict)
		return
	}

	p.forward(w, r)
}

// tunnel answers CONNECT. See [DecideTunnel] for why the rule is as thin as it is.
func (p *Proxy) tunnel(w http.ResponseWriter, r *http.Request) {
	if verdict := DecideTunnel(p.policy, r.Host); !verdict.Allow {
		p.refuse(w, r.Host, verdict)
		return
	}

	upstream, err := net.DialTimeout("tcp", r.Host, 10*time.Second)
	if err != nil {
		http.Error(w, "upstream unreachable", http.StatusBadGateway)
		return
	}
	defer upstream.Close()

	hijacker, ok := w.(http.Hijacker)
	if !ok {
		http.Error(w, "cannot tunnel", http.StatusInternalServerError)
		return
	}
	client, _, err := hijacker.Hijack()
	if err != nil {
		return
	}
	defer client.Close()

	if _, err := io.WriteString(client, "HTTP/1.1 200 Connection Established\r\n\r\n"); err != nil {
		return
	}

	done := make(chan struct{}, 2)
	go func() { _, _ = io.Copy(upstream, client); done <- struct{}{} }()
	go func() { _, _ = io.Copy(client, upstream); done <- struct{}{} }()
	<-done
}

// forward relays an allowed plain-HTTP request.
func (p *Proxy) forward(w http.ResponseWriter, r *http.Request) {
	outbound := r.Clone(r.Context())
	// A proxy receives absolute-form; a client must send origin-form. Leaving RequestURI set makes
	// the transport refuse the request outright.
	outbound.RequestURI = ""
	stripHopByHop(outbound.Header)

	response, err := p.transport.RoundTrip(outbound)
	if err != nil {
		http.Error(w, "upstream unreachable", http.StatusBadGateway)
		return
	}
	defer response.Body.Close()

	stripHopByHop(response.Header)
	for name, values := range response.Header {
		for _, value := range values {
			w.Header().Add(name, value)
		}
	}
	// A second CSP, for the same reason there are two download mechanisms: they fail differently.
	// This one only ever reaches a plain-http document, because an https document arrives inside a
	// CONNECT tunnel this layer cannot read — those get theirs from the CDP response stage. Two CSP
	// headers intersect, so a document that passes both paths is fenced once, not twice.
	if isHTML(response.Header.Get("Content-Type")) {
		w.Header().Add(HeaderName, Directives)
	}
	w.WriteHeader(response.StatusCode)
	_, _ = io.Copy(w, response.Body)
}

func (p *Proxy) refuse(w http.ResponseWriter, target string, verdict Verdict) {
	p.record(ProxyRefusal{
		Target:      target,
		Consequence: verdict.Consequence,
		Detail:      verdict.Detail,
	})
	// 403 rather than closing the connection: a closed connection is what a broken proxy looks like,
	// and the difference between "refused" and "broken" is the difference between the agent learning
	// something and the agent retrying.
	w.Header().Set("X-NucleOS-Fence", string(verdict.Consequence))
	http.Error(w, "refused by the NucleOS browser fence: "+string(verdict.Consequence), http.StatusForbidden)
}

// hopByHop are the headers that belong to one connection and must not be relayed. Upgrade is in the
// list because by the time forward runs, an upgrade request has already been refused — so if one is
// still here it is ours to drop, not the origin's to see.
var hopByHop = []string{
	"Connection", "Proxy-Connection", "Keep-Alive", "Proxy-Authenticate",
	"Proxy-Authorization", "Te", "Trailer", "Transfer-Encoding", "Upgrade",
}

func stripHopByHop(headers http.Header) {
	for _, name := range hopByHop {
		headers.Del(name)
	}
}

func flattenHeaders(headers http.Header) map[string]string {
	out := make(map[string]string, len(headers))
	for name, values := range headers {
		if len(values) > 0 {
			out[name] = values[0]
		}
	}
	return out
}

func isHTML(contentType string) bool {
	return strings.HasPrefix(strings.ToLower(strings.TrimSpace(contentType)), "text/html")
}
