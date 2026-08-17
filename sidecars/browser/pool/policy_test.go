package pool

import (
	"errors"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
	"nucleosbrowser/profile"
)

// TestEachProfileKindMapsToItsFenceRule. Two enums for one distinction, and this is the only place
// they meet.
func TestEachProfileKindMapsToItsFenceRule(t *testing.T) {
	policy, err := Policy(browser.Placement{
		Profile: profile.Ref{Kind: profile.Project, ID: "acme"},
		Origins: []string{"https://jira.example.org"},
	})
	if err != nil {
		t.Fatalf("project: %v", err)
	}
	if policy.Profile != fence.Project {
		t.Errorf("project mapped to %q", policy.Profile)
	}
	if len(policy.Origins) != 1 {
		t.Errorf("the site list did not travel: %v", policy.Origins)
	}

	policy, err = Policy(browser.Placement{Profile: profile.Ref{Kind: profile.Ephemeral, ID: "r1"}})
	if err != nil {
		t.Fatalf("ephemeral: %v", err)
	}
	if policy.Profile != fence.Ephemeral {
		t.Errorf("ephemeral mapped to %q", policy.Profile)
	}
}

// TestAMappingMistakeCannotBeQuiet is the argument that makes two enums survivable, stated as a test
// rather than as a paragraph: swap the two kinds and every affected open fails loudly, because a
// project profile with no list and an ephemeral profile with one are both refused by fence.Policy.
// If either of those ever became tolerated, this pairing would become a silent mis-classification and
// the enums would have to be merged.
func TestAMappingMistakeCannotBeQuiet(t *testing.T) {
	// What a project placement would look like if it were mapped to the ephemeral rule.
	if err := (fence.Policy{Profile: fence.Ephemeral, Origins: []string{"https://jira.example.org"}}).Validate(); err == nil {
		t.Error("a site list under the ephemeral rule is accepted, so mapping project→ephemeral would be silent")
	}
	// And the other direction: an ephemeral placement carries no list, so the project rule refuses it.
	if err := (fence.Policy{Profile: fence.Project}).Validate(); err == nil {
		t.Error("an empty list under the project rule is accepted, so mapping ephemeral→project would be silent")
	}
}

// TestAPlacementNobodyDecidedIsNotAFence. It must be ErrFenceNotAttached specifically: spec §6.2a
// says a fence that is not fully there does not get to look like one, and serve maps that error to
// 503 rather than to something a caller would retry.
func TestAPlacementNobodyDecidedIsNotAFence(t *testing.T) {
	for _, placement := range []browser.Placement{
		{},
		{Profile: profile.Ref{Kind: "other", ID: "x"}},
		{Profile: profile.Ref{Kind: profile.Project, ID: "acme"}},                             // no list
		{Profile: profile.Ref{Kind: profile.Ephemeral, ID: "r1"}, Origins: []string{"a.org"}}, // a list that would be ignored
		{Profile: profile.Ref{Kind: profile.Project, ID: "acme"}, Origins: []string{"  "}},    // a list of nothing
	} {
		if _, err := Policy(placement); !errors.Is(err, browser.ErrFenceNotAttached) {
			t.Errorf("Policy(%+v) = %v, want ErrFenceNotAttached", placement, err)
		}
	}
}

// TestTheLoopbackListStaysEmpty. Nothing the núcleo sends may open it: this machine's own services —
// the núcleo's API, the other sidecars, and the browser's own debugging port, which asks for no token
// at all — are reachable from a page unless the fence refuses them (spec §6.2). It is a seam, not a
// setting, and a placement field that filled it would make it a setting.
func TestTheLoopbackListStaysEmpty(t *testing.T) {
	policy, err := Policy(browser.Placement{
		Profile: profile.Ref{Kind: profile.Project, ID: "acme"},
		Origins: []string{"https://jira.example.org"},
	})
	if err != nil {
		t.Fatalf("policy: %v", err)
	}
	if len(policy.Loopback) != 0 {
		t.Fatalf("the loopback list was filled from a placement: %v", policy.Loopback)
	}
	if verdict := fence.DecideTunnel(policy, "127.0.0.1:8791"); verdict.Allow {
		t.Fatal("the fence admits the núcleo's own API")
	}

	// And the other way in is shut too: a loopback address smuggled into the SITE list is not an
	// https origin, so the policy is refused rather than quietly carrying it.
	if _, err := Policy(browser.Placement{
		Profile: profile.Ref{Kind: profile.Project, ID: "acme"},
		Origins: []string{"https://jira.example.org", "http://127.0.0.1:8791"},
	}); !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("a loopback origin in the site list = %v, want ErrFenceNotAttached", err)
	}
}
