package chrome

import (
	"fmt"
	"testing"

	"nucleosbrowser/browser"
)

// listing is a page shaped like the ones this filter exists for: a directory, where the thing being
// looked for is one line among many and the many are all the same shape.
func listing(t *testing.T) []axNode {
	t.Helper()
	nodes := []axNode{
		{NodeID: "1", Role: axValue{Value: "heading"}, Name: axValue{Value: "Everything"}, BackendDOMNodeID: 10},
	}
	for i := 1; i <= 40; i++ {
		nodes = append(nodes, axNode{
			NodeID:           fmt.Sprintf("l%d", i),
			Role:             axValue{Value: "link"},
			Name:             axValue{Value: fmt.Sprintf("Report %d", i)},
			BackendDOMNodeID: int64(100 + i),
		})
	}
	nodes = append(nodes, axNode{
		NodeID: "inv", Role: axValue{Value: "link"},
		Name: axValue{Value: "Invoices"}, BackendDOMNodeID: 999,
	})
	return nodes
}

func says(elements []browser.Element, name string) bool {
	for _, one := range elements {
		if one.Name == name {
			return true
		}
	}
	return false
}

// TestFindKeepsOnlyWhatWasAskedFor.
//
// Paging is not searching. TextFrom and ControlsFrom made a long page readable in order, at a cost
// proportional to the page; finding one link in a directory still meant carrying the directory. On
// anything catalogue-shaped that was the whole turn, and the agent had read nothing it wanted.
func TestFindKeepsOnlyWhatWasAskedFor(t *testing.T) {
	driver := &Driver{}
	entry := newTestSession()
	collected, _ := collectParts(oneDocument(listing(t)), browser.SnapshotRequest{Find: "invoic"})
	elements, _ := driver.name(entry, collected, false)

	if len(elements) != 1 {
		t.Fatalf("expected the one match, got %d: %+v", len(elements), elements)
	}
	if elements[0].Name != "Invoices" {
		t.Fatalf("the wrong line came back: %+v", elements[0])
	}
	if elements[0].Ref == "" {
		t.Error("a found control with no ref is a control the agent cannot act on")
	}
}

// TestAMatchIsCaseInsensitiveAndPartial.
//
// An agent searches with the words it read, in whatever case the task wrote them. A filter that made
// it get the case right would be a filter that answers "nothing here" about a page that has it, and
// "nothing here" is the one answer this whole file exists to stop being a lie.
func TestAMatchIsCaseInsensitiveAndPartial(t *testing.T) {
	for _, needle := range []string{"INVOICES", "invoices", "Invoic", "voice"} {
		collected, _ := collectParts(oneDocument(listing(t)), browser.SnapshotRequest{Find: needle})
		driver := &Driver{}
		elements, _ := driver.name(newTestSession(), collected, false)
		if !says(elements, "Invoices") {
			t.Errorf("%q found nothing, and the page says Invoices", needle)
		}
	}
}

// TestARoleCanBeSearchedFor.
//
// "What can I press here" is a question about a page, and it is one an agent asks constantly. The
// role is part of what a line SAYS, so it is part of what a search reads.
func TestARoleCanBeSearchedFor(t *testing.T) {
	nodes := []axNode{
		{NodeID: "1", Role: axValue{Value: "button"}, Name: axValue{Value: "Approve"}, BackendDOMNodeID: 11},
		{NodeID: "2", Role: axValue{Value: "link"}, Name: axValue{Value: "Approve elsewhere"}, BackendDOMNodeID: 12},
	}
	collected, _ := collectParts(oneDocument(nodes), browser.SnapshotRequest{Find: "button"})
	driver := &Driver{}
	elements, _ := driver.name(newTestSession(), collected, false)

	if len(elements) != 1 || elements[0].Role != "button" {
		t.Fatalf("searching by role should have found the one button: %+v", elements)
	}
}

// TestProseIsSearchedByWhatItSaysAndNotByItsRole.
//
// Every paragraph on every page has the role `text`, so matching the role for prose would make the
// word "text" select the entire page — a search that answers with everything is the same failure as
// one that answers with nothing, wearing the opposite face.
func TestProseIsSearchedByWhatItSaysAndNotByItsRole(t *testing.T) {
	nodes := []axNode{
		{NodeID: "1", Role: axValue{Value: "StaticText"}, Name: axValue{Value: "Revenue fell."}},
		{NodeID: "2", Role: axValue{Value: "StaticText"}, Name: axValue{Value: "Costs rose."}},
	}
	collected, _ := collectParts(oneDocument(nodes), browser.SnapshotRequest{Find: "text"})
	if len(collected) != 0 {
		t.Fatalf("the role of prose is not something to search for: %+v", collected)
	}
}

// TestASearchDoesNotTurnTheRestOfThePageStale.
//
// The bug this guards is the one that makes a filter dangerous rather than merely narrow. Refs are
// minted from what a snapshot collected, and `entry.refs` used to become exactly that — so a search
// would have narrowed the session's whole idea of the page to the matches, and the very next act on
// something an earlier snapshot showed would come back "not in the current snapshot; take a new
// one". True about the reading, false about the page, and it would have sent the agent round a loop
// where every fresh snapshot re-broke the refs it was taken to repair.
func TestASearchDoesNotTurnTheRestOfThePageStale(t *testing.T) {
	driver := &Driver{}
	entry := newTestSession()

	whole, _ := collectParts(oneDocument(listing(t)), browser.SnapshotRequest{})
	before, _ := driver.name(entry, whole, false)
	if len(before) < 40 {
		t.Fatalf("the whole page should have come back: %d lines", len(before))
	}
	held := before[3].Ref
	if held == "" {
		t.Fatal("nothing to hold")
	}

	filtered, _ := collectParts(oneDocument(listing(t)), browser.SnapshotRequest{Find: "invoic"})
	if _, _ = driver.name(entry, filtered, false); entry.refs[held].backend == 0 {
		t.Fatalf("%s stopped naming anything because a search was taken: %v", held, entry.refs)
	}
}

// TestWhatIsGoneIsAboutThePageAndNotAboutTheSlice.
//
// `gone` is the half omission cannot express, so it has to be computed against everything on the
// page rather than against what this reading carried. A cursor or a search that made its own
// leftovers look like departures would report a page tearing itself down while it sat still.
func TestWhatIsGoneIsAboutThePageAndNotAboutTheSlice(t *testing.T) {
	driver := &Driver{}
	entry := newTestSession()

	whole, _ := collectParts(oneDocument(listing(t)), browser.SnapshotRequest{})
	if _, _ = driver.name(entry, whole, false); len(entry.refs) == 0 {
		t.Fatal("nothing was named")
	}

	again, _ := collectParts(oneDocument(listing(t)), browser.SnapshotRequest{ControlsFrom: 30})
	_, gone := driver.name(entry, again, true)
	if len(gone) != 0 {
		t.Fatalf("reading on from control 30 reported %d elements as having left the page: %v", len(gone), gone)
	}
}
