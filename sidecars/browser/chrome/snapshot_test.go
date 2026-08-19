package chrome

import (
	"strings"
	"testing"

	"nucleosbrowser/browser"
)

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

	elements, refs, truncated := collect(nodes)

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

	elements, _, _ := collect(nodes)

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

	elements, _, _ := collect(nodes)

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

	elements, _, truncated := collect(nodes)

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

	elements, _, _ := collect(nodes)

	if len(elements) != 1 || elements[0].Role != "link" {
		t.Fatalf("the nested text was emitted beside its own link: %+v", elements)
	}
}

var _ = browser.Element{}
