package fence

import (
	"strings"
	"testing"
)

// TestTheFencePolicyCarriesTheDirectivesItIsFor.
//
// Asserted by name rather than by comparing the whole string, so that adding a directive does not
// break the test while removing the one that closes wss: does. connect-src is the load-bearing one:
// it is the only mechanism in the whole pillar that closes a WebSocket over TLS.
func TestTheFencePolicyCarriesTheDirectivesItIsFor(t *testing.T) {
	required := map[string]string{
		"connect-src 'none'":       "fetch, XHR, EventSource, beacon and BOTH ws: and wss: — the only thing that closes a WebSocket over TLS",
		"form-action http: https:": "a form submitting anywhere the fence cannot see it; where it may GO is the allowlist's rule",
		"object-src 'none'":        "a plugin document, which connect-src does not govern",
		"base-uri 'none'":          "a rewritten <base>, which changes what every relative url resolves to",
		"webrtc 'block'":           "ignored by Chrome 151 and sent anyway; spec §6.2b closes when Chrome implements it",
	}
	for directive, why := range required {
		if !strings.Contains(Directives, directive) {
			t.Errorf("the fence lost %q, which is what closes: %s", directive, why)
		}
	}
}

// TestTheFenceDoesNotDecideWhetherSomethingIsAForm.
//
// The directive used to be 'none', and this test is the guard on putting it back — which is easy to
// do by reflex, because "the fence blocks form submission" reads like a security property and is not
// one. A GET form submits to action?fields, which is a document GET; the same GET reached by a link
// or by goto was always allowed. Refusing it decided by what the ELEMENT is, in a section of §6.2
// whose title is that the boundary is the network and not the intent.
//
// The rules that DO bound a form submission are asserted next door, in policy_test.go: a POST is
// refused by the method filter, and an off-allowlist target is refused as a document. Neither of
// them can be satisfied by a directive here, and neither of them needs one.
func TestTheFenceDoesNotDecideWhetherSomethingIsAForm(t *testing.T) {
	if strings.Contains(Directives, "form-action 'none'") {
		t.Error("form-action is back to 'none': every search box, filter and pager on the web is now" +
			" refused, and a POST form is stopped twice by racing layers again")
	}
	for _, scheme := range []string{"http:", "https:"} {
		if !strings.Contains(Directives, scheme) {
			t.Errorf("form-action no longer admits %s, so an ordinary submission is refused here"+
				" instead of being judged by the method filter and the allowlist", scheme)
		}
	}
}

// TestTheFenceDoesNotGovernRendering. script-src and friends would stop the page running at all, and
// a page the agent cannot read is not a page it is safer with — the taint barrier of spec §6.0
// already assumes everything read here is hostile.
func TestTheFenceDoesNotGovernRendering(t *testing.T) {
	for _, directive := range []string{"script-src", "img-src", "style-src", "default-src"} {
		if strings.Contains(Directives, directive) {
			t.Errorf("the fence carries %q; it bounds what leaves, not what renders", directive)
		}
	}
}

// TestInjectCSPAppends. Two CSP headers intersect and the stricter wins every directive, so
// appending cannot be loosened by the page and cannot loosen the page. A version that parsed and
// merged could get that wrong; this one has no way to.
func TestInjectCSPAppends(t *testing.T) {
	original := []Header{
		{Name: "Content-Type", Value: "text/html"},
		{Name: HeaderName, Value: "default-src 'self'"},
	}
	injected := InjectCSP(original)

	if len(injected) != len(original)+1 {
		t.Fatalf("got %d headers, want %d", len(injected), len(original)+1)
	}
	for i, header := range original {
		if injected[i] != header {
			t.Fatalf("header %d was rewritten: %+v", i, injected[i])
		}
	}
	if !CarriesFence(injected) {
		t.Fatal("the fence's policy is not on the result")
	}

	// The input must not be modified in place: the caller still holds it, and CDP's paused-request
	// struct is reused for the refusal record.
	if len(original) != 2 || original[1].Value != "default-src 'self'" {
		t.Fatalf("the caller's headers were mutated: %+v", original)
	}
}

func TestCarriesFence(t *testing.T) {
	if CarriesFence(nil) {
		t.Error("an empty header list reported a fence")
	}
	if CarriesFence([]Header{{Name: HeaderName, Value: "default-src 'self'"}}) {
		t.Error("somebody else's CSP was mistaken for ours; a second injection would be skipped")
	}
	if !CarriesFence([]Header{{Name: "content-security-policy", Value: Directives}}) {
		t.Error("the header name is compared case-sensitively; a lower-case one would be stacked twice")
	}
}

func TestHeadersFrom(t *testing.T) {
	got := HeadersFrom([]Header{
		{Name: "Content-Type", Value: "text/html"},
		{Name: "content-disposition", Value: "attachment"},
	})
	if got["Content-Type"] != "text/html" || got["content-disposition"] != "attachment" {
		t.Fatalf("headers came through as %+v", got)
	}
}
