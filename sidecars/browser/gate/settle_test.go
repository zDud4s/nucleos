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
// ONE snapshot, taken immediately, with no polling, AND a slow endpoint. Both are needed. Polling
// hides the race — retry for a second and the answer turns up. And an act already spends up to a
// second and a half waiting for a fence refusal, so a page that answers inside that window is
// covered whether this wait works or not: the first version of this test fetched an instant
// endpoint and would have passed with the wait deleted.
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

	began := time.Now()
	result, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  refFor(t, before, "Load the report"),
	})
	if err != nil {
		t.Fatalf("click: %v", err)
	}
	if spent := time.Since(began); spent < 2*time.Second {
		t.Fatalf("the click returned after %v, which is inside the refusal window; it did not wait for the fetch", spent)
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
// This one CANNOT be made to discriminate, and saying so is better than implying otherwise. A pure
// redraw is bounded by movingBound at a second and a half, which is also what an act already spends
// waiting for a fence refusal — so any redraw this wait covers, that window covered too. What it
// asserts is that the end-to-end behaviour is right; the evidence that awaitSettled is what makes it
// right is in chrome/settle_test.go, which calls it directly.
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

// TestAScrollThatLoadsMoreIsWaitedFor.
//
// Scroll was left out of the wait because "it moves the viewport and runs no handler", which is false
// on any endless list: scrolling is THE gesture that loads more.
//
// The claim has to be made against a load that is SLOWER than the window an act already spends
// waiting for a fence refusal — one and a half seconds — or the test passes on that window and
// measures nothing. /slow-rows takes two and a half, so a wait that does not drain what the ferry is
// carrying returns first and the reading shows the list exactly as it was.
func TestAScrollThatLoadsMoreIsWaitedFor(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/endless"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	before, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if hasName(before, "Revenue fell") {
		t.Fatal("the list was already full; this proves nothing")
	}

	began := time.Now()
	result, err := driver.Act(ctx, session.ID, browser.Action{Kind: browser.ActionScroll, Text: "down"})
	if err != nil {
		t.Fatalf("scroll: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("the scroll was refused: %+v", result.Refusal)
	}
	if spent := time.Since(began); spent < 2*time.Second {
		t.Fatalf("the scroll returned after %v, which is inside the refusal window; it did not wait for the load", spent)
	}

	after, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if !hasName(after, "Revenue fell") {
		t.Fatalf("the scroll returned before what it loaded arrived: %+v", after.Elements)
	}
}
