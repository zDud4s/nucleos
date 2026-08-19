package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/cdp/cdptest"
)

func humanOn(t *testing.T, fake *cdptest.Browser, conn *cdp.Conn) *Human {
	t.Helper()
	human, err := ConnectHuman(context.Background(), conn)
	if err != nil {
		t.Fatalf("connect human: %v", err)
	}
	t.Cleanup(human.Detach)
	_ = fake
	return human
}

// Spec §5.3a. The chain is what a login actually looks like: out to the identity provider and BACK,
// with the destination named twice. `grant` reads the last step to tell the destination from the
// IdP, so collapsing this to a set here — which looks like tidying — would file the two backwards on
// the screen where a person revokes one of them.
func TestTheChainKeepsTheOrderAndTheReturn(t *testing.T) {
	fake, conn := dial(t)
	human := humanOn(t, fake, conn)

	for _, url := range []string{
		"https://jira.example.org/login",
		"https://accounts.google.com/o/oauth2/auth",
		"https://accounts.google.com/o/oauth2/auth", // a re-render of the same page
		"https://jira.example.org/browse/X-1",
	} {
		fake.Emit("", "Target.targetInfoChanged", map[string]any{
			"targetInfo": map[string]any{"targetId": "T1", "type": "page", "url": url},
		})
	}
	waitForChain(t, human, 3)

	want := []string{
		"https://jira.example.org/login",
		"https://accounts.google.com/o/oauth2/auth",
		"https://jira.example.org/browse/X-1",
	}
	got := human.Chain()
	if len(got) != len(want) {
		t.Fatalf("chain = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("chain[%d] = %q, want %q", i, got[i], want[i])
		}
	}
}

// A popup counts. An SSO login is very often one, spec §5.4 blocks popups in AGENT mode precisely
// because of what they can do, and a chain recorded from the first tab only would miss the identity
// provider — which is the single host §5.3a exists to capture.
func TestAPopupIsPartOfTheChain(t *testing.T) {
	fake, conn := dial(t)
	human := humanOn(t, fake, conn)

	fake.Emit("", "Target.targetInfoChanged", map[string]any{
		"targetInfo": map[string]any{"targetId": "T1", "type": "page", "url": "https://jira.example.org/login"},
	})
	fake.Emit("", "Target.targetCreated", map[string]any{
		"targetInfo": map[string]any{"targetId": "T2", "type": "page", "url": "https://login.microsoftonline.com/oauth2"},
	})
	waitForChain(t, human, 2)

	if human.Chain()[1] != "https://login.microsoftonline.com/oauth2" {
		t.Fatalf("the popup is missing from the chain: %v", human.Chain())
	}
}

// What the browser does on its way out is not somewhere a person chose to go. Without this the tabs
// Chrome touches during shutdown arrive at `grant` looking exactly like a login step.
func TestNothingAfterTheWindowClosesReachesTheChain(t *testing.T) {
	fake, conn := dial(t)
	human := humanOn(t, fake, conn)
	fake.Handle("Target.createTarget", func(cdptest.Call) (any, error) {
		return map[string]any{"targetId": "T1"}, nil
	})
	fake.Handle("Target.attachToTarget", func(cdptest.Call) (any, error) {
		return map[string]any{"sessionId": "S1"}, nil
	})
	fake.Handle("Target.closeTarget", func(cdptest.Call) (any, error) {
		return map[string]any{"success": true}, nil
	})

	session, err := human.Open(context.Background(), browser.OpenRequest{URL: "https://jira.example.org/login"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	waitForChain(t, human, 1)

	if err := human.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	fake.Emit("", "Target.targetInfoChanged", map[string]any{
		"targetInfo": map[string]any{"targetId": "T9", "type": "page", "url": "https://tracker.example.net/beacon"},
	})
	time.Sleep(50 * time.Millisecond)

	for _, url := range human.Chain() {
		if url == "https://tracker.example.net/beacon" {
			t.Fatalf("a url from after the close reached the chain: %v", human.Chain())
		}
	}
}

// The person's window is theirs. Every agent verb is refused here as well as in the núcleo, and
// Screenshot is the one that matters most: the page a handover exists for is a login form with a
// password half-typed into it.
func TestThePersonsWindowAnswersNothingToTheAgent(t *testing.T) {
	fake, conn := dial(t)
	human := humanOn(t, fake, conn)

	if _, err := human.Snapshot(context.Background(), "h1", browser.SnapshotRequest{}); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("snapshot: %v", err)
	}
	if _, err := human.Act(context.Background(), "h1", browser.Action{Kind: browser.ActionClick}); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("act: %v", err)
	}
	if _, err := human.Screenshot(context.Background(), "h1"); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("screenshot: %v", err)
	}
	if _, err := human.Handoff(context.Background(), "h1", "again"); !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Errorf("handoff: %v", err)
	}
}

// A headful window is unfenced by design (spec §6.4) — so this pins that it never PRETENDS to be
// fenced. Nothing here arms interception, and the absence has to be deliberate and visible: a future
// change that adds Fetch.enable to this path would break the login the handover exists to allow.
func TestTheHumanBrowserArmsNoFence(t *testing.T) {
	fake, conn := dial(t)
	humanOn(t, fake, conn)

	for _, method := range fake.Methods() {
		if method == "Fetch.enable" || method == "Browser.setDownloadBehavior" {
			t.Fatalf("the person's browser was fenced: %v", fake.Methods())
		}
	}
	if fake.IndexOf("Target.setDiscoverTargets") < 0 {
		t.Fatalf("the recorder never started: %v", fake.Methods())
	}
	var params map[string]any
	if err := json.Unmarshal(fake.Calls()[fake.IndexOf("Target.setDiscoverTargets")].Params, &params); err != nil {
		t.Fatal(err)
	}
	if params["discover"] != true {
		t.Fatalf("discovery is off, so no chain would be recorded: %v", params)
	}
}

func waitForChain(t *testing.T, human *Human, want int) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if len(human.Chain()) >= want {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("the chain never reached %d entries: %v", want, human.Chain())
}
