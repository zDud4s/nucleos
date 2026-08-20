// Package fence is spec §6.2 as code: in agent mode the browser emits GET and HEAD over HTTP(S),
// and nothing else leaves the machine.
//
// # Three layers, and what each one really closes
//
// The spec's first version named one mechanism and assumed it covered everything. The spike of
// 2026-08-15 measured that it did not, so the fence is three layers with different blind spots, and
// this table is the only honest way to describe it. It is written here rather than in a design doc
// because the blind spots are what a reader needs before they touch any of it.
//
//	Layer                      Closes                                  Blind to
//	-------------------------  --------------------------------------  ---------------------------
//	CDP Fetch, browser session  method, scheme, off-list documents,     WebSocket, entirely: a
//	  (fence.go in chrome/)     downloads, service-worker scripts       requestPaused with a ws:
//	                                                                    url never arrives
//	Injected CSP                fetch/XHR/EventSource/beacon, ws: AND   nothing it is asked to do,
//	  (csp.go)                  wss:, form submission; inherited by     but only where a document
//	                            blob: documents, which is the only      response passes through us
//	                            thing that contains them
//	Loopback proxy              ws:// (a GET with Upgrade); loopback,   everything inside a CONNECT
//	  (proxy.go)                which the CDP layer also refuses        tunnel, which is TLS to a
//	                                                                    host the page chose
//
// WebRTC is closed by none of the three — not the CDP fence, not the CSP, not the proxy. It is
// closed OUTSIDE this file, by a preference written into the profile before the browser starts
// (spec §6.2b; see launch.applyWebRTCPolicy), which is worth knowing here: nothing you add to this
// table will govern it.
//
// # How narrow the proxy actually is, since it is easy to overrate
//
// Almost all real traffic reaches it as CONNECT and passes through opaque. Its unique contribution is
// plaintext `ws://`, which the CDP layer cannot see at all — and real sites use `wss://`, which it
// cannot see either. So it is a thin layer that closes one measured hole and backstops the CSP on
// plain-http documents, and the security of this pillar does not rest on it. Saying otherwise would
// put weight on the layer least able to carry it.
//
// # Why wss: is layer 2's job and not layer 3's
//
// With a proxy configured, Chrome sends `ws://` through it as an ordinary absolute-form GET carrying
// `Upgrade: websocket`, which the proxy reads and refuses. `wss://` it sends as `CONNECT host:443`,
// byte-identical to the CONNECT for any https sub-resource on the same host. Refusing it would mean
// refusing https. So the proxy does not pretend to close it, and `connect-src 'none'` does — which
// is also what the spike measured containing a blob: document, because a blob: inherits its parent's
// policy.
package fence

import (
	"errors"
	"fmt"
	"net"
	"net/url"
	"strconv"
	"strings"

	"nucleosbrowser/browser"
)

// ProfileKind is what the session's profile carries, and therefore what a wrong answer costs.
//
// It is not a bool, and it has no zero value that means anything: [Policy.Validate] rejects an unset
// one. A fence configured by accident would be a fence with a default, and the default that would
// get written is the permissive one.
type ProfileKind string

const (
	// Ephemeral is a profile with no identity in it. There are no cookies to spend, so a document
	// from any host costs nothing beyond rendering it — spec §5.3 routes exactly the off-list
	// requests here.
	Ephemeral ProfileKind = "ephemeral"
	// Project is a profile with live sessions. A document from an off-list host executes next to
	// those cookies, so it is refused before it runs (spec §5.4).
	Project ProfileKind = "project"
)

