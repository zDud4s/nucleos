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
		last, err = driver.Snapshot(ctx, session.ID, false)
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
