package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"slices"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/cdp/cdptest"
)

func dial(t *testing.T) (*cdptest.Browser, *cdp.Conn) {
	t.Helper()
	fake, err := cdptest.Start()
	if err != nil {
		t.Fatalf("fake browser: %v", err)
	}
	t.Cleanup(fake.Close)
	conn, err := cdp.Dial(fake.URL, 5*time.Second)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return fake, conn
}

// autoAttachOnCreate makes the fake behave like Chrome: creating a target raises an attach event,
// because Connect turned auto-attach on.
func autoAttachOnCreate(fake *cdptest.Browser) {
	fake.Handle("Target.createTarget", func(cdptest.Call) (any, error) {
		go func() {
			time.Sleep(20 * time.Millisecond)
			fake.Emit("", "Target.attachedToTarget", map[string]any{
				"sessionId": "S1",
				"targetInfo": map[string]any{
					"targetId": "T1",
					"type":     "page",
					"url":      "",
				},
				"waitingForDebugger": true,
			})
		}()
		return map[string]any{"targetId": "T1"}, nil
	})
}

// TestConnectAttachesTheFenceBeforeAnythingElse.
//
// This is spec §6.2a as an ordering assertion. Both calls happening is not enough — a driver that
// created a target first and armed afterwards would pass a "did it call Fetch.enable" test while
// leaving a window in which a page runs unfenced.
func TestConnectAttachesTheFenceBeforeAnythingElse(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn); err != nil {
		t.Fatalf("connect: %v", err)
	}

	methods := fake.Methods()
	fetchAt := fake.IndexOf("Fetch.enable")
	autoAt := fake.IndexOf("Target.setAutoAttach")
	if fetchAt < 0 || autoAt < 0 {
		t.Fatalf("the fence was not attached at all: %v", methods)
	}
	if fetchAt > autoAt {
		t.Errorf("interception was armed after auto-attach: %v", methods)
	}
	if slices.Contains(methods, "Target.createTarget") {
		t.Errorf("Connect created a target: %v", methods)
	}
}

// TestTheFenceGoesOnTheBrowserSession is the spike's most consequential finding, asserted. A
// page-session fence never sees a service worker's script fetch.
func TestTheFenceGoesOnTheBrowserSession(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn); err != nil {
		t.Fatalf("connect: %v", err)
	}
	for _, call := range fake.Calls() {
		if call.Method != "Fetch.enable" && call.Method != "Target.setAutoAttach" {
			continue
		}
		if call.Session != string(cdp.BrowserSession) {
			t.Fatalf("%s went to session %q, not the browser session", call.Method, call.Session)
		}
	}
}

// TestAutoAttachPausesNewTargets. Without waitForDebuggerOnStart there is a TOCTOU window in which
// a new target navigates before the interception is on it (spec §5.4).
func TestAutoAttachPausesNewTargets(t *testing.T) {
	fake, conn := dial(t)
	if _, err := Connect(context.Background(), conn); err != nil {
		t.Fatalf("connect: %v", err)
	}
	for _, call := range fake.Calls() {
		if call.Method != "Target.setAutoAttach" {
			continue
		}
		var params map[string]any
		if err := json.Unmarshal(call.Params, &params); err != nil {
			t.Fatalf("params: %v", err)
		}
		if params["waitForDebuggerOnStart"] != true {
			t.Fatalf("new targets are not paused: %v", params)
		}
		if params["flatten"] != true {
			t.Fatalf("flatten is off, so sessions would not be addressable: %v", params)
		}
		return
	}
	t.Fatal("no auto-attach call")
}

// TestAFenceThatWillNotArmRefusesToProduceADriver.
//
// The whole point of Connect being the only constructor: if interception cannot be turned on, there
// is no Driver, so there is nothing that could navigate.
func TestAFenceThatWillNotArmRefusesToProduceADriver(t *testing.T) {
	fake, conn := dial(t)
	fake.Handle("Fetch.enable", func(cdptest.Call) (any, error) {
		return nil, errors.New("'Fetch.enable' wasn't found")
	})

	driver, err := Connect(context.Background(), conn)
	if !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("got %v, want ErrFenceNotAttached", err)
	}
	if driver != nil {
		t.Fatal("a Driver was handed back with no fence")
	}
	if slices.Contains(fake.Methods(), "Target.createTarget") {
		t.Errorf("a target was created despite the fence failing: %v", fake.Methods())
	}
}