// Policy is the fence's configuration for one browser.
//
// It arrives from the núcleo at launch, per profile, and never from the agent — the same boundary
// [browser.OpenRequest] holds by having no profile field. A per-request origin list would let the
// caller widen the fence for the request it is about to make.
type Policy struct {
	Profile ProfileKind
	// Origins are whole origins, "https://host:port", matched exactly. The shape and the matching
	// rule mirror `core/src/browser_policy.rs` deliberately: two allowlists that disagree about what
	// "the same site" means is a hole neither of them can see.
	Origins []string
	// Loopback is what this profile may reach ON THIS MACHINE, as "scheme://host:port". Empty means
	// nothing, which is the normal case.
	//
	// A separate field rather than entries in Origins, and the separation is the point. Origins is
	// about which sites the profile's cookies may meet; this is about which of OUR OWN services a
	// page may address. They fail in opposite directions — a too-wide Origins spends the person's
	// session on a stranger, a too-wide Loopback hands a stranger the núcleo — and one list would
	// mean an entry added for the first reason silently doing the second.
	//
	// It also accepts http://, which Origins never does: a local dev server on plain http is the
	// ordinary case here, and there is no session on it to lose.
	Loopback []string
}

// ErrNoProfileKind is [Policy.Validate]'s refusal to fence a profile nobody named. Spec §6.2a's rule
// — a fence that is not there does not get to look like one that is — applied to configuration
// rather than to attachment.
var ErrNoProfileKind = errors.New("fence: policy names no profile kind, refusing to build a fence with a default")

// Validate reports whether a policy is usable. A driver refuses to exist without one.
func (p Policy) Validate() error {
	for _, entry := range p.Loopback {
		if normaliseLoopback(entry) == "" {
			return fmt.Errorf("fence: %q is not a loopback origin", entry)
		}
	}
	switch p.Profile {
	case Ephemeral:
		if len(p.Origins) > 0 {
			// Silently ignoring a list somebody wrote is how a security control becomes decorative.
			return errors.New("fence: an ephemeral profile has no origin list; this one would be ignored")
		}
		return nil
	case Project:
		if len(p.Origins) == 0 {
			// Not a fail-closed default worth having: it would admit no document at all, so every
			// session on it would look like a broken browser rather than a refused one.
			return errors.New("fence: a project profile with an empty origin list would admit no document at all")
		}
		for _, entry := range p.Origins {
			if NormaliseEntry(entry) == "" {
				return fmt.Errorf("fence: %q is not a host or an https origin", entry)
			}
		}
		return nil
	default:
		return ErrNoProfileKind
	}
}

// Verdict is one decision. Allow and Consequence are mutually exclusive by construction: a refusal
// always names why, because the agent is shown the reason and "blocked" tells it nothing it can act
// on (spec §6.2).
type Verdict struct {
	Allow       bool
	Consequence browser.Consequence
	Detail      string
}

func allow() Verdict { return Verdict{Allow: true} }

func refuse(consequence browser.Consequence, format string, args ...any) Verdict {
	return Verdict{Consequence: consequence, Detail: fmt.Sprintf(format, args...)}
}

// Request is a request the browser is about to make, as CDP describes it at the request stage.
type Request struct {
	Method string
	URL    string
	// ResourceType is CDP's: "Document", "XHR", "Image", "Script", … A document is the one that
	// executes in the profile, which is why the allowlist applies to it and not to the rest (§5.5).
	ResourceType string
	Headers      map[string]string
}

// IsDocument reports whether this request would produce a document — top-level or in a frame. Spec
// §5.4 makes no distinction between the two, and the version of the spec that did left the iframe
// path open.
func (r Request) IsDocument() bool { return strings.EqualFold(r.ResourceType, "Document") }

