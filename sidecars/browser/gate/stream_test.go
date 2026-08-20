//go:build browsergate

package gate_test

import (
	"context"
	"net/url"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestAStreamIsRefusedAtOnceAndNotAtTheTimeout.
//
// The ferry read the whole body before answering, so a page fetching a stream sat until the outer
// bound — thirty seconds — and only then failed. A refusal that arrives that late is not read as a
// rule; it is read as a network that is broken, and the agent's reasonable next move is to retry it
// and spend another thirty.
//
// What is asserted is the TIME. That the fetch fails is true either way, and true is not the claim.
func TestAStreamIsRefusedAtOnceAndNotAtTheTimeout(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()

	began := time.Now()
	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: site.origin() + "/spa?src=" + url.QueryEscape("/stream"),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	// Open waits for what the ferry is carrying, so this is the refusal's own latency and not a
	// separate measurement of it.
	opened := time.Since(began)

	last, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if last.Blocked == nil {
		t.Fatal("the page could not get its content and the reading did not say so")
	}
	// Comfortably below the thirty-second bound and comfortably above nothing: what is being ruled
	// out is the ferry having read a body that never ends.
	if opened > 15*time.Second {
		t.Fatalf("opening a page whose fetch is a stream took %v; it was refused at the timeout, not at the headers", opened)
	}
}
