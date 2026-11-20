// §spec pilar-de-browser

package fence

import (
	"testing"

	"nucleosbrowser/browser"
)

func localPolicy() Policy {
	return Policy{
		Profile:        Project,
		Origins:        []string{"https://example.org"},
		Loopback:       []string{"http://localhost:5173", "http://localhost:8791", "https://127.0.0.1:8791"},
		ForbiddenPorts: []int{8791},
	}
}

// A listed origin is admitted, an unlisted one is refused with the origin attached.
func TestListedLoopbackOriginAdmittedUnlistedRefusedWithOrigin(t *testing.T) {
	policy := localPolicy()
	if v := Decide(policy, Request{Method: "GET", URL: "http://localhost:5173/app", ResourceType: "Document"}); !v.Allow {
		t.Fatalf("listed origin refused: %+v", v)
	}
	v := Decide(policy, Request{Method: "GET", URL: "http://localhost:3000/x", ResourceType: "XHR"})
	if v.Allow || v.Consequence != browser.ConsequenceLoopback || v.Origin != "http://localhost:3000" {
		t.Fatalf("unlisted origin = %+v", v)
	}
}

// A forbidden port is refused before the Loopback list is consulted, for every entry point.
func TestForbiddenPortRefusedEvenWhenListed(t *testing.T) {
	policy := localPolicy()
	v := Decide(policy, Request{Method: "GET", URL: "http://localhost:8791/health", ResourceType: "XHR"})
	if v.Allow || v.Consequence != browser.ConsequenceLoopback || v.Origin != "http://localhost:8791" {
		t.Fatalf("Decide on a forbidden, listed port = %+v", v)
	}
	v = DecideTunnel(policy, "127.0.0.1:8791")
	if v.Allow || v.Consequence != browser.ConsequenceLoopback || v.Origin != "https://127.0.0.1:8791" {
		t.Fatalf("DecideTunnel on a forbidden, listed port = %+v", v)
	}
	// Without the forbidden list the same entry is admitted: empty list is today's behaviour.
	policy.ForbiddenPorts = nil
	if v := Decide(policy, Request{Method: "GET", URL: "http://localhost:8791/health", ResourceType: "XHR"}); !v.Allow {
		t.Fatalf("no forbidden ports, listed entry refused: %+v", v)
	}
}

func TestLoopbackValidation(t *testing.T) {
	bad := localPolicy()
	bad.Loopback = []string{"https://example.org"}
	if bad.Validate() == nil || bad.ValidateEmbedded() == nil {
		t.Error("a non-loopback origin in Loopback was accepted")
	}
	bad = localPolicy()
	bad.ForbiddenPorts = []int{0}
	if bad.Validate() == nil {
		t.Error("port 0 accepted as a forbidden port")
	}
	if err := localPolicy().Validate(); err != nil {
		t.Errorf("valid policy refused: %v", err)
	}
}
