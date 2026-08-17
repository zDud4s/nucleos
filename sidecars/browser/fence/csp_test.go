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
		"connect-src 'none'": "fetch, XHR, EventSource, beacon and BOTH ws: and wss: — the only thing that closes a WebSocket over TLS",
		"form-action 'none'": "form submission, which spec §6.2 blocks independently of method",
		"object-src 'none'":  "a plugin document, which connect-src does not govern",
		"base-uri 'none'":    "a rewritten <base>, which changes what every relative url resolves to",
		"webrtc 'block'":     "ignored by Chrome 151 and sent anyway; spec §6.2b closes when Chrome implements it",
	}
	for directive, why := range required {
		if !strings.Contains(Directives, directive) {
			t.Errorf("the fence lost %q, which is what closes: %s", directive, why)
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
