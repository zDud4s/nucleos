package chrome

import (
	"strings"
	"testing"

	"nucleosbrowser/browser"
)

// snapshotFrom runs the two halves the driver runs: read the tree, then name what it found. The
// split exists because refs belong to a session and the tree does not, so a test that wants to see
// refs has to have a session too.
func snapshotFrom(nodes []axNode) ([]browser.Element, map[string]int64, bool) {
	driver := &Driver{}
	entry := &session{refByNode: map[int64]string{}, lastReported: map[string]browser.Element{}}
	collected, truncated := collect(nodes)
	elements, _ := driver.name(entry, collected, false)
	return elements, entry.refs, truncated
}

func text(id, parent, value string) axNode {
	return axNode{NodeID: id, Role: axValue{Value: "StaticText"}, Name: axValue{Value: value}}
}

// TestAButtonsOwnTextIsNotRepeatedBesideIt.
//
// A control's accessible name comes FROM its StaticText child, so a version of this that emitted
// both would say everything twice — and on a page that is mostly links, "twice" is the difference
// between a snapshot an agent can read and one that fills the turn.
func TestAButtonsOwnTextIsNotRepeatedBesideIt(t *testing.T) {
	nodes := []axNode{
		{NodeID: "1", Role: axValue{Value: "button"}, Name: axValue{Value: "Sign in"},
			ChildIDs: []string{"2"}, BackendDOMNodeID: 11},
		text("2", "1", "Sign in"),
		text("3", "", "You need an account to continue."),
	}

	elements, refs, truncated := snapshotFrom(nodes)

	if truncated {
		t.Error("nothing was near the budget")
	}
	if len(elements) != 2 {
		t.Fatalf("expected the button and the paragraph, got %d: %+v", len(elements), elements)
	}
	if elements[0].Role != "button" || elements[0].Ref != "e1" {
		t.Errorf("first should be the button with a ref: %+v", elements[0])
	}
	if elements[1].Role != "text" || elements[1].Name != "You need an account to continue." {
		t.Errorf("second should be the prose: %+v", elements[1])
	}
	if elements[1].Ref != "" {
		t.Error("prose carries no ref: no act in the set does anything to a paragraph")
	}
	if refs["e1"] != 11 {
		t.Errorf("the ref must resolve to the button's node: %v", refs)
	}
}

// TestBothHalvesOfACheckboxAreSpelledOut.
//
// An absent `checked` is indistinguishable from an element that has no checked state at all, and
// that is precisely the distinction a checkbox turns on. So `unchecked` is said rather than implied.
func TestBothHalvesOfACheckboxAreSpelledOut(t *testing.T) {
	nodes := []axNode{
		{NodeID: "1", Role: axValue{Value: "checkbox"}, Name: axValue{Value: "Remember me"},
			Properties: []axProperty{{Name: "checked", Value: axValue{Value: "false"}}}},
		{NodeID: "2", Role: axValue{Value: "checkbox"}, Name: axValue{Value: "Send updates"},
			Properties: []axProperty{{Name: "checked", Value: axValue{Value: "true"}}}},
		{NodeID: "3", Role: axValue{Value: "button"}, Name: axValue{Value: "Continue"},
			Properties: []axProperty{{Name: "disabled", Value: axValue{Value: "true"}}}},
	}

	elements, _, _ := snapshotFrom(nodes)

	if len(elements) != 3 {
		t.Fatalf("got %d: %+v", len(elements), elements)
	}
	if strings.Join(elements[0].State, ",") != "unchecked" {
		t.Errorf("an unticked box must say so: %+v", elements[0])
	}
	if strings.Join(elements[1].State, ",") != "checked" {
		t.Errorf("a ticked box must say so: %+v", elements[1])
	}
	if strings.Join(elements[2].State, ",") != "disabled" {
		t.Errorf("a dead button must say so, or the agent presses it forever: %+v", elements[2])
	}
}

