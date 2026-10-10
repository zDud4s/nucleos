package chrome

import (
	"fmt"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// snapshotFrom runs the two halves the driver runs: read the tree, then name what it found. The
// split exists because refs belong to a session and the tree does not, so a test that wants to see
// refs has to have a session too.
func snapshotFrom(nodes []axNode) ([]browser.Element, map[string]int64, bool) {
	driver := &Driver{}
	entry := newTestSession()
	collected, truncated := collectParts(oneDocument(nodes), browser.SnapshotRequest{})
	elements, _ := driver.name(entry, collected, false)
	return elements, backends(entry.refs), truncated
}

// testPage is where these trees pretend to have come from, which is what a link's address is
// shortened against.
const testPage = "https://example.org/here"

// oneDocument wraps a node list as a page with nothing framed in it, which is what every test in
// this file is about — the framing is measured against real Chromium in the gate, because a fake
// tree cannot have a process boundary in it.
func oneDocument(nodes []axNode) *tree {
	return &tree{nodes: nodes, inner: map[int64]*tree{}}
}

func newTestSession() *session {
	return &session{refByNode: map[nodeKey]string{}, lastReported: map[string]browser.Element{}}
}

// backends drops the document a ref belongs to, so these tests can keep saying "e1 is node 11".
func backends(refs map[string]nodeKey) map[string]int64 {
	flat := make(map[string]int64, len(refs))
	for ref, key := range refs {
		flat[ref] = key.backend
	}
	return flat
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
	entry := newTestSession()
	take := func(nodes []axNode) map[string]string {
		collected, _ := collectParts(oneDocument(nodes), browser.SnapshotRequest{})
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
	if entry.refs[after["Save"]].backend != 10 {
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
	entry := newTestSession()
	take := func(nodes []axNode, changesOnly bool) ([]browser.Element, []string) {
		collected, _ := collectParts(oneDocument(nodes), browser.SnapshotRequest{})
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

// sliceFrom is snapshotFrom with the prose cursor, for the tests that are about continuation.
// collectParts keeps the older tests reading the way they did, before one snapshot had two budgets.
func collectParts(root *tree, req browser.SnapshotRequest) ([]found, bool) {
	read := collect(root, req, testPage)
	return read.elements, read.truncated
}

func sliceFrom(nodes []axNode, textFrom int) ([]browser.Element, bool, int) {
	read := collect(oneDocument(nodes), browser.SnapshotRequest{TextFrom: textFrom}, testPage)
	elements, _ := (&Driver{}).name(newTestSession(), read.elements, false)
	return elements, read.truncated, read.textNext
}

// controlsFrom is sliceFrom for the other budget.
func controlsFrom(nodes []axNode, from int) ([]browser.Element, bool, int) {
	read := collect(oneDocument(nodes), browser.SnapshotRequest{ControlsFrom: from}, testPage)
	elements, _ := (&Driver{}).name(newTestSession(), read.elements, false)
	return elements, read.truncated, read.controlsNext
}

// paragraphs builds a page of numbered blocks, each big enough that a handful fills the budget.
func paragraphs(count, size int) []axNode {
	nodes := make([]axNode, 0, count)
	for i := 0; i < count; i++ {
		body := fmt.Sprintf("[%02d]%s", i, strings.Repeat("x", size-4))
		nodes = append(nodes, text(fmt.Sprintf("p%d", i), "", body))
	}
	return nodes
}

func proseOf(elements []browser.Element) string {
	var all strings.Builder
	for _, element := range elements {
		if element.Role == "text" {
			all.WriteString(element.Name)
		}
	}
	return all.String()
}

// TestACutPageCanBeReadOnFromWhereItStopped.
//
// Truncation without a continuation is a dead end. The budget is not about the viewport, so no
// amount of scrolling moves it: the agent is told the page goes on and has no verb that reaches the
// rest, which is a worse position than not being told at all — it knows something is there and
// cannot get it.
func TestACutPageCanBeReadOnFromWhereItStopped(t *testing.T) {
	nodes := paragraphs(45, 1000)

	first, truncated, next := sliceFrom(nodes, 0)
	if !truncated {
		t.Fatal("45000 characters against a 20000 budget must report truncation")
	}
	if next <= 0 {
		t.Fatal("a cut snapshot must say where to read on from")
	}
	if !strings.Contains(proseOf(first), "[00]") {
		t.Error("the first slice does not start at the beginning")
	}

	second, stillMore, _ := sliceFrom(nodes, next)
	prose := proseOf(second)
	if strings.Contains(prose, "[00]") {
		t.Error("the second slice repeated what the first already delivered")
	}
	if !strings.Contains(prose, "[20]") {
		t.Errorf("the second slice does not carry on where the first stopped; it had: %.40q", prose)
	}
	if !stillMore {
		t.Error("forty-five blocks do not fit in two slices of twenty; the second must still offer more")
	}
}

// TestTheCutIsAPrefixAndNotASieve.
//
// Skip-and-continue was the shape this had: a paragraph too big for what was left of the budget was
// dropped, and a shorter one further down was let through. The result is a page nobody wrote, and
// nothing in the snapshot distinguishes it from the page.
func TestTheCutIsAPrefixAndNotASieve(t *testing.T) {
	nodes := append(paragraphs(21, 1000), text("short", "", "[99]tail"))

	elements, truncated, _ := sliceFrom(nodes, 0)
	if !truncated {
		t.Fatal("this page is over the budget")
	}
	if strings.Contains(proseOf(elements), "[99]") {
		t.Error("a short block from past the cut was let through while a long one before it was dropped")
	}
}

// TestAWholePageOffersNoContinuation.
//
// The offer is the presence of the offset. A page that fits must not carry one, or an agent that
// follows it politely reads the same page twice.
func TestAWholePageOffersNoContinuation(t *testing.T) {
	elements, truncated, next := sliceFrom(paragraphs(3, 100), 0)
	if truncated || next != 0 {
		t.Errorf("a page that fits was reported as cut: truncated=%v next=%d", truncated, next)
	}
	if len(elements) != 3 {
		t.Errorf("expected the whole page: %+v", elements)
	}
}

// links builds a page of numbered links, which is what a directory listing is.
func links(count int) []axNode {
	nodes := make([]axNode, 0, count)
	for i := 0; i < count; i++ {
		nodes = append(nodes, axNode{
			NodeID:           fmt.Sprintf("l%d", i),
			Role:             axValue{Value: "link"},
			Name:             axValue{Value: fmt.Sprintf("item %03d", i)},
			BackendDOMNodeID: int64(1000 + i),
		})
	}
	return nodes
}

func controlsIn(elements []browser.Element) []browser.Element {
	var out []browser.Element
	for _, element := range elements {
		if element.Ref != "" {
			out = append(out, element)
		}
	}
	return out
}

// TestTheActionableSetHasABoundToo.
//
// "Controls are never dropped" was written against prose crowding out a button, and for that it is
// right. On a directory listing with two thousand links it meant a snapshot with NO bound at all,
// reported as `truncated: false` because the prose had fit — and the failure did not arrive as an
// error, it arrived as a turn with no room left to think in.
func TestTheActionableSetHasABoundToo(t *testing.T) {
	elements, truncated, next := controlsFrom(links(500), 0)

	if !truncated {
		t.Fatal("five hundred links came back claiming to be the whole page")
	}
	if got := len(controlsIn(elements)); got != controlBudget {
		t.Errorf("carried %d controls against a budget of %d", got, controlBudget)
	}
	if next != controlBudget {
		t.Errorf("the cursor must point at the first control not delivered, got %d", next)
	}
}

// TestTheActionableSetCanBeReadOnFrom.
func TestTheActionableSetCanBeReadOnFrom(t *testing.T) {
	nodes := links(500)

	first, _, next := controlsFrom(nodes, 0)
	second, stillMore, _ := controlsFrom(nodes, next)

	if stillMore {
		t.Error("five hundred links fit in two slices of three hundred")
	}
	names := map[string]bool{}
	for _, element := range controlsIn(first) {
		names[element.Name] = true
	}
	for _, element := range controlsIn(second) {
		if names[element.Name] {
			t.Fatalf("the second slice repeated %q from the first", element.Name)
		}
	}
	if got := len(controlsIn(first)) + len(controlsIn(second)); got != 500 {
		t.Errorf("the two slices together are not the page: %d links", got)
	}
}

// TestALongArticleStillKeepsItsButtons.
//
// The rule the budgets were split to preserve. Bounding prose and controls together would mean a
// page of text costing the agent the one thing it can act on.
func TestALongArticleStillKeepsItsButtons(t *testing.T) {
	nodes := append(paragraphs(40, 1000), links(5)...)

	elements, truncated, _ := sliceFrom(nodes, 0)

	if !truncated {
		t.Fatal("forty thousand characters is over the budget")
	}
	if got := len(controlsIn(elements)); got != 5 {
		t.Errorf("prose crowded out the controls after all: %d of 5", got)
	}
}

// cellOf builds one table cell holding some text.
func cellOf(id, role, body string) []axNode {
	return []axNode{
		{NodeID: id, Role: axValue{Value: role}, ChildIDs: []string{id + "t"}},
		text(id+"t", id, body),
	}
}

func tableOf() []axNode {
	nodes := []axNode{
		{NodeID: "tbl", Role: axValue{Value: "table"}, ChildIDs: []string{"r1", "r2"}},
		{NodeID: "r1", Role: axValue{Value: "row"}, ChildIDs: []string{"h1", "h2"}},
		{NodeID: "r2", Role: axValue{Value: "row"}, ChildIDs: []string{"c1", "c2"}},
	}
	nodes = append(nodes, cellOf("h1", "columnheader", "Quarter")...)
	nodes = append(nodes, cellOf("h2", "columnheader", "Revenue")...)
	nodes = append(nodes, cellOf("c1", "cell", "Q1")...)
	nodes = append(nodes, cellOf("c2", "cell", "-11%")...)
	return nodes
}

// TestATableComesBackAsRowsAndNotAsLooseCells.
//
// The accessibility tree HAS the grid. Dropping the table roles at the door turned it into a stream
// of numbers in reading order — the agent could read every figure and could not say which column
// any of them was in, which for a table is the whole of the information.
func TestATableComesBackAsRowsAndNotAsLooseCells(t *testing.T) {
	elements, _, _ := sliceFrom(tableOf(), 0)

	var rows []string
	for _, element := range elements {
		if element.Role == "row" {
			rows = append(rows, element.Name)
		}
		if element.Role == "text" {
			t.Errorf("a cell came back loose, outside its row: %q", element.Name)
		}
	}
	if len(rows) != 2 {
		t.Fatalf("expected a header row and a body row, got %d: %+v", len(rows), rows)
	}
	if rows[0] != "Quarter | Revenue" {
		t.Errorf("the headers lost their shape: %q", rows[0])
	}
	if rows[1] != "Q1 | -11%" {
		t.Errorf("the row lost its shape: %q", rows[1])
	}
}

// TestALinkInACellKeepsItsRefAndItsPlaceInTheRow.
//
// The one place this file says something twice on purpose. The alternative is a row reading
// " | 3 days ago", with a hole where the link was, and a hole in a table is worse than a word
// repeated — the agent cannot tell an empty first column from a column it was not shown.
func TestALinkInACellKeepsItsRefAndItsPlaceInTheRow(t *testing.T) {
	nodes := []axNode{
		{NodeID: "tbl", Role: axValue{Value: "table"}, ChildIDs: []string{"r1"}},
		{NodeID: "r1", Role: axValue{Value: "row"}, ChildIDs: []string{"c1", "c2"}},
		{NodeID: "c1", Role: axValue{Value: "cell"}, ChildIDs: []string{"a"}},
		{NodeID: "a", Role: axValue{Value: "link"}, Name: axValue{Value: "Getting started"},
			ChildIDs: []string{"at"}, BackendDOMNodeID: 77},
		text("at", "a", "Getting started"),
	}
	nodes = append(nodes, cellOf("c2", "cell", "3 days ago")...)

	elements, _, _ := sliceFrom(nodes, 0)

	var row string
	var linkRef string
	for _, element := range elements {
		if element.Role == "row" {
			row = element.Name
		}
		if element.Role == "link" {
			linkRef = element.Ref
		}
	}
	if row != "Getting started | 3 days ago" {
		t.Errorf("the row has a hole where the link is: %q", row)
	}
	if linkRef == "" {
		t.Error("the link inside the cell lost its ref, so the table can be read and not used")
	}
}

// A row whose only editable cell has no label, which is what an ordinary application's table looks
// like: the item names the row and the box holds a number.
func unnamedCell() []axNode {
	return []axNode{
		{NodeID: "r", Role: axValue{Value: "row"}, ChildIDs: []string{"c1", "c2", "c3"}, BackendDOMNodeID: 1},
		{NodeID: "c1", Role: axValue{Value: "cell"}, ChildIDs: []string{"t1"}, BackendDOMNodeID: 2},
		{NodeID: "t1", Role: axValue{Value: "StaticText"}, Name: axValue{Value: "Cabo HDMI"}, BackendDOMNodeID: 3},
		{NodeID: "c2", Role: axValue{Value: "cell"}, ChildIDs: []string{"in"}, BackendDOMNodeID: 4},
		{NodeID: "in", Role: axValue{Value: "textbox"}, Value: axValue{Value: "7"}, BackendDOMNodeID: 5},
		{NodeID: "c3", Role: axValue{Value: "cell"}, ChildIDs: []string{"b"}, BackendDOMNodeID: 6},
		{NodeID: "b", Role: axValue{Value: "button"}, Name: axValue{Value: "x"}, BackendDOMNodeID: 7},
	}
}

// TestAnUnnamedBoxStillEarnsARef.
//
// The rule used to be "no accessible name, no ref", and its comment argued the case of an unnamed
// BUTTON: nothing to say about what pressing it does, so offering it invites a guess. True there,
// and applied to a class it does not fit. A quantity field in a table row is not ambiguous — the row
// says what it is — and dropping it left the agent with no way to type in it and no way to report
// that a field was there.
//
// Both directions asserted, because the fix is a line and the wrong version of it lets everything
// through: the unnamed box comes back, the unnamed button still does not.
func TestAnUnnamedBoxStillEarnsARef(t *testing.T) {
	nodes := append(unnamedCell(), axNode{
		NodeID: "ghost", Role: axValue{Value: "button"}, BackendDOMNodeID: 8,
	})

	collected, _ := collectParts(oneDocument(nodes), browser.SnapshotRequest{})
	elements, _ := (&Driver{}).name(newTestSession(), collected, false)

	var boxes, buttons int
	for _, one := range elements {
		if one.Ref == "" {
			continue
		}
		switch one.Role {
		case "textbox":
			boxes++
			if one.Value != "7" {
				t.Fatalf("the box came back without what is in it: %+v", one)
			}
		case "button":
			buttons++
			if one.Name == "" {
				t.Fatal("an unnamed button earned a ref; there is nothing to say about what " +
					"pressing it would do, which is the whole reason the name rule exists")
			}
		}
	}
	if boxes != 1 {
		t.Fatalf("the unnamed box earned no ref, so there is no way to type in it: %+v", elements)
	}
	if buttons != 1 {
		t.Fatalf("buttons = %d, want the one named x: %+v", buttons, elements)
	}
}

// TestARowSaysWhatIsInItsUnnamedBox.
//
// The other half of the same change, and without it the fix trades one hole for another. rowLine's
// own comment says a row with a gap where a control was is worse than a word repeated — and letting
// unnamed controls through reintroduced exactly that gap, because a control contributes its NAME to
// the row and these have none. They contribute what they HOLD instead.
func TestARowSaysWhatIsInItsUnnamedBox(t *testing.T) {
	collected, _ := collectParts(oneDocument(unnamedCell()), browser.SnapshotRequest{})

	var row string
	for _, one := range collected {
		if one.element.Role == "row" {
			row = one.element.Name
		}
	}

	if row != "Cabo HDMI | 7 | x" {
		t.Fatalf("row = %q, want the box's contents where the box is; a row that reads "+
			"\"Cabo HDMI |  | x\" hides the field it is about", row)
	}
}

// TestSnapshotLeavesOutThePanel. The panel is the person's, and the agent never reads it: its host
// element and everything under it are dropped from the walk by backend node id, so no ref is ever
// minted for a panel node and nothing the panel says can reach the agent as page content. The page's
// own controls are the control, so the omission is shown to be about the panel and not about an
// empty reading.
func TestSnapshotLeavesOutThePanel(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID

	const hostBackend, insideBackend = 9900, 9901
	node := func(nodeID, role, name string, backend int64, children ...string) map[string]any {
		return map[string]any{
			"nodeId":           nodeID,
			"ignored":          false,
			"role":             map[string]any{"type": "role", "value": role},
			"name":             map[string]any{"type": "computedString", "value": name},
			"childIds":         children,
			"backendDOMNodeId": backend,
		}
	}
	fake.Handle("Accessibility.getFullAXTree", func(cdptest.Call) (any, error) {
		return map[string]any{"nodes": []any{
			node("n1", "RootWebArea", "Page", 1, "n2", "n3", "n5"),
			node("n2", "button", "Page button", 11),
			node("n3", "generic", "Panel host", hostBackend, "n4"),
			node("n4", "button", "Panel button", insideBackend),
			node("n5", "link", "Page link", 12),
		}}, nil
	})
	fake.Handle("DOM.getDocument", func(cdptest.Call) (any, error) {
		return map[string]any{"root": map[string]any{
			"nodeId": 1, "backendNodeId": 1, "nodeName": "#document",
			"children": []any{map[string]any{
				"nodeId": 2, "backendNodeId": 2, "nodeName": "HTML",
				"children": []any{
					map[string]any{"nodeId": 3, "backendNodeId": 3, "nodeName": "BODY"},
					map[string]any{"nodeId": 4, "backendNodeId": hostBackend, "nodeName": "NUCLEOS-PANEL"},
				},
			}},
		}}, nil
	})

	snapshot := snapshotOf(t, driver, id)

	names := map[string]bool{}
	for _, element := range snapshot.Elements {
		names[element.Name] = true
		if strings.Contains(element.Name, "Panel") {
			t.Errorf("the panel was read into the snapshot: %+v", element)
		}
	}
	if !names["Page button"] || !names["Page link"] {
		t.Fatalf("the page's own controls are missing, so the omission proves nothing: %+v", snapshot.Elements)
	}
	driver.mu.Lock()
	defer driver.mu.Unlock()
	for ref, key := range driver.sessions[id].refs {
		if key.backend == hostBackend || key.backend == insideBackend {
			t.Errorf("ref %s was minted for panel node %d", ref, key.backend)
		}
	}
}
