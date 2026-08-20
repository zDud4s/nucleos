//go:build browsergate

package gate_test

import (
	"context"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestAClickThatFetchesIsFinishedBeforeTheActReturns.
//
// The whole ferry exists for pages that render themselves from an API, and on such a page the
// ordinary interaction is a CLICK — which does not navigate. So the act returned as soon as the CDP
// call came back, which is before the fetch it started had been made, let alone answered, and the
// agent's next reading was of the page as it stood before it pressed anything. Open was covered,
// goto and back were covered, and the case the feature was built for was not.
//
// ONE snapshot, taken immediately, with no polling. Polling is exactly what hides this: retry for a
// second and the answer turns up, and the test then proves that the content eventually appears
// rather than that the act waited for it.
func TestAClickThatFetchesIsFinishedBeforeTheActReturns(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/click-spa"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	before, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if hasName(before, "Revenue fell") {
		t.Fatal("the page had the content before anything was pressed; this proves nothing")
	}

	result, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  refFor(t, before, "Load the report"),
	})
	if err != nil {
		t.Fatalf("click: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("the click was refused: %+v", result.Refusal)
	}
	if result.Navigated {
		t.Fatal("this page does not navigate; the wait being measured is the other one")
	}

	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if !hasName(after, "Revenue fell") {
		t.Fatalf("the act returned before what it started arrived: %+v", after.Elements)
	}
}

// TestAClickThatStartsNothingCostsOnlyTheReactionWindow.
//
// The other half of the same rule, and the one that keeps the fix from being a tax. A click that
// asks for nothing DOES pay the reaction window — there is no way to know it started nothing except
// by giving it a moment to start something — so what is asserted here is the bound, not its absence.
//
// The failure it guards is the wait falling through to the load deadline: awaitSettled shares its
// overall bound with Open, and a version that reached for it whenever nothing was in flight would
// make every inert click cost fifteen seconds. Three of them here are a second's worth of windows,
// or three quarters of a minute.
func TestAClickThatStartsNothingCostsOnlyTheReactionWindow(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/click-spa"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	first, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	ref := refFor(t, first, "Do nothing")
	began := time.Now()
	for i := 0; i < 3; i++ {
		if _, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionClick, Ref: ref}); err != nil {
			t.Fatalf("click %d: %v", i, err)
		}
	}
	// MEASURED at about five and a half seconds, which is mostly not this wait: an act already pays
	// the fence's refusal window, and three of those are the bulk of it. The number below sits
	// between the two worlds this can be in — about five seconds when an inert click reacts and
	// returns, about fifty when each one falls through to the load deadline instead.
	if spent := time.Since(began); spent > 12*time.Second {
		t.Fatalf("three clicks that started nothing took %v", spent)
	}
}

// TestAClickThatRedrawsIsFinishedBeforeTheActReturns.
//
// The other half of the same wait, and the half the ferry could never see: a click that asks for
// nothing, navigates nowhere, and simply draws. There is no request to count and no lifecycle event
// to hear, so before the page's own MutationObserver there was no signal at all — the act returned
// on the CDP round trip, and the next reading was of the page as it stood before the click.
//
// ONE snapshot, no polling, for the same reason as its neighbour: polling proves the content turns
// up eventually, which is not the claim.
func TestAClickThatRedrawsIsFinishedBeforeTheActReturns(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/click-render"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	before, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if hasName(before, "Revenue fell") {
		t.Fatal("the page had the detail before anything was pressed; this proves nothing")
	}

	result, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  refFor(t, before, "Show the detail"),
	})
	if err != nil {
		t.Fatalf("click: %v", err)
	}
	if result.Navigated {
		t.Fatal("this page does not navigate; the wait being measured is the other one")
	}

	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if hasName(after, "Loading the detail") {
		t.Fatal("the act returned on the placeholder: the wait stopped at the reaction window")
	}
	if !hasName(after, "Revenue fell") {
		t.Fatalf("the act returned before the page had finished drawing: %+v", after.Elements)
	}
}