// Decide is the request-stage rule. Pure, so the whole of §6.2 is a table test with no Chrome in it.
//
// The order of the checks decides which reason a refusal reports when more than one applies, and it
// runs from the most fundamental outwards: a scheme we do not speak, then a channel that would carry
// bytes both ways, then a method with a consequence, then whose document this is. A POST to an
// off-list host is reported as a POST, because the method is what stops it leaving.
func Decide(policy Policy, request Request) Verdict {
	parsed, err := url.Parse(request.URL)
	if err != nil {
		return refuse(browser.ConsequenceScheme, "unparseable url")
	}

	switch strings.ToLower(parsed.Scheme) {
	case "http", "https":
	case "ws", "wss":
		// Defence in depth that has never fired: the spike measured that Fetch never sees a ws: url
		// at all, so if this branch ever does trigger, Chrome changed and the proxy is no longer the
		// only thing standing between a page and a bidirectional socket. Worth keeping for that
		// signal alone.
		return refuse(browser.ConsequenceChannel, "%s is not HTTP", parsed.Scheme)
	default:
		// file:, ftp:, chrome-extension: and anything else. data:/blob:/javascript: never reach
		// here — they make no request, which is exactly why CSP and not this function contains them.
		return refuse(browser.ConsequenceScheme, "scheme %q is not allowed in agent mode", parsed.Scheme)
	}

	if isWebSocketUpgrade(request.Headers) {
		return refuse(browser.ConsequenceChannel, "websocket upgrade")
	}

	if isServiceWorkerScript(request.Headers) {
		// Spec §5.8. Everything else about this request is allowed — it is a GET for a script, often
		// from a listed origin — and letting it through installs code that keeps running after the
		// page closes and survives into the person's headful session. The spike measured that failing
		// THIS request is what stops the registration, and that it only works with the interception
		// on the browser session: on the page session the request never appears and the worker
		// installs regardless.
		return refuse(browser.ConsequenceServiceWorker, "a service worker would stay in this profile")
	}

	switch strings.ToUpper(request.Method) {
	case "GET", "HEAD":
	default:
		if request.IsDocument() {
			// A non-GET that produces a document is a form submission in all but name. Reporting it
			// as one is the difference between the agent learning "that button submits a form" and
			// learning "something was blocked".
			return refuse(browser.ConsequenceForm, "%s form submission", strings.ToUpper(request.Method))
		}
		return refuse(browser.ConsequenceMethod, "%s has a consequence", strings.ToUpper(request.Method))
	}

	// Loopback, for EVERY resource type and in both kinds of profile.
	//
	// This is the one place the fence protects us rather than the site on the other side, and it is
	// the reason spec §5.5's "sub-resources are unrestricted" cannot be read literally. Agent mode is
	// launched with --proxy-bypass-list=<-loopback> precisely so the fence sees loopback traffic; the
	// consequence is that a page CAN address this machine's own services — the núcleo's API, the
	// other sidecars, and the browser's own debugging port, all of which answer on 127.0.0.1. An
	// allowlist entry is the only way in, so a profile that has business with a local service says so
	// in configuration rather than by a page guessing a port.
	if isLoopbackURL(request.URL) {
		if !listedLoopback(loopbackOriginOf(request.URL), policy.Loopback) {
			return refuse(browser.ConsequenceLoopback, "this machine's own services are not addressable from a page")
		}
		// Admitted by name, so the site allowlist below does not also have to cover it — and could
		// not, being https-only by design. This is what makes a local dev server on plain http
		// reachable at all: it is not on the web, there is no session on it to spend, and the núcleo
		// wrote its address down on purpose.
		return allow()
	}

	if policy.Profile == Project && request.IsDocument() {
		origin := OriginOf(request.URL)
		if origin == "" || !listed(origin, policy.Origins) {
			return refuse(browser.ConsequenceOffAllowlist,
				"this profile does not admit documents from %s", describeOrigin(request.URL, origin))
		}
	}

	return allow()
}

// DecideTunnel is the CONNECT rule, and it is deliberately thin.
//
// A CONNECT tunnel is opaque: the bytes inside are TLS to a server the page chose, and no proxy that
// is not terminating that TLS can say anything about them. So this does NOT try to police method or
// content — the CDP layer does that, and it can, because it sees the request before it is encrypted.
//
// What was here first was a rule refusing any port but 443, and it was removed rather than kept for
// comfort. It bought nothing — a page that wants to reach a host of its choosing does it on 443,
// which was allowed — and it broke the case `core/src/browser_policy.rs` explicitly supports, an
// origin with its own port such as https://jira.example.com:8443. A check that blocks legitimate
// configuration while stopping nothing is worse than no check: it reads as a boundary.
//
// What remains is the boundary that is real: loopback is this machine.
func DecideTunnel(policy Policy, hostPort string) Verdict {
	host, _, err := net.SplitHostPort(hostPort)
	if err != nil || host == "" {
		return refuse(browser.ConsequenceChannel, "unreadable CONNECT target")
	}
	// CONNECT means TLS, so the origin behind it is an https one whatever the port.
	if isLoopbackHost(host) && !listedLoopback(normaliseLoopback("https://"+hostPort), policy.Loopback) {
		return refuse(browser.ConsequenceLoopback, "this machine's own services are not addressable from a page")
	}
	return allow()
}

