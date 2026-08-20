//go:build browsergate

package gate_test

import (
	"context"
	"net/url"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// reading opens a page and takes one snapshot of it.
func reading(t *testing.T, path string) browser.Snapshot {
	t.Helper()
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	t.Cleanup(cancel)

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + path})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	return snapshot
}

// TestALinksAddressSurvivesTheAccessibilityTree.
//
// The MEASUREMENT this feature rests on: that Chromium reports a link's href as an accessibility
// property at all. The unit tests above build the property by hand and would pass against a tree
// that never carries one — which is the shape of every assumption this repository has had to
// retract, and it is only ever settled here.
//
// Three links called "Details" is the case the whole thing is for. Without an address they are one
// link to an agent, and it has no way to pick.
func TestALinksAddressSurvivesTheAccessibilityTree(t *testing.T) {
	snapshot := reading(t, "/links")

	var addresses []string
	for _, element := range snapshot.Elements {
		if element.Role == "link" && element.Name == "Details" {
			addresses = append(addresses, element.URL)
		}
	}
	if len(addresses) != 3 {
		t.Fatalf("expected three links called Details, got %d: %+v", len(addresses), snapshot.Elements)
	}
	for _, address := range addresses {
		if address == "" {
			t.Fatalf("a link came back with no address, so the three are one link: %v", addresses)
		}
	}
	if addresses[0] == addresses[1] {
		t.Fatalf("two links with the same words came back identical: %v", addresses)
	}
	if !strings.HasPrefix(addresses[0], "/invoices/") {
		t.Errorf("a link to this page's own origin should be a path: %q", addresses[0])
	}
	// The one that leaves the host keeps its whole address, because that is the part worth knowing.
	if !strings.HasPrefix(addresses[2], "http") {
		t.Errorf("a link off the origin should carry its whole address: %q", addresses[2])
	}
}

// TestTheFocusedElementIsNamedInTheReading.
//
// `press` with no ref sends its key wherever focus already is, so this is the reading naming the one
// element that verb was about to act on. Measured rather than assumed for the same reason as the
// link's address: `focused` being an accessibility property Chromium fills in is a claim about
// Chromium.
func TestTheFocusedElementIsNamedInTheReading(t *testing.T) {
	snapshot := reading(t, "/focus")

	focused := ""
	for _, element := range snapshot.Elements {
		for _, state := range element.State {
			if state == "focused" {
				if focused != "" {
					t.Fatalf("two elements claim the keyboard: %q and %q", focused, element.Name)
				}
				focused = element.Name
			}
		}
	}
	if focused == "" {
		t.Fatalf("nothing in the reading says where a key would land: %+v", snapshot.Elements)
	}
	if focused != "Query" {
		t.Errorf("the autofocused box is Query and the reading named %q", focused)
	}
}

// TestAPageDrawnIntoACanvasSaysSoRatherThanReadingAsEmpty.
//
// The last shape of the failure this pillar was built to end, and the one place nothing reported it.
// A chart, a map, a PDF viewer: the page loads perfectly, the accessibility tree has nothing of it,
// and the reading comes back short and confident — no `blocked`, no `truncated`, no `still_loading`.
// An agent concludes the thing it was sent for is not there, on the one page where that is most
// certainly wrong.
//
// This does not make the drawing readable. It makes the absence legible, which is the difference
// between concluding and asking.
func TestAPageDrawnIntoACanvasSaysSoRatherThanReadingAsEmpty(t *testing.T) {
	snapshot := reading(t, "/canvas")

	// The premise: the drawing really is invisible to the tree. If this ever stops being true the
	// test below is measuring nothing.
	if hasName(snapshot, "Revenue fell") {
		t.Fatal("the canvas text reached the accessibility tree; this page no longer poses the problem")
	}
	if len(snapshot.Unread) == 0 {
		t.Fatalf("a page that is entirely a drawing read as a page with nothing on it: %+v", snapshot)
	}
	found := false
	for _, one := range snapshot.Unread {
		if one.Kind == "canvas" && one.Count > 0 {
			found = true
		}
	}
	if !found {
		t.Fatalf("the drawing was not named: %+v", snapshot.Unread)
	}
}

// TestAnOrdinaryPageIsNotAccusedOfHidingSomething.
//
// The control, and it matters more than the test above. A signal that fires on every page is one an
// agent learns to ignore, and then it is worse than nothing: it is noise that also happens to be
// true where it counts.
func TestAnOrdinaryPageIsNotAccusedOfHidingSomething(t *testing.T) {
	if snapshot := reading(t, "/controls"); len(snapshot.Unread) != 0 {
		t.Fatalf("an ordinary page was reported as showing something unreadable: %+v", snapshot.Unread)
	}
}

// unreadCount is how many of one kind a reading says the page is showing.
func unreadCount(snapshot browser.Snapshot, kind string) int {
	for _, one := range snapshot.Unread {
		if one.Kind == kind {
			return one.Count
		}
	}
	return 0
}

// TestAChartInsideASameOriginFrameIsNotInvisible.
//
// The gap in the first version of `unread`, and it was not a corner case: querySelectorAll does not
// cross into an iframe's document, so the single most likely place for a chart to be — an embedded
// dashboard — was invisible to a field whose whole purpose is to notice charts. The reading came
// back short, with no `blocked`, no `truncated` and no `unread`, which is the exact silence the
// field was added to break.
func TestAChartInsideASameOriginFrameIsNotInvisible(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/framing?src=/canvas"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if unreadCount(snapshot, "canvas") == 0 {
		t.Fatalf("the framed chart was not counted, so the agent reads this page as having nothing"+
			" it cannot read: %+v", snapshot.Unread)
	}
}

// TestAChartInsideACrossOriginFrameIsNotInvisible.
//
// The other half, and it cannot be reached the same way: touching a cross-origin frame's document
// throws, by the rule the whole browser is built on. It is a separate target with its own execution
// context, so the only way to ask is to ask IT — which is what the accessibility walk already does
// for its contents, and what the reading now does for what the tree cannot carry.
//
// It uses otherHost — the same server under the name Chromium calls a different SITE — and the first
// version of this test did not, which is worth recording because it looked right and measured
// nothing. Two ports on 127.0.0.1 are cross-ORIGIN but SAME-SITE, so Chromium keeps that frame in
// the same process: there is no second target to ask, and the test failed against code that was
// correct for the case it was meant to be about. Site isolation is by site; only a different host
// gets a process of its own.
//
// Polled for the same reason the SSO test polls: a snapshot taken the instant Open returns is racing
// the frame's own load, which is a different question from this one.
func TestAChartInsideACrossOriginFrameIsNotInvisible(t *testing.T) {
	site := newSite(t)
	policy := admitting(site)
	policy.Loopback = append(policy.Loopback, otherHost(site))
	driver, _ := fenced(t, policy)
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: site.origin() + "/framing?src=" + url.QueryEscape(otherHost(site)+"/canvas"),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Refusal != nil {
		t.Fatalf("the framing page itself was refused: %+v", session.Refusal)
	}

	var last browser.Snapshot
	deadline := time.Now().Add(30 * time.Second)
	for {
		last, err = driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
		if err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		if unreadCount(last, "canvas") > 0 || time.Now().After(deadline) {
			break
		}
		time.Sleep(500 * time.Millisecond)
	}
	if unreadCount(last, "canvas") == 0 {
		t.Fatalf("the chart in the cross-site frame was not counted, so the agent reads this page as"+
			" having nothing it cannot read: %+v", last.Unread)
	}
}
