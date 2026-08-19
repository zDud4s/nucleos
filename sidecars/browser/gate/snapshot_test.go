//go:build browsergate

package gate_test

import (
	"context"
	"net/url"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// otherHost is the same server under a name Chromium considers a different SITE.
//
// Site isolation partitions by scheme + host, not by port, so a second httptest server on
// 127.0.0.1 would land in the same renderer and prove nothing. `localhost` and `127.0.0.1` resolve
// to the same socket and are different sites, which is exactly the pair this needs: one process
// boundary, no DNS, no second server.
func otherHost(s *site) string {
	return strings.Replace(s.origin(), "127.0.0.1", "localhost", 1)
}

// A login form inside a cross-site frame is the case this pillar exists for.
//
// `Accessibility.getFullAXTree` is asked of ONE target. A same-process iframe is part of that
// target's tree; a cross-site one is a separate process with a separate target, and the driver
// already knows this — it re-arms auto-attach per target precisely because the spike measured that
// a cross-site frame never appears otherwise (chrome/driver.go, onEvent). The snapshot never
// re-crosses that boundary. So the SSO form the agent was sent to fill can be absent from the
// reading with nothing anywhere reporting a problem: an empty frame is a correct reading of a
// target that genuinely contains nothing.
//
// Polled rather than read once, because a snapshot taken the instant Open returns is racing the
// frame's own load. That race is a defect in its own right and is fixed elsewhere; measuring
// through it would be measuring the wrong thing.
func TestTheSnapshotReachesInsideACrossSiteFrame(t *testing.T) {
	site := newSite(t)
	policy := admitting(site)
	policy.Loopback = append(policy.Loopback, otherHost(site))
	driver, _ := fenced(t, policy)

	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	framed := otherHost(site) + "/reading"
	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: site.origin() + "/framing?src=" + url.QueryEscape(framed),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.Refusal != nil {
		t.Fatalf("the framing page itself was refused: %+v", session.Refusal)
	}

	var last browser.Snapshot
	deadline := time.Now().Add(30 * time.Second)
	for {
		last, err = driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
		if err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		if inFrame(last) || time.Now().After(deadline) {
			break
		}
		time.Sleep(500 * time.Millisecond)
	}

	if !inFrame(last) {
		t.Errorf("nothing inside the cross-site frame reached the snapshot.\n"+
			"the agent reads this page as empty and concludes the form is not there.\ngot: %+v",
			last.Elements)
	}
}

// inFrame reports whether anything that only exists inside the framed document came back.
func inFrame(snapshot browser.Snapshot) bool {
	for _, element := range snapshot.Elements {
		if element.Name == "Email" && element.Role == "textbox" {
			return true
		}
		if element.Role == "text" && strings.Contains(element.Name, "Revenue fell by eleven percent") {
			return true
		}
	}
	return false
}

// TestATableKeepsItsShape.
//
// The roles this rests on — row, cell, columnheader — are an assumption about what Chromium calls
// things, and this repository has had three of those turn out wrong against a hand-built tree. The
// grid was being dropped at the door: an agent could read every figure in a table and could not say
// which column any of them was in, which for a table is the whole of the information.
func TestATableKeepsItsShape(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/table"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	snapshot, err := driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}

	var rows []string
	linked := ""
	for _, element := range snapshot.Elements {
		if element.Role == "row" {
			rows = append(rows, element.Name)
		}
		if element.Role == "link" && element.Name == "Q2" {
			linked = element.Ref
		}
	}

	if len(rows) != 3 {
		t.Fatalf("expected three rows, got %d: %+v\nthe whole snapshot was: %+v", len(rows), rows, snapshot.Elements)
	}
	if rows[0] != "Quarter | Revenue" {
		t.Errorf("the headers lost their shape: %q", rows[0])
	}
	if rows[1] != "Q1 | -11%" {
		t.Errorf("a row lost its shape: %q", rows[1])
	}
	if rows[2] != "Q2 | +4%" {
		t.Errorf("a row with a link in it lost its shape: %q", rows[2])
	}
	if linked == "" {
		t.Error("the link inside a cell has no ref, so the table can be read and not used")
	}
}

// TestAPageFetchesItsOwnContentThroughTheFence.
//
// The measurement that produced this, kept because it is the argument for the whole file it led to:
// with `connect-src 'none'` and nothing else, this dashboard rendered a shell. The fetch never left,
// the snapshot carried the heading and the menu and none of the content, and the agent read that as
// a page with nothing on it — a correct reading, and the wrong conclusion, with nothing anywhere to
// contradict it.
//
// The channel is STILL closed. What changed is that the page can ask us, and we decide: the shim
// hands the url to a binding, the driver checks it, and the BROWSER loads it in the profile and
// through the fence's own interception. So `GET /content` arriving here is the point of the
// assertion — it proves the request was really made, in the real cookie jar, rather than answered
// from somewhere of ours.
func TestAPageFetchesItsOwnContentThroughTheFence(t *testing.T) {
	site := newSite(t)
	driver, _ := fenced(t, admitting(site))
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	session, err := driver.Open(ctx, browser.OpenRequest{URL: site.origin() + "/spa"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if !site.reached("GET /content", 20*time.Second) {
		t.Fatal("the request was never made; the page is still a shell")
	}

	var last browser.Snapshot
	deadline := time.Now().Add(20 * time.Second)
	for {
		last, err = driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
		if err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		if hasName(last, "Approve the write-down") || time.Now().After(deadline) {
			break
		}
		time.Sleep(300 * time.Millisecond)
	}

	if !hasName(last, "Approve the write-down") {
		t.Fatalf("the content arrived and the agent still cannot see it: %+v", last.Elements)
	}
	var prose string
	for _, element := range last.Elements {
		if element.Role == "text" {
			prose += element.Name
		}
	}
	if !strings.Contains(prose, "Revenue fell by eleven percent") {
		t.Errorf("the words the page fetched are not in the reading: %q", prose)
	}
	if last.Blocked != nil {
		t.Errorf("a page that was served was also reported as refused: %+v", last.Blocked)
	}
}

// TestAPageCannotHaveTheFenceFetchFromAnotherHost.
//
// The rule that makes the ferry a service and not a hole. Same-origin only: it opens no host the
// page could not already reach, and the answer comes from a server the page already IS. The origin
// is taken from the execution context Chromium reports, never from the page, because a restriction
// the restricted thing describes is not one.
func TestAPageCannotHaveTheFenceFetchFromAnotherHost(t *testing.T) {
	site := newSite(t)
	policy := admitting(site)
	policy.Loopback = append(policy.Loopback, otherHost(site))
	driver, _ := fenced(t, policy)
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	// The same server under a name Chromium calls a different site, and one this profile even
	// admits — so what refuses this is the ferry's own rule and not the allowlist.
	elsewhere := otherHost(site) + "/content"
	session, err := driver.Open(ctx, browser.OpenRequest{
		URL: site.origin() + "/spa?src=" + url.QueryEscape(elsewhere),
	})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	if site.reached("GET /content", 5*time.Second) {
		t.Fatal("the fence carried a request to another host")
	}

	var last browser.Snapshot
	deadline := time.Now().Add(20 * time.Second)
	for {
		last, err = driver.Snapshot(ctx, session.ID, browser.SnapshotRequest{})
		if err != nil {
			t.Fatalf("snapshot: %v", err)
		}
		if last.Blocked != nil || time.Now().After(deadline) {
			break
		}
		time.Sleep(300 * time.Millisecond)
	}
	if last.Blocked == nil {
		t.Fatal("the page could not get its content and the reading did not say so")
	}
	if last.Blocked.Consequence != browser.ConsequencePageRequest {
		t.Errorf("named %q", last.Blocked.Consequence)
	}
}

// hasName reports whether anything in a snapshot is called this.
func hasName(snapshot browser.Snapshot, name string) bool {
	for _, element := range snapshot.Elements {
		if strings.Contains(element.Name, name) {
			return true
		}
	}
	return false
}
