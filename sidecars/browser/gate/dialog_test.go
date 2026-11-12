//go:build browsergate

package gate_test

import (
	"context"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestAPageThatAsksAQuestionDoesNotFreezeTheSession.
//
// A page calls confirm(). Chromium hands the dialog to the attached CDP client and BLOCKS the
// renderer until somebody answers it — and with Page.enable on, the attached client is this driver.
// Nothing in the module handled Page.javascriptDialogOpening, so the answer never came.
//
// The failure that produces is the worst one available. Not a wrong reading, which the agent could
// doubt, and not a refusal, which it could act on: the act never returns, the snapshot after it never
// returns, and the session is gone for the rest of its life. An alert() on a cookie notice, a
// confirm() on a delete button, a beforeunload on a half-filled form — none of them is exotic.
//
// The context is deliberately short. A freeze here would otherwise hold the whole gate group until
// its own deadline, and the thing being measured is a hang: it fails faster than it passes.
func TestAPageThatAsksAQuestionDoesNotFreezeTheSession(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/dialog"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	button := findRef(t, snapshot, "Delete")

	began := time.Now()
	if _, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: button}); err != nil {
		t.Fatalf("the act never came back from a page that opened a dialog (%s): %v", time.Since(began), err)
	}

	// The session has to survive it, which is the half a lucky timing could hide: an act that
	// returned because its own deadline passed leaves a renderer still waiting on the dialog, and
	// this is where that shows.
	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("the session was frozen by the dialog: %v", err)
	}

	// And the answer that reached the page was NO. The button says "Delete everything?", and a
	// driver that answers questions on a person's behalf must answer the only way that cannot
	// destroy something: accepting is a decision, dismissing is declining to make one.
	if after.Title != "dismissed" {
		t.Errorf("the page was told %q; a dialog answered any way but no is the agent confirming"+
			" something nobody asked it to confirm", after.Title)
	}
}
