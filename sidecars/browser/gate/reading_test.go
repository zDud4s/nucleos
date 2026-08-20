//go:build browsergate

package gate_test

import (
	"context"
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