// loopbackOriginOf reduces a url to "scheme://host:port", keeping the scheme — unlike [OriginOf],
// which drops everything that is not https. Plain http is the ordinary case for a local service, and
// there is no session on it to protect by refusing.
func loopbackOriginOf(raw string) string {
	parsed, err := url.Parse(raw)
	if err != nil {
		return ""
	}
	return normaliseLoopback(parsed.Scheme + "://" + parsed.Host)
}

func normaliseLoopback(entry string) string {
	parsed, err := url.Parse(strings.TrimSpace(entry))
	if err != nil {
		return ""
	}
	scheme := strings.ToLower(parsed.Scheme)
	if scheme != "http" && scheme != "https" {
		return ""
	}
	host := strings.ToLower(strings.TrimSuffix(parsed.Hostname(), "."))
	if host == "" || !isLoopbackHost(host) {
		// A non-loopback entry in this list is a configuration mistake that would otherwise sit
		// there matching nothing and reading as though it did something.
		return ""
	}
	port := parsed.Port()
	if port == "" {
		port = map[string]string{"http": "80", "https": "443"}[scheme]
	}
	if number, err := strconv.Atoi(port); err != nil || number <= 0 || number > 65535 {
		return ""
	}
	return scheme + "://" + host + ":" + port
}

func listedLoopback(origin string, entries []string) bool {
	if origin == "" {
		return false
	}
	for _, entry := range entries {
		if normaliseLoopback(entry) == origin {
			return true
		}
	}
	return false
}

// isLoopbackURL reports whether a url addresses this machine.
func isLoopbackURL(raw string) bool {
	parsed, err := url.Parse(raw)
	if err != nil {
		return false
	}
	return isLoopbackHost(parsed.Hostname())
}

// isLoopbackHost covers the names and the addresses, because they are the same destination and a
// check that only knew one of them would be a check the caller can spell around.
//
// Known gap, named rather than silently absent: the private ranges (10/8, 172.16/12, 192.168/16) are
// NOT refused here. They are somebody's intranet as often as they are an attack, and a browser that
// could not reach an internal Jira would be a browser nobody uses. Loopback has no such reading —
// nothing on 127.0.0.1 belongs to a web page.
func isLoopbackHost(host string) bool {
	host = strings.ToLower(strings.Trim(strings.TrimSuffix(host, "."), "[]"))
	if host == "localhost" || strings.HasSuffix(host, ".localhost") {
		return true
	}
	if address := net.ParseIP(host); address != nil {
		return address.IsLoopback() || address.IsUnspecified()
	}
	return false
}

// Response is a response the browser is about to receive, at the response stage.
type Response struct {
	URL          string
	StatusCode   int
	ResourceType string
	Headers      map[string]string
}

// IsDocument reports whether this response is a document, and therefore where CSP has to land.
func (r Response) IsDocument() bool { return strings.EqualFold(r.ResourceType, "Document") }

// DecideResponse is the response-stage rule, and it exists for one thing the request stage cannot
// see: a download announces itself in the response, not in the request (spec §6.2, test 8).
//
// Browser.setDownloadBehavior("deny") is set as well. Two mechanisms rather than one because they
// fail differently — the browser-level setting covers a download this function never sees, and this
// function covers a Chrome whose setting was renamed.
func DecideResponse(_ Policy, response Response) Verdict {
	disposition := header(response.Headers, "content-disposition")
	if strings.HasPrefix(strings.ToLower(strings.TrimSpace(disposition)), "attachment") {
		return refuse(browser.ConsequenceDownload, "the response asks to be saved to disk")
	}
	return allow()
}

