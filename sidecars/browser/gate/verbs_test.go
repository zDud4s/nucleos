//go:build browsergate

package gate_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/chrome"
)

// The verb group. It measures the three things a unit test against a fake cannot: that a synthesised
// key is a key as far as the page is concerned, that setting a dropdown's value is a choice as far
// as the page is concerned, and that history exists.

// refFor finds the ref of a control by the name a person would call it.
func refFor(t *testing.T, snapshot browser.Snapshot, name string) string {
	t.Helper()
	for _, element := range snapshot.Elements {
		if element.Name == name && element.Ref != "" {
			return element.Ref
		}
	}
	t.Fatalf("no control called %q in the snapshot; it had: %+v", name, snapshot.Elements)
	return ""
}

func onControls(t *testing.T) (*site, *chrome.Driver, browser.SessionID) {
	t.Helper()
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/controls"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Refusal != nil {
		t.Fatalf("the page itself was refused: %+v", session.Refusal)
	}
	return site, driver, session.ID
}

// TestAKeyReachesThePageAndTypingDoesNot.
//
// The whole reason `press` exists. Input.insertText puts characters in the box without any key
// event, so a search that submits on Enter could not be submitted and a field that watches
// keystrokes saw none — and the agent's only evidence was a page that did not change.
func TestAKeyReachesThePageAndTypingDoesNot(t *testing.T) {
	site, driver, id := onControls(t)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	snapshot, err := driver.Snapshot(ctx, id, false)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	box := refFor(t, snapshot, "Search")

	if _, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionType, Ref: box, Text: "lisbon"}); err != nil {
		t.Fatalf("type: %v", err)
	}
	if site.reached("BEACON keydown-l", 2*time.Second) {
		t.Error("typing produced a key event after all; the comment on typeInto is wrong and press may be unnecessary")
	}

	if _, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionPress, Ref: box, Text: "Enter"}); err != nil {
		t.Fatalf("press: %v", err)
	}
	if !site.reached("BEACON keydown-Enter", 10*time.Second) {
		t.Error("the page never heard the key; a form that submits on Enter cannot be submitted")
	}
}

// TestChoosingAnOptionIsAChoiceAsFarAsThePageIsConcerned.
//
// Setting `value` without firing input and change is the version of this that does nothing visible
// and reports success, which is worse than not having the verb.
func TestChoosingAnOptionIsAChoiceAsFarAsThePageIsConcerned(t *testing.T) {
	site, driver, id := onControls(t)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	snapshot, err := driver.Snapshot(ctx, id, false)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	dropdown := refFor(t, snapshot, "Where")

	result, err := driver.Act(ctx, id, browser.Action{
		Kind: browser.ActionSelect, Ref: dropdown, Text: "Spain",
	})
	if err != nil {
		t.Fatalf("select: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("choosing an option that is there was refused: %+v", result.Refusal)
	}
	if !site.reached("BEACON chose-es", 10*time.Second) {
		t.Error("the page was never told the choice changed")
	}
}

// TestScrollingThePageMovesIt.
func TestScrollingThePageMovesIt(t *testing.T) {
	site, driver, id := onControls(t)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	if _, err := driver.Act(ctx, id, browser.Action{Kind: browser.ActionScroll}); err != nil {
		t.Fatalf("scroll: %v", err)
	}
	if !site.reached("BEACON scrolled", 10*time.Second) {
		t.Error("the page did not move; content that only exists after scrolling is unreachable")
	}
}

// TestGoingBackReturnsToThePageBefore.
//
// An agent that follows the wrong link had no way back: re-opening the url it came from needs the
// url, which it may not have kept, and pays the admission check again.
func TestGoingBackReturnsToThePageBefore(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	start := site.origin() + "/link?href=" + site.origin() + "/reading"
	session, err := driver.Open(ctx, browser.OpenRequest{URL: start})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	snapshot, err := driver.Snapshot(ctx, session.ID, false)
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	forward, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick, Ref: refFor(t, snapshot, "Go"),
	})
	if err != nil {
		t.Fatalf("click: %v", err)
	}
	if !forward.Navigated || !strings.Contains(forward.URL, "/reading") {
		t.Fatalf("the click did not report where it went: %+v", forward)
	}

	back, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionBack})
	if err != nil {
		t.Fatalf("back: %v", err)
	}
	if back.Outcome != browser.OutcomeDone {
		t.Fatalf("going back was refused: %+v", back.Refusal)
	}
	if !back.Navigated || !strings.Contains(back.URL, "/link") {
		t.Errorf("back did not return to the page before: %+v", back)
	}
}
