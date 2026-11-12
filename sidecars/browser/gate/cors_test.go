//go:build browsergate

package gate_test

import (
	"context"
	"net/url"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// dashboard is a page on one host whose content lives on another, which is how a large part of the
// web is built and was, until this, a page the agent read as empty.
func dashboard(t *testing.T, app, api *site, path string) (*browser.Snapshot, browser.Session) {
	t.Helper()
	driver, _ := fenced(t, admittingBoth(app, api))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	t.Cleanup(cancel)

	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: app.origin() + "/spa?src=" + url.QueryEscape(api.origin()+path),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	return &snapshot, session
}

// TestAServiceThatOptsInIsRead.
//
// The rule the ferry stopped being same-origin-only for. `app.example.com` rendering itself from
// `api.example.com` is not an edge case, and refusing it left every such page blank with the fence
// reporting nothing an agent could act on.
//
// What replaced same-origin is not a wider guess, it is the BROWSER'S rule: the profile must admit
// the other host at all, and the response must carry the Access-Control-Allow-Origin an unfenced
// Chromium would have demanded before letting the page read it. This grants nothing a browser
// without any of this would refuse.
func TestAServiceThatOptsInIsRead(t *testing.T) {
	app, api := newSite(t), newSite(t)
	snapshot, _ := dashboard(t, app, api, "/cors?allow="+url.QueryEscape(app.origin()))

	if !hasName(*snapshot, "The neighbouring service answered") {
		t.Fatalf("the service named this page and it was not read: %+v", snapshot.Elements)
	}
}

// TestAServiceThatSaysNothingIsNotRead.
//
// The other half, and the one that keeps this from being a hole. A server that has not opted in is
// not readable by a page from somewhere else — that is what CORS says, and a ferry that ignored it
// would be handing pages data no browser would give them.
//
// The refusal has to be VISIBLE. A cross-origin read that failed silently is the shell problem
// again: the agent sees a page with nothing on it and no reason to doubt it.
func TestAServiceThatSaysNothingIsNotRead(t *testing.T) {
	app, api := newSite(t), newSite(t)
	snapshot, _ := dashboard(t, app, api, "/cors")

	if hasName(*snapshot, "The neighbouring service answered") {
		t.Fatal("a service that opted nobody in was read anyway")
	}
	if snapshot.Blocked == nil {
		t.Fatal("the page could not get its content and the reading did not say so")
	}
	// The request DID arrive — a browser makes it and then refuses to hand the answer to the page,
	// which is exactly what CORS is. Asserting this is what separates "the CORS check refused it"
	// from "the allowlist refused it", and the two would otherwise look identical from the snapshot.
	select {
	case <-api.arrived:
	case <-time.After(2 * time.Second):
		t.Fatal("the request never reached the service, so this proves nothing about CORS")
	}
}

// TestAServiceTheProfileDoesNotAdmitIsNotReached.
//
// The floor under the CORS rule. CORS is the server's answer to "may this page read me", and it is
// the server's to give — so on its own it would let a page pull from anywhere that says yes. The
// profile's own list decides which hosts are in play at all, and it answers first.
func TestAServiceTheProfileDoesNotAdmitIsNotReached(t *testing.T) {
	app, api, stranger := newSite(t), newSite(t), newSite(t)
	driver, _ := fenced(t, admittingBoth(app, api))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: app.origin() + "/spa?src=" +
			url.QueryEscape(stranger.origin()+"/cors?allow="+url.QueryEscape(app.origin())),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	if hasName(snapshot, "The neighbouring service answered") {
		t.Fatal("a host this profile does not admit was read from")
	}
	// The request must not have been MADE either. A refusal after the fact is a request that left
	// the machine, and for a host the profile never admitted that is the whole of the damage.
	select {
	case what := <-stranger.arrived:
		t.Fatalf("the request reached a host this profile does not admit: %s", what)
	case <-time.After(time.Second):
	}
}