func TestAutoAttachFailureAlsoRefuses(t *testing.T) {
	fake, conn := dial(t)
	fake.Handle("Target.setAutoAttach", func(cdptest.Call) (any, error) {
		return nil, errors.New("nope")
	})
	if _, err := Connect(context.Background(), conn); !errors.Is(err, browser.ErrFenceNotAttached) {
		t.Fatalf("got %v, want ErrFenceNotAttached", err)
	}
}

// TestOpenArmsTheTargetBeforeNavigating. The target is created, attached while paused, its Page
// domain enabled — and only then does the navigation happen.
func TestOpenArmsTheTargetBeforeNavigating(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}

	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.ID == "" {
		t.Fatal("no session id")
	}

	methods := fake.Methods()
	navigateAt := fake.IndexOf("Page.navigate")
	if navigateAt < 0 {
		t.Fatalf("never navigated: %v", methods)
	}
	for _, earlier := range []string{"Fetch.enable", "Target.setAutoAttach", "Target.createTarget", "Page.enable"} {
		at := fake.IndexOf(earlier)
		if at < 0 {
			t.Errorf("%s never happened: %v", earlier, methods)
			continue
		}
		if at > navigateAt {
			t.Errorf("%s happened AFTER the navigation: %v", earlier, methods)
		}
	}
}

// TestAPausedTargetIsReleased. A target born paused that nobody resumes never runs at all, so the
// fence would be perfect and the browser useless.
func TestAPausedTargetIsReleased(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err != nil {
		t.Fatalf("open: %v", err)
	}

	deadline := time.After(3 * time.Second)
	for {
		if slices.Contains(fake.Methods(), "Runtime.runIfWaitingForDebugger") {
			return
		}
		select {
		case <-deadline:
			t.Fatalf("a paused target was never resumed: %v", fake.Methods())
		case <-time.After(25 * time.Millisecond):
		}
	}
}

// TestEachAttachedSessionReArmsOnItsOwnChildren. Auto-attach is hierarchical: the spike measured
// that a cross-site iframe is never offered until the PAGE session auto-attaches too.
func TestEachAttachedSessionReArmsOnItsOwnChildren(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	if _, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"}); err != nil {
		t.Fatalf("open: %v", err)
	}

	deadline := time.After(3 * time.Second)
	for {
		for _, call := range fake.Calls() {
			if call.Method == "Target.setAutoAttach" && call.Session == "S1" {
				return
			}
		}
		select {
		case <-deadline:
			t.Fatal("the page session never re-armed; out-of-process iframes would be unfenced")
		case <-time.After(25 * time.Millisecond):
		}
	}
}

// TestActRefusesARefNoSnapshotShowed. Refs make "act on something that was not in the snapshot"
// representable only as a refusal — and a refusal, not an error, so the agent can recover.
func TestActRefusesARefNoSnapshotShowed(t *testing.T) {
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	result, err := driver.Act(context.Background(), session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  "e99",
	})
	if err != nil {
		t.Fatalf("an unknown ref must not be an error: %v", err)
	}
	if result.Outcome != browser.OutcomeRefused || !result.Valid() {
		t.Fatalf("got %+v, want a valid refusal", result)
	}
	if slices.Contains(fake.Methods(), "DOM.resolveNode") {
		t.Error("the driver tried to resolve a ref no snapshot had minted")
	}
}

func TestUnknownSessionsAreNamed(t *testing.T) {
	_, conn := dial(t)
	driver, err := Connect(context.Background(), conn)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	ctx := context.Background()
	if _, err := driver.Snapshot(ctx, "nope"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("snapshot: %v", err)
	}
	if _, err := driver.Screenshot(ctx, "nope"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("screenshot: %v", err)
	}
	if err := driver.Close(ctx, "nope"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("close: %v", err)
	}
}

// TestDriverSatisfiesTheContract keeps this implementation and the interface from drifting.
func TestDriverSatisfiesTheContract(t *testing.T) {
	var _ browser.Driver = (*Driver)(nil)
}
