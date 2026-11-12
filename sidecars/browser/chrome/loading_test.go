package chrome

import (
	"encoding/json"
	"strings"
	"testing"

	"nucleosbrowser/cdp/cdptest"
)

// answering makes the page say what `locate` asks it, which is where the readiness in a snapshot
// comes from. Everything else the driver evaluates keeps the fake's ordinary answer.
func answering(fake *cdptest.Browser, ready string) {
	fake.Handle("Runtime.evaluate", func(call cdptest.Call) (any, error) {
		var params struct {
			Expression string `json:"expression"`
		}
		if err := json.Unmarshal(call.Params, &params); err != nil {
			return map[string]any{}, nil
		}
		if !strings.Contains(params.Expression, "document.readyState") {
			return map[string]any{}, nil
		}
		located, _ := json.Marshal(map[string]any{
			"url":   "https://example.org/",
			"title": "Example",
			"ready": ready,
		})
		return map[string]any{
			"result": map[string]any{"value": string(located)},
		}, nil
	})
}

// TestAReadingSaysWhetherThePageHasFinished.
//
// The flag could be raised and never lowered. Opening said `still_loading`, acting said it, and the
// only move an agent has in reply — take another reading — said nothing at all, because the snapshot
// had no such field. There is no `wait` verb on purpose, so an agent that was told the page had not
// arrived had been handed a fact with no way to act on it and no way to see it change.
func TestAReadingSaysWhetherThePageHasFinished(t *testing.T) {
	for _, one := range []struct {
		ready string
		still bool
	}{
		{ready: "loading", still: true},
		{ready: "interactive", still: true},
		{ready: "complete", still: false},
	} {
		t.Run(one.ready, func(t *testing.T) {
			fake, driver := connected(t)
			session := opened(t, driver)
			answering(fake, one.ready)

			if got := snapshotOf(t, driver, session.ID).StillLoading; got != one.still {
				t.Fatalf("readyState %q was reported as still_loading=%v", one.ready, got)
			}
		})
	}
}

// TestAPageThatCannotBeAskedIsNotCalledUnfinished.
//
// The direction the guess must not go. An evaluate that fails says nothing about the document, and
// a reading that turned silence into "not finished" would send the agent round a loop it could never
// leave — every snapshot reporting a page that never settles, on a page that settled long ago.
func TestAPageThatCannotBeAskedIsNotCalledUnfinished(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	answering(fake, "")

	if snapshotOf(t, driver, session.ID).StillLoading {
		t.Fatal("a page that could not be asked was reported as unfinished")
	}
}

// TestAPageWaitingOnTheFerryIsUnfinished.
//
// readyState is about the DOCUMENT, and the ferry is the one thing in flight that it cannot see: a
// carried request does not go through the browser's network stack, so a page can be `complete` and
// still be waiting for the content it will render itself from.
func TestAPageWaitingOnTheFerryIsUnfinished(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	answering(fake, "complete")

	entry, err := driver.lookup(session.ID)
	if err != nil {
		t.Fatalf("lookup: %v", err)
	}
	driver.setCarrying(entry, 1)

	if !snapshotOf(t, driver, session.ID).StillLoading {
		t.Fatal("a page still waiting on a ferried request was reported as finished")
	}
}
