//go:build browsergate

package gate_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// says reports whether anything in the reading is called this.
func says(snapshot browser.Snapshot, name string) bool {
	for _, element := range snapshot.Elements {
		if strings.Contains(element.Name, name) {
			return true
		}
	}
	return false
}

// TestAMenuThatOpensOnPointerDownOpens.
//
// The click was `this.click()`: one synthetic event on the element, and no mousemove, mouseover,
// pointerdown, mousedown or mouseup anywhere. A great many real components — menus, dropdowns,
// popovers — open on pointerdown rather than on click, because that is what makes them feel
// immediate, and under the old verb not one of them opened.
//
// What made it the worst kind of failure is what happened next. The act reported DONE. The reading
// afterwards was entirely CORRECT, and showed the menu closed. Nothing anywhere said a problem had
// occurred, so the agent concluded the button does not work — and clicked it again.
//
// Measured against the pinned build, because whether Chromium synthesises pointer events from
// Input.dispatchMouseEvent is a fact about Chromium and not something this repository can assert.
func TestAMenuThatOpensOnPointerDownOpens(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/pointer"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if says(snapshot, "Archive") {
		t.Fatal("the menu was open before anything clicked it; this page proves nothing")
	}

	result, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  findRef(t, snapshot, "Actions"),
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome == browser.OutcomeRefused {
		t.Fatalf("the click was refused as %q: %s", result.Refusal.Consequence, result.Refusal.Detail)
	}

	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot after: %v", err)
	}
	if !says(after, "Archive") {
		t.Fatalf("the menu did not open, and the act said it was done. This is the failure the whole"+
			" verb was rewritten for: %+v", after.Elements)
	}
}

// TestAMenuThatOpensOnHoverIsOpenByTheTimeItIsClicked.
//
// There is no hover verb and this is why one is not needed. Moving the pointer onto the element is
// part of clicking it, so mouseover and mouseenter fire before the press, and a menu that opens on
// hover is open by the time the next reading is taken — its items carrying refs the agent can use.
//
// A separate verb would be the obvious way to reach this, and would be a fifth thing for the agent
// to know about when what it wants is simply to press what it can see.
func TestAMenuThatOpensOnHoverIsOpenByTheTimeItIsClicked(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/hover"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if says(snapshot, "Export") {
		t.Fatal("the submenu was already open; this page proves nothing")
	}

	if _, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  findRef(t, snapshot, "File"),
	}); err != nil {
		t.Fatalf("act: %v", err)
	}

	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot after: %v", err)
	}
	if !says(after, "Export") {
		t.Fatalf("the pointer never arrived on the parent, so a hover menu is unreachable: %+v", after.Elements)
	}
}

// TestABannerOverAButtonIsSaidRatherThanClickedThrough.
//
// The refusal the verb could not previously make. element.click() fires on the element whatever is
// painted over it, so a button under a consent banner was clicked, reported done, and — because the
// page's own handler is bound to the banner's click, not the button's — did nothing that mattered.
// The agent read a page where nothing changed and pressed it again.
//
// The accessibility tree carries the button either way: an overlay is a painting decision and the
// tree is not about painting. So the reading shows a perfectly ordinary button, and the only place
// this can be caught is at the moment of aiming.
//
// The message names WHAT is in the way, because that is the whole of its worth: an agent told a
// banner is on top can dismiss the banner.
func TestABannerOverAButtonIsSaidRatherThanClickedThrough(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/covered"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	result, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  findRef(t, snapshot, "Save"),
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("a button under a full-page banner was reported as clicked: %+v", result)
	}
	if !strings.Contains(result.Refusal.Detail, "cookies") {
		t.Errorf("the refusal does not name what is in the way, which is the only part the agent can"+
			" act on: %q", result.Refusal.Detail)
	}
}
