package chrome

import (
	"context"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
	"nucleosbrowser/fence"
)

// refusedBy is the sentence Chromium writes to its own log when our CSP stops something.
func refusedBy(what, url, directive string) map[string]any {
	return map[string]any{"entry": map[string]any{
		"source": "security",
		"level":  "error",
		"text": "Refused to " + what + " '" + url + "' because it violates the following " +
			"Content Security Policy directive: \"" + directive + "\".",
	}}
}

// formActionDirective is the form-action clause exactly as the fence sends it.
//
// Read from fence.Directives rather than written out, because `violation` matches the whole
// `directive value` pair against what the fence actually ships: a literal here would keep passing
// after someone changed the policy, asserting a route for a sentence Chromium would never print.
func formActionDirective() string {
	for _, one := range strings.Split(fence.Directives, ";") {
		if trimmed := strings.TrimSpace(one); strings.HasPrefix(trimmed, "form-action") {
			return trimmed
		}
	}
	return "form-action"
}

func snapshotOf(t *testing.T, driver *Driver, id browser.SessionID) browser.Snapshot {
	t.Helper()
	snapshot, err := driver.Snapshot(context.Background(), id, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	return snapshot
}

// TestAPageThatCouldNotFetchItsContentSaysSoOnTheReading.
//
// The whole reason this file exists. `connect-src 'none'` is enforced in the renderer, so no request
// is ever made, so the interception never pauses one and the fence has nothing to report. A page
// that arrives empty and fills itself from an API renders a shell — and a shell is a CORRECT reading
// of an empty page, which is why nothing anywhere used to contradict it.
func TestAPageThatCouldNotFetchItsContentSaysSoOnTheReading(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)

	fake.Emit("S1", "Log.entryAdded",
		refusedBy("connect to", "https://api.example.org/items", "connect-src 'none'"))

	snapshot := snapshotOf(t, driver, session.ID)
	if snapshot.Blocked == nil {
		t.Fatal("the page could not fetch its own content and the reading did not say so")
	}
	if snapshot.Blocked.Count != 1 {
		t.Errorf("counted %d", snapshot.Blocked.Count)
	}
	if snapshot.Blocked.Consequence != browser.ConsequencePageRequest {
		t.Errorf("named %q", snapshot.Blocked.Consequence)
	}
	if !strings.Contains(snapshot.Blocked.Detail, "api.example.org") {
		t.Errorf("the detail does not say what the page was reaching for: %q", snapshot.Blocked.Detail)
	}
}

// TestAPageLeftAloneSaysNothing.
//
// The field has to be absent on the ordinary page, or its presence stops meaning anything and the
// agent learns to skip it.
func TestAPageLeftAloneSaysNothing(t *testing.T) {
	_, driver := connected(t)
	session := opened(t, driver)

	if blocked := snapshotOf(t, driver, session.ID).Blocked; blocked != nil {
		t.Errorf("a page that tried nothing was reported as blocked: %+v", blocked)
	}
}

// TestAPolicyThisFenceDidNotSendIsNotAttributedToIt.
//
// A site may ship a CSP of its own, and what that stops is the site's business. Reporting it here
// would tell the agent that this machine refused something when what happened is that the page
// refused itself — and the agent's next move differs: one is "ask a person", the other is "this
// page is like this".
func TestAPolicyThisFenceDidNotSendIsNotAttributedToIt(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)

	fake.Emit("S1", "Log.entryAdded",
		refusedBy("load the script", "https://cdn.example.org/x.js", "script-src 'self'"))
	fake.Emit("S1", "Log.entryAdded",
		refusedBy("connect to", "https://api.example.org/items", "connect-src 'self'"))

	if blocked := snapshotOf(t, driver, session.ID).Blocked; blocked != nil {
		t.Errorf("a policy this fence never sent was charged to it: %+v", blocked)
	}
}

// TestABlockedWebSocketKeepsItsOwnName.
//
// `ws:` is refused by the proxy under this name and `wss:` is invisible to both other layers, so it
// reaches the agent only through here. Two names for one channel depending on which layer caught it
// would be the vocabulary lying about the machine.
func TestABlockedWebSocketKeepsItsOwnName(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)

	fake.Emit("S1", "Log.entryAdded",
		refusedBy("connect to", "wss://live.example.org/feed", "connect-src 'none'"))

	blocked := snapshotOf(t, driver, session.ID).Blocked
	if blocked == nil {
		t.Fatal("a blocked socket was not reported at all")
	}
	if blocked.Consequence != browser.ConsequenceChannel {
		t.Errorf("a websocket came back as %q", blocked.Consequence)
	}
}

// TestAFormTheCSPStoppedReachesTheActThatCausedIt.
//
// This is the one that used to come back as "done". A POST was stopped twice — by the method rule
// and by `form-action 'none'` — and the two raced inside Chrome; when the CSP won there was no
// request, so nothing to report. The gate test that should have caught it had its assertion removed,
// with a comment recording exactly this. A form submission is always caused by an act, so it belongs
// on the act and not on the reading.
//
// The directive is no longer 'none' (fence/csp.go), so the race that produced the bug is gone and
// what remains here is the ROUTE: a form-action violation lands on the act. It fires for a form
// aimed somewhere off the network now — blob:, in practice — which is rare and is exactly why the
// path needs a test rather than a witness. The value is taken from the real one so that changing
// the directive cannot leave this asserting a string Chromium will never print.
func TestAFormTheCSPStoppedReachesTheActThatCausedIt(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		fake.Emit("S1", "Log.entryAdded",
			refusedBy("send form data to", "blob:https://example.org/9f2c", formActionDirective()))
		// reachable, not an empty result: the click asks the page where the element is before it
		// presses, and a page that answers nothing fails the act before this test's subject runs.
		return reachable(), nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatal("a submission the fence stopped came back as done")
	}
	if result.Refusal.Consequence != browser.ConsequenceForm {
		t.Errorf("named %q rather than the consequence §6.2 already gave it", result.Refusal.Consequence)
	}

	// And not on the reading as well: one thing that happened, reported once.
	if blocked := snapshotOf(t, driver, id).Blocked; blocked != nil {
		t.Errorf("the form was also counted against the page: %+v", blocked)
	}
}

// TestWhatTheLastPageBlockedIsNotCountedAgainstThisOne.
//
// The count answers "is what I am reading the whole page". An answer carried over from the previous
// document does not answer that about this one.
func TestWhatTheLastPageBlockedIsNotCountedAgainstThisOne(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)

	fake.Emit("S1", "Log.entryAdded",
		refusedBy("connect to", "https://api.example.org/items", "connect-src 'none'"))
	if snapshotOf(t, driver, id).Blocked == nil {
		t.Fatal("nothing was recorded to begin with")
	}
	// Re-minted, because that snapshot replaced the session's ref table with what it found — and
	// against a fake with no accessibility tree, what it found is nothing.
	driver.sessions[id].refs["e1"] = nodeKey{session: "S1", backend: 42}

	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		fake.Emit("S1", "Page.frameNavigated", map[string]any{"frame": map[string]any{"id": "F1"}})
		fake.Emit("S1", "Page.lifecycleEvent", map[string]any{"name": "networkAlmostIdle"})
		return reachable(), nil
	})
	if result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"}); !result.Navigated {
		t.Fatal("the act did not navigate, so this proves nothing")
	}

	if blocked := snapshotOf(t, driver, id).Blocked; blocked != nil {
		t.Errorf("the previous document's count followed the session to a new page: %+v", blocked)
	}
}
