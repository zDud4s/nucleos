//go:build browsergate

package gate_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestAPageThatFailedSaysSoRatherThanReadingAsAPage.
//
// The quietest hole this contract had. A 404 is a page — heading, sentence, search box — and every
// signal the reading carries says it is fine: nothing blocked, nothing truncated, nothing still
// loading. An agent sent to find something reads it correctly and concludes the thing is not there,
// when what happened is that the request failed.
//
// So the test asserts both halves, and the second is the one that gives the first its meaning: the
// status says 404 AND the reading looks like an ordinary page. If the reading alone were enough,
// carrying the status would be decoration.
func TestAPageThatFailedSaysSoRatherThanReadingAsAPage(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/missing"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Status != 404 {
		t.Fatalf("opening a page that 404s reported status %d", session.Status)
	}

	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if snapshot.Status != 404 {
		t.Errorf("the reading reported status %d; a click or a goto replaces the document without"+
			" producing a new session, so the reading is the only place this can arrive", snapshot.Status)
	}

	// The half that makes the point. Everything else about this reading says a page arrived.
	if snapshot.Blocked != nil || snapshot.Truncated || snapshot.StillLoading {
		t.Fatal("this 404 announced itself some other way, so it is not the case the status is for")
	}
	if len(snapshot.Elements) == 0 {
		t.Fatal("the 404 read as empty, which the agent could already doubt; the trap is the one that reads FINE")
	}
}

// TestAPageThatArrivedSaysTwoHundred, so that a status of zero keeps meaning "nothing said" rather
// than being what every page reports. Without this, the field above would pass its test while being
// blank everywhere, and an agent would learn to ignore it.
func TestAPageThatArrivedSaysTwoHundred(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/reading"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Status != 200 {
		t.Fatalf("an ordinary page reported status %d", session.Status)
	}
}

// TestAFrameThatFailedIsNotThePagesFailure.
//
// The status is the PAGE's. An advertisement, a widget or a tracker that 404s inside an iframe says
// nothing about whether the article loaded, and reporting a frame's failure as the page's would be
// worse than reporting nothing at all: the agent would abandon a page that is perfectly fine, and
// this time the reading would agree with it.
//
// The frame is on the same origin, so the fence admits it and it genuinely 404s — a frame refused
// for its origin would never reach the response stage and the test would pass without measuring
// anything.
func TestAFrameThatFailedIsNotThePagesFailure(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: site.origin() + "/framing?src=" + site.origin() + "/missing",
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if !site.reached("GET /missing", settle) {
		t.Fatal("the frame was never fetched, so nothing here is being measured")
	}
	if session.Status != 200 {
		t.Errorf("the page reported status %d; the 404 belongs to the frame inside it", session.Status)
	}

	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if snapshot.Status != 200 {
		t.Errorf("the reading reported status %d for a page that arrived", snapshot.Status)
	}
	// And the frame's contents are read, which is what says the 404 really did land inside it.
	var read strings.Builder
	for _, element := range snapshot.Elements {
		read.WriteString(element.Name)
		read.WriteString("\n")
	}
	if !strings.Contains(read.String(), "We could not find that") {
		t.Logf("the frame's own words are not in the reading; the page's status is still the claim here")
	}
}
