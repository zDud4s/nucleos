package chrome

import (
	"context"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// opened is the setup every test here shares: a fenced driver with a session on a page.
func opened(t *testing.T, driver *Driver) browser.Session {
	t.Helper()
	session, err := driver.Open(context.Background(), browser.OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	return session
}

func connected(t *testing.T) (*cdptest.Browser, *Driver) {
	t.Helper()
	fake, conn := dial(t)
	autoAttachOnCreate(fake)
	driver, err := Connect(context.Background(), conn, projectPolicy())
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	// Short, because every test below is about what happens at the bound and none of them is about
	// how long the bound is.
	driver.readyWithin = 150 * time.Millisecond
	driver.idleGrace = 30 * time.Millisecond
	return fake, driver
}

// TestOpeningWaitsForThePageToArrive.
//
// `Page.navigate` returns on commit, not on arrival. Before this, Open returned at that moment and
// the agent's first snapshot raced the load — reading a script-rendered page as an empty one, with
// nothing anywhere reporting a problem because an empty reading of a half-loaded page is a correct
// reading of that instant.
func TestOpeningWaitsForThePageToArrive(t *testing.T) {
	fake, driver := connected(t)
	fake.NoAutoLoad = true

	// The page arrives well after navigate answers. If Open did not wait, it would return before
	// this fires and the assertion below would see the earlier instant.
	fake.Handle("Page.navigate", func(cdptest.Call) (any, error) {
		go func() {
			time.Sleep(40 * time.Millisecond)
			fake.Emit("S1", "Page.lifecycleEvent", map[string]any{"name": "networkAlmostIdle"})
		}()
		return map[string]any{}, nil
	})

	started := time.Now()
	session := opened(t, driver)

	if session.StillLoading {
		t.Error("the page arrived inside the bound; reporting it as unfinished sends the agent to re-read a page that is already there")
	}
	if elapsed := time.Since(started); elapsed < 40*time.Millisecond {
		t.Errorf("open returned in %v, before the page had arrived: it is still racing the load", elapsed)
	}
}

// TestAPageThatNeverArrivesIsSaidToBeUnfinished.
//
// The bound is the honest half. A wait with no bound is a call that never returns; a bound with no
// report is a silence the agent reads as readiness, which is the failure this whole path exists to
// remove.
func TestAPageThatNeverArrivesIsSaidToBeUnfinished(t *testing.T) {
	fake, driver := connected(t)
	fake.NoAutoLoad = true

	session := opened(t, driver)

	if !session.StillLoading {
		t.Error("a page that never loaded came back looking finished")
	}
	if session.ID == "" {
		t.Error("the session must still exist: an unfinished page is one the agent can read and act on, not an error")
	}
}

// TestAnActThatMovesThePageSaysSo.
//
// A click that navigates leaves every ref the agent holds naming an element in a document that is
// gone. Reported as plain "done", the agent would act on one of them next — and the ref would
// resolve against nothing, or worse, against whatever the new renderer gave the same number to.
func TestAnActThatMovesThePageSaysSo(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)

	driver.sessions[session.ID].refs["e1"] = nodeKey{session: "S1", backend: 42}
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "O1"}}, nil
	})
	// The click navigates. Emitted from the handler so it lands while the act is still in flight,
	// which is where a real one would land.
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		fake.Emit("S1", "Page.frameNavigated", map[string]any{"frame": map[string]any{"id": "F1"}})
		fake.Emit("S1", "Page.lifecycleEvent", map[string]any{"name": "networkAlmostIdle"})
		return map[string]any{}, nil
	})

	result, err := driver.Act(context.Background(), session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  "e1",
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if !result.Navigated {
		t.Fatal("a click that replaced the document was reported as an ordinary one")
	}
	if result.URL != "https://example.org/landed" {
		t.Errorf("the act did not say where the page went: %q", result.URL)
	}
	if result.StillLoading {
		t.Error("the new page arrived; saying otherwise costs the agent a re-read")
	}

	// And the refs are gone with the document. The agent that ignores `navigated` gets an answer it
	// knows what to do with rather than a resolve against a document that no longer exists.
	again, err := driver.Act(context.Background(), session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  "e1",
	})
	if err != nil {
		t.Fatalf("second act: %v", err)
	}
	if again.Outcome != browser.OutcomeRefused {
		t.Error("a ref from before the navigation was still honoured")
	}
}

// TestAnActThatDoesNotMoveThePageSaysNothingAboutIt.
//
// The other half, and the reason `navigated` is worth anything: a click that opens a menu must not
// tell the agent to throw away refs that are still good.
func TestAnActThatDoesNotMoveThePageSaysNothingAboutIt(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)

	driver.sessions[session.ID].refs["e1"] = nodeKey{session: "S1", backend: 42}
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "O1"}}, nil
	})

	result, err := driver.Act(context.Background(), session.ID, browser.Action{
		Kind: browser.ActionClick,
		Ref:  "e1",
	})
	if err != nil {
		t.Fatalf("act: %v", err)
	}
	if result.Navigated || result.URL != "" {
		t.Errorf("a click that changed nothing was reported as a navigation: %+v", result)
	}
	if _, still := driver.sessions[session.ID].refs["e1"]; !still {
		t.Error("refs were dropped by an act that did not move the page")
	}
}
