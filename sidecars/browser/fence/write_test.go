package fence

import (
	"strings"
	"testing"

	"nucleosbrowser/browser"
)

// TestAnEphemeralProfileWithAWriteListIsRejected.
//
// The narrower half of the rule that already refuses an origin list on a throwaway, and it is
// narrower for a reason worth keeping straight. An origin list there would be ignored; a WRITE list
// there is nonsense, because a throwaway has no login in it and there is nobody for a form to be
// submitted as.
func TestAnEphemeralProfileWithAWriteListIsRejected(t *testing.T) {
	policy := Policy{Profile: Ephemeral, Writable: []string{"https://example.org"}}
	if err := policy.Validate(); err == nil {
		t.Fatal("a throwaway accepted a write list, so it would look like it had one")
	}
}

// TestAWriteListEntryThatMatchesNothingIsRejected.
//
// The same argument the origin list makes: an entry that normalises to nothing sits there matching
// no request and reading, on the screen where somebody granted it, as though it did something.
func TestAWriteListEntryThatMatchesNothingIsRejected(t *testing.T) {
	policy := Policy{Profile: Project,
		Origins:  []string{"https://example.org"},
		Writable: []string{"not a url at all"}}
	if err := policy.Validate(); err == nil {
		t.Fatal("an unusable write entry was accepted")
	}

	usable := Policy{Profile: Project,
		Origins:  []string{"https://example.org"},
		Writable: []string{"example.org", "http://localhost:3000"}}
	if err := usable.Validate(); err != nil {
		t.Fatalf("a usable write list was rejected: %v", err)
	}
}

// TestEachWayAFormIsRefusedSaysSomethingDifferent.
//
// The claim this makes is not that the refusals are worded nicely. It is that they DISCRIMINATE:
// four conditions fail four different ways, and each one points at a different next move — ask a
// person for the wheel, act on the form instead of watching the page submit it, aim at the origin
// you are on, or stop. A fence that answered "form-submission" to all four would leave an agent with
// no move except to try again, which is the one thing a refusal exists to prevent.
//
// It also pins the two orderings that were decided rather than fallen into: the act is asked about
// before the grant, and the origin is asked about before the grant. A refusal naming the handoff for
// a request that would not leave even after somebody granted it sends a person to hand over a
// permission that changes nothing.
func TestEachWayAFormIsRefusedSaysSomethingDifferent(t *testing.T) {
	policy := Policy{Profile: Project,
		Origins:  []string{"https://example.org", "https://jira.example.com"},
		Writable: []string{"https://example.org"}}

	details := map[string]string{}
	for name, request := range map[string]Request{
		"nothing acted": {Method: "POST", URL: "https://example.org/reply", ResourceType: "Document"},
		"another origin": {Method: "POST", URL: "https://jira.example.com/x", ResourceType: "Document",
			Armed: "https://example.org:443"},
		"no grant": {Method: "POST", URL: "https://jira.example.com/x", ResourceType: "Document",
			Armed: "https://jira.example.com:443"},
	} {
		verdict := Decide(policy, request)
		if verdict.Allow {
			t.Fatalf("%s: allowed, and every one of these should be refused", name)
		}
		if verdict.Consequence != browser.ConsequenceForm {
			t.Fatalf("%s: reported as %q, want a form submission", name, verdict.Consequence)
		}
		details[name] = verdict.Detail
	}

	for one, first := range details {
		for other, second := range details {
			if one != other && first == second {
				t.Fatalf("%q and %q are refused with the same words, so the agent cannot tell them"+
					" apart: %q", one, other, first)
			}
		}
	}

	// The one that names a way forward is the one a person can act on, and only that one. Naming the
	// handoff in the others would send somebody to grant a permission that changes nothing.
	if !strings.Contains(details["no grant"], "browser_handoff") {
		t.Errorf("the refusal a person can fix does not say how: %q", details["no grant"])
	}
	if strings.Contains(details["nothing acted"], "browser_handoff") {
		t.Errorf("a form the page submitted itself points at a person, and no grant would let it"+
			" through: %q", details["nothing acted"])
	}
	if strings.Contains(details["another origin"], "browser_handoff") {
		t.Errorf("a cross-origin form points at a person, and no grant makes it something else: %q",
			details["another origin"])
	}
}

// TestOnlyAFormSubmissionAsksForAWriteWindow.
//
// NeedsWriteWindow is what tells the driver when to go looking for an armed window, and it is in
// this package precisely so it cannot drift from the rule that judges. This is the assertion that
// keeps the two in step: everything Decide would judge by the write rule, and nothing else.
func TestOnlyAFormSubmissionAsksForAWriteWindow(t *testing.T) {
	cases := []struct {
		request Request
		want    bool
	}{
		{Request{Method: "POST", URL: "https://example.org/x", ResourceType: "Document"}, true},
		{Request{Method: "post", URL: "https://example.org/x", ResourceType: "document"}, true},
		{Request{Method: "POST", URL: "https://example.org/x", ResourceType: "XHR"}, false},
		{Request{Method: "GET", URL: "https://example.org/x", ResourceType: "Document"}, false},
		{Request{Method: "DELETE", URL: "https://example.org/x", ResourceType: "Document"}, false},
	}
	for _, one := range cases {
		if got := NeedsWriteWindow(one.request); got != one.want {
			t.Errorf("%s %s: got %v, want %v", one.request.Method, one.request.ResourceType, got, one.want)
		}
	}
}

// TestTheOriginAWriteIsCheckedAgainstIsTheOneTheGrantIsWrittenIn.
//
// Both sides of the write rule go through WriteOriginOf: the driver arms with it, and the fence
// judges with it. If the two shapes disagreed by so much as a default port, every grant in the
// product would be inert and the refusal would say the origin was not writable while naming the
// origin that is.
func TestTheOriginAWriteIsCheckedAgainstIsTheOneTheGrantIsWrittenIn(t *testing.T) {
	cases := map[string]string{
		"https://example.org/reply":          "https://example.org:443",
		"https://example.org:443/reply":      "https://example.org:443",
		"https://EXAMPLE.org./reply?x=1":     "https://example.org:443",
		"https://example.org:8443/reply":     "https://example.org:8443",
		"http://localhost:3000/save":         "http://localhost:3000",
		"http://127.0.0.1/save":              "http://127.0.0.1:80",
		"http://example.org/reply":           "",
		"file:///etc/passwd":                 "",
		"https://xn--exmple-cua.org/reply":   "https://xn--exmple-cua.org:443",
		"https://user:pw@example.org/reply":  "https://example.org:443",
		"https://jira.example@evil.net/post": "https://evil.net:443",
	}
	for raw, want := range cases {
		if got := WriteOriginOf(raw); got != want {
			t.Errorf("WriteOriginOf(%q) = %q, want %q", raw, got, want)
		}
	}
}
