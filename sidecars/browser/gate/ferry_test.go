//go:build browsergate

package gate_test

import (
	"context"
	"net/url"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// TestAFerriedRequestCarriesTheProfilesIdentity.
//
// The claim the ferry lives or dies on. A page's own API call is authenticated by the cookies the
// browser would have sent, and the whole reason this pillar exists is to reach what is behind a
// login — so a ferry that fetched anonymously would work on the open web and fail on exactly the
// pages it was built for, returning a login page the agent would read as the answer.
//
// The request is made by the sidecar, not by the browser (see chrome/ferry.go for the three measured
// reasons the browser cannot make it), so the cookies are fetched from the profile through CDP and
// attached by hand. This is what proves that hand is steady: /whoami answers with the cookie it was
// given, or with "none", and the two are one word apart.
func TestAFerriedRequestCarriesTheProfilesIdentity(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	// A cookie lands in the profile the ordinary way: a page sets it.
	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/set-cookie"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	// Then a page in that same profile fetches something that reports who it was.
	result, err := driver.Act(ctx, session.ID, browser.Action{
		Kind: browser.ActionGoto,
		Text: site.origin() + "/spa?src=" + url.QueryEscape("/whoami"),
	})
	if err != nil {
		t.Fatalf("goto: %v", err)
	}
	if result.Outcome != browser.OutcomeDone {
		t.Fatalf("the dashboard was refused: %+v", result.Refusal)
	}

	var last browser.Snapshot
	deadline := time.Now().Add(20 * time.Second)
	for {
		last, err = driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
		if err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		if hasName(last, "1") || time.Now().After(deadline) {
			break
		}
		time.Sleep(300 * time.Millisecond)
	}

	if hasName(last, "none") {
		t.Fatal("the ferry fetched anonymously; every page behind a login would answer with the login page")
	}
	if !hasName(last, "1") {
		t.Fatalf("the answer never arrived at all: %+v", last.Elements)
	}
}