// isWebSocketUpgrade reads the handshake out of the headers.
//
// Checking Upgrade rather than Connection: Connection is a hop-by-hop list that a proxy may rewrite,
// while Upgrade names the protocol being asked for. Anything asking to become a protocol that is not
// HTTP is refused, not just websocket, because the point is the channel and not the name.
func isWebSocketUpgrade(headers map[string]string) bool {
	return strings.TrimSpace(header(headers, "upgrade")) != ""
}

// isServiceWorkerScript reads the one thing that distinguishes a worker's script from any other
// script: the Service-Worker header, which the fetch for a registration's script carries by
// specification and nothing else does.
//
// Resource type would not do it — Chrome classifies the fetch as Script or Other depending on how it
// was triggered — and neither would the path, which is whatever the site called the file.
func isServiceWorkerScript(headers map[string]string) bool {
	return strings.EqualFold(strings.TrimSpace(header(headers, "service-worker")), "script")
}

// header does a case-insensitive lookup. CDP hands headers back with whatever casing the origin
// sent, so a map lookup on "Content-Disposition" misses a server that wrote it in lower case.
func header(headers map[string]string, name string) string {
	for key, value := range headers {
		if strings.EqualFold(key, name) {
			return value
		}
	}
	return ""
}

// OriginOf reduces a url to the identity the allowlist is written in, or "" if it has none.
//
// This mirrors `origin_of` in `core/src/browser_policy.rs`, including the two decisions that look
// strict and are load-bearing: https only, so an http url can never match an entry; and the port
// inside the identity, so a different port is a different service on the same machine.
//
// The one difference is IDN, and it is a difference in who does the work rather than in the answer:
// Rust's Url::parse applies IDNA itself, while here the url arrives from Chrome, which has already
// resolved it to punycode. A homograph therefore reduces to a different ASCII host in both.
func OriginOf(raw string) string {
	parsed, err := url.Parse(raw)
	if err != nil || !strings.EqualFold(parsed.Scheme, "https") {
		return ""
	}
	host := strings.ToLower(strings.TrimSuffix(parsed.Hostname(), "."))
	if host == "" {
		return ""
	}
	port := parsed.Port()
	if port == "" {
		port = "443"
	}
	if number, err := strconv.Atoi(port); err != nil || number <= 0 || number > 65535 {
		return ""
	}
	return "https://" + host + ":" + port
}

// NormaliseEntry brings a list entry into the shape [OriginOf] produces, or returns "".
//
// A bare host is accepted because that is what an owner writes in `.ai/browser.yaml`, and it gains
// https:// rather than http:// — when an entry is ambiguous, the reading that must not win is the
// permissive one.
func NormaliseEntry(entry string) string {
	entry = strings.TrimSpace(entry)
	if entry == "" || entry == "." {
		// An empty line must match nothing. Left to the parser it becomes something, and a stray
		// newline in a config file would widen the list without anyone editing it.
		return ""
	}
	if !strings.Contains(entry, "://") {
		entry = "https://" + entry
	}
	return OriginOf(entry)
}

// listed is exact whole-origin matching. No subdomains, no wildcards — see browser_policy.rs for why
// this is deliberately stricter than the trust list the rest of NucleOS uses.
func listed(origin string, entries []string) bool {
	for _, entry := range entries {
		if normalised := NormaliseEntry(entry); normalised != "" && normalised == origin {
			return true
		}
	}
	return false
}

// describeOrigin names the host in a refusal without echoing the whole url back.
//
// The url is attacker-controlled text on its way into the agent's context (spec §6.5). A path and a
// query add nothing a caller can act on and give a page somewhere to write a sentence, so only the
// origin travels — and when there is no origin, only the scheme.
func describeOrigin(raw, origin string) string {
	if origin != "" {
		return origin
	}
	if parsed, err := url.Parse(raw); err == nil && parsed.Scheme != "" {
		return parsed.Scheme + ":// (not an https origin)"
	}
	return "an unreadable url"
}
