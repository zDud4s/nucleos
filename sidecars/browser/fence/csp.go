package fence

import "strings"

// HeaderName is the header the fence adds to every document response it lets through.
const HeaderName = "Content-Security-Policy"

// Directives is the fence's policy, and it is the layer that closes what CDP cannot see.
//
// Each one is here for a measured reason, not for hygiene:
//
//   - connect-src 'none' is the whole point. It closes fetch, XHR, EventSource, sendBeacon, and —
//     the reason it is here at all — WebSocket in both schemes. CSP matches a wss: url against
//     connect-src, and no source expression can admit https: while refusing wss: (CSP3's scheme
//     matching makes https: match wss: too), so there is no middle setting to reach for. The spike
//     measured 'none' holding for both, and measured a blob: document inheriting it from its parent,
//     which is the only thing that contains a blob: navigation at all.
//
//   - form-action http: https: is the one directive here that was LOOSENED, and the sentence it
//     replaces is worth keeping: it read 'none', and called itself "belt to the method filter's
//     braces: a GET form is still a submission, and the method filter would wave it through".
//
//     That belt was the only part of §6.2 decided by what an element IS rather than by what leaves
//     the machine, inside a section titled "the boundary is the network, not the intent". It made
//     <a href="/delete?id=1"> allowed and <form method=get action="/search"> refused, though both
//     are a GET to a listed origin and neither can send a byte the other cannot. Submitting a GET
//     form is navigating to action?fields, which the agent could already do with goto if it were
//     willing to build the url by hand — so what the belt cost was every search box, filter and
//     pager on the web, and what it bought was a url the agent had to assemble itself.
//
//     It also cost determinism. A POST form was stopped twice, here and by the method filter, and
//     the two raced inside Chrome: when this one won, the renderer abandoned the submission before
//     a request existed, so Fetch never paused and the agent heard the weaker of the two refusals
//     (chrome/csp.go exists because of that). A same-origin POST now meets exactly one rule.
//
//     What is left is a scheme rule with a thin, stated job: a form may only submit somewhere the
//     fence can SEE it. Where it may go stays the allowlist's decision, and whether the method has
//     a consequence stays the method filter's — both of which govern a form submission exactly as
//     they govern a goto. Chrome already refuses javascript: and data: form actions on its own, so
//     on today's build this closes blob: and nothing else. Kept for the same reason as webrtc
//     'block' below: one directive is cheap, and a redundant one costs nothing but this paragraph.
//
//   - object-src 'none' and base-uri 'none' are the two directives whose absence quietly re-opens the
//     others: a plugin document is not governed by connect-src, and a rewritten <base> changes what
//     every relative url in the page resolves to.
//
//   - webrtc 'block' is CSP3, and the spike measured Chrome 151 ignoring it — createOffer still
//     returns an SDP. It is sent anyway, and this comment is the reason: it costs one directive, an
//     unknown directive does not invalidate the rest of the policy, and on the day Chrome implements
//     it the hole in spec §6.2b closes without anyone having to remember this file exists.
//
// What is deliberately NOT here: script-src, img-src, style-src. The fence bounds what leaves the
// machine, not what the page renders. A page that cannot run its own scripts is a page the agent
// cannot read, and the taint barrier of §6.0 already assumes everything it reads is hostile.
const Directives = "connect-src 'none'; form-action http: https:; object-src 'none'; base-uri 'none'; webrtc 'block'"

// Header is one response header, in the shape CDP's Fetch.continueResponse wants.
type Header struct {
	Name  string `json:"name"`
	Value string `json:"value"`
}

// InjectCSP appends the fence's policy to a response's headers.
//
// APPENDS. Never replaces, and never merges into a policy the page already sent. Two CSP headers
// intersect — the browser enforces both, and the stricter wins every directive — so a page cannot
// loosen ours by sending its own, and we cannot accidentally loosen the page's by rewriting it. A
// version of this that parsed the existing header and merged the directives would be able to get
// that wrong; this one cannot.
func InjectCSP(headers []Header) []Header {
	out := make([]Header, 0, len(headers)+1)
	out = append(out, headers...)
	return append(out, Header{Name: HeaderName, Value: Directives})
}

// HeadersFrom converts CDP's map-shaped headers to the list shape, preserving nothing about order
// beyond what the caller gives us.
func HeadersFrom(pairs []Header) map[string]string {
	out := make(map[string]string, len(pairs))
	for _, pair := range pairs {
		out[pair.Name] = pair.Value
	}
	return out
}

// CarriesFence reports whether a header list already has the fence's policy on it. Used by the tests
// that assert every document response got one, and by the driver to avoid stacking duplicates on a
// response that passes through twice.
func CarriesFence(headers []Header) bool {
	for _, entry := range headers {
		if strings.EqualFold(entry.Name, HeaderName) && entry.Value == Directives {
			return true
		}
	}
	return false
}