// TestATextboxReportsWhatIsInIt. Without this an agent that types cannot read back what it typed,
// which turns every form into an open loop.
func TestATextboxReportsWhatIsInIt(t *testing.T) {
	nodes := []axNode{
		{NodeID: "1", Role: axValue{Value: "textbox"}, Name: axValue{Value: "Email"},
			Value: axValue{Value: "  someone@example.org "}},
	}

	elements, _, _ := snapshotFrom(nodes)

	if len(elements) != 1 || elements[0].Value != "someone@example.org" {
		t.Fatalf("the box's contents did not come back trimmed: %+v", elements)
	}
}

// TestTheTextBudgetDropsProseAndNeverControls.
//
// The order of sacrifice is the decision here. Controls are what the agent acts on, so a page whose
// buttons went missing to make room for prose would be a page it cannot use at all — the budget
// takes text and says it did.
func TestTheTextBudgetDropsProseAndNeverControls(t *testing.T) {
	nodes := []axNode{{NodeID: "start", Role: axValue{Value: "heading"}, Name: axValue{Value: "Top"}}}
	for i := 0; i < 40; i++ {
		nodes = append(nodes, text(string(rune('a'+i)), "", strings.Repeat("x", 1000)))
	}
	nodes = append(nodes, axNode{
		NodeID: "end", Role: axValue{Value: "button"}, Name: axValue{Value: "Buried"},
	})

	elements, _, truncated := snapshotFrom(nodes)

	if !truncated {
		t.Fatal("40000 characters of prose against a 20000 budget must report truncation")
	}
	var buried, spent bool
	for _, element := range elements {
		if element.Name == "Buried" {
			buried = true
		}
		if element.Role == "text" {
			spent = true
		}
	}
	if !buried {
		t.Error("a control past the budget was dropped; controls are never what gets cut")
	}
	if !spent {
		t.Error("no prose survived at all, so the budget is not a budget")
	}
}

// TestProseInsideAControlIsSkippedHoweverDeep. The walk goes up parents rather than checking the
// immediate one: a button containing a span containing the text is the ordinary shape on the web.
func TestProseInsideAControlIsSkippedHoweverDeep(t *testing.T) {
	nodes := []axNode{
		{NodeID: "1", Role: axValue{Value: "link"}, Name: axValue{Value: "Read more"},
			ChildIDs: []string{"2"}},
		{NodeID: "2", Role: axValue{Value: "generic"}, ChildIDs: []string{"3"}},
		text("3", "2", "Read more"),
	}

	elements, _, _ := snapshotFrom(nodes)

	if len(elements) != 1 || elements[0].Role != "link" {
		t.Fatalf("the nested text was emitted beside its own link: %+v", elements)
	}
}

var _ = browser.Element{}

// TestARefMeansTheSameElementOnTheNextSnapshot.
//
// The promise the agent already assumed and did not have. Refs were minted by position — first
// interesting node is e1 — so a click that inserted one row above renamed everything below it, and
// an agent holding "e2" from the previous snapshot was holding a name for something else. It acted
// on the wrong thing and nothing anywhere reported an error, because both snapshots were correct
// readings of their own instant.
//
// Keying on the backend node id, which Chromium keeps stable for the life of the node, makes a ref a
// handle on an ELEMENT rather than on a position.
func TestARefMeansTheSameElementOnTheNextSnapshot(t *testing.T) {
	driver := &Driver{}
	entry := &session{refByNode: map[int64]string{}, lastReported: map[string]browser.Element{}}
	take := func(nodes []axNode) map[string]string {
		collected, _ := collect(nodes)
		elements, _ := driver.name(entry, collected, false)
		byName := map[string]string{}
		for _, element := range elements {
			if element.Ref != "" {
				byName[element.Name] = element.Ref
			}
		}
		return byName
	}

	before := take([]axNode{
		{NodeID: "a", Role: axValue{Value: "button"}, Name: axValue{Value: "Save"}, BackendDOMNodeID: 10},
		{NodeID: "b", Role: axValue{Value: "button"}, Name: axValue{Value: "Cancel"}, BackendDOMNodeID: 20},
	})

	// A row appears ABOVE both. By position everything below would shift by one.
	after := take([]axNode{
		{NodeID: "new", Role: axValue{Value: "button"}, Name: axValue{Value: "Undo"}, BackendDOMNodeID: 5},
		{NodeID: "a", Role: axValue{Value: "button"}, Name: axValue{Value: "Save"}, BackendDOMNodeID: 10},
		{NodeID: "b", Role: axValue{Value: "button"}, Name: axValue{Value: "Cancel"}, BackendDOMNodeID: 20},
	})

	if after["Save"] != before["Save"] || after["Cancel"] != before["Cancel"] {
		t.Fatalf("refs moved under the agent: %v then %v", before, after)
	}
	if after["Undo"] == before["Save"] || after["Undo"] == before["Cancel"] {
		t.Fatalf("a new element took a ref that already meant something: %v", after)
	}
	if entry.refs[after["Save"]] != 10 {
		t.Errorf("the ref no longer resolves to its node: %v", entry.refs)
	}
}

// TestAChangesOnlyReadCarriesWhatMovedAndWhatLeft.
//
// The saving this exists for is real: an agent's loop is act, snapshot, act, and paying for the
// whole page after every click is most of what a browsing turn costs. What makes it safe rather than
// merely cheap is `gone`. A full snapshot says an element has disappeared by not containing it; a
// partial one says nothing at all by not containing it, so disappearance has to be stated or the
// agent goes on believing in a button that is no longer there.
func TestAChangesOnlyReadCarriesWhatMovedAndWhatLeft(t *testing.T) {
	driver := &Driver{}
	entry := &session{refByNode: map[int64]string{}, lastReported: map[string]browser.Element{}}
	take := func(nodes []axNode, changesOnly bool) ([]browser.Element, []string) {
		collected, _ := collect(nodes)
		return driver.name(entry, collected, changesOnly)
	}

	page := []axNode{
		{NodeID: "t", Role: axValue{Value: "StaticText"}, Name: axValue{Value: "A paragraph nobody edited."}},
		{NodeID: "a", Role: axValue{Value: "button"}, Name: axValue{Value: "Save"}, BackendDOMNodeID: 10},
		{NodeID: "b", Role: axValue{Value: "checkbox"}, Name: axValue{Value: "Agree"}, BackendDOMNodeID: 20,
			Properties: []axProperty{{Name: "checked", Value: axValue{Value: "false"}}}},
		{NodeID: "c", Role: axValue{Value: "button"}, Name: axValue{Value: "Cancel"}, BackendDOMNodeID: 30},
	}
	if full, _ := take(page, false); len(full) != 4 {
		t.Fatalf("the first read is the whole page: %+v", full)
	}

	// The box gets ticked and Cancel disappears. Nothing else moves.
	changed := []axNode{
		{NodeID: "t", Role: axValue{Value: "StaticText"}, Name: axValue{Value: "A paragraph nobody edited."}},
		{NodeID: "a", Role: axValue{Value: "button"}, Name: axValue{Value: "Save"}, BackendDOMNodeID: 10},
		{NodeID: "b", Role: axValue{Value: "checkbox"}, Name: axValue{Value: "Agree"}, BackendDOMNodeID: 20,
			Properties: []axProperty{{Name: "checked", Value: axValue{Value: "true"}}}},
	}
	elements, gone := take(changed, true)

	if len(elements) != 1 || elements[0].Name != "Agree" {
		t.Fatalf("only the checkbox moved; got %+v", elements)
	}
	if len(elements[0].State) != 1 || elements[0].State[0] != "checked" {
		t.Errorf("the change itself is missing: %+v", elements[0])
	}
	if len(gone) != 1 {
		t.Fatalf("Cancel left the page and was not reported: %v", gone)
	}

	// A changes-only read that follows a changes-only read compares against what was ACTUALLY sent,
	// not against the last full page — otherwise the same change would be reported for ever.
	again, goneAgain := take(changed, true)
	if len(again) != 0 || len(goneAgain) != 0 {
		t.Errorf("nothing moved and something was reported: %+v %v", again, goneAgain)
	}
}
