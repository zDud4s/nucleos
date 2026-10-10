package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/cdp/cdptest"
)

// The half of Look a real browser cannot be made to demonstrate: what happens when the picture
// fails.
//
// The gate proves the overlay is drawn, composed in the right place, and taken away again — against
// Chromium, which is the only thing that can answer those. What it cannot easily produce is a
// capture that ERRORS after the drawing succeeded, and that is precisely the path where a missing
// removal would leave an overlay behind for good: the verb returns an error, nobody looks at the
// page again until the next snapshot, and the next snapshot reads our drawing as the page's content.

// drewOn makes the fake answer the two calls a draw makes, and returns the labels it should report.
func drewOn(fake *cdptest.Browser, labels ...string) {
	answer, _ := json.Marshal(labels)
	fake.Handle("Runtime.callFunctionOn", func(call cdptest.Call) (any, error) {
		if strings.Contains(string(call.Params), overlayID) {
			return map[string]any{
				"result": map[string]any{"value": string(answer)},
			}, nil
		}
		return reachable(), nil
	})
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "OBJ"}}, nil
	})
}

// TestTheOverlayIsTakenAwayEvenWhenThePictureFails.
//
// The defer is the whole mechanism, and its absence is invisible on the happy path — which is where
// every other test of this verb lives. Here the capture fails, Look returns an error, and the
// question is whether the page was left holding a hundred of our divs.
func TestTheOverlayIsTakenAwayEvenWhenThePictureFails(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	drewOn(fake, "e1")
	driver.mu.Lock()
	entry := driver.sessions[session.ID]
	entry.refs["e1"] = nodeKey{session: entry.cdp, backend: 42}
	driver.mu.Unlock()
	fake.Handle("Page.captureScreenshot", func(cdptest.Call) (any, error) {
		return nil, errors.New("the renderer went away")
	})

	if _, err := driver.Look(context.Background(), session.ID); err == nil {
		t.Fatal("a capture that failed was reported as a picture")
	}

	if !erased(fake) {
		t.Fatal("the overlay was left in the page after a look that failed, so the next reading of" +
			" that page reads our own drawing as its content")
	}
}

// TestALookOfAPageWithNoRefsDrawsNothingAtAll.
//
// The unit-level twin of the gate's control. It is here as well as there because the two catch
// different mistakes: the gate would catch a labelling that came from the page, and this catches a
// draw call issued against a document with nothing to draw — which would inject an empty overlay,
// and therefore a removal, on every look at an unread page.
func TestALookOfAPageWithNoRefsDrawsNothingAtAll(t *testing.T) {
	fake, driver := connected(t)
	session := opened(t, driver)
	fake.Handle("Page.captureScreenshot", func(cdptest.Call) (any, error) {
		return map[string]any{"data": ""}, nil
	})

	result, err := driver.Look(context.Background(), session.ID)
	if err != nil {
		t.Fatalf("look: %v", err)
	}
	if len(result.Labels) != 0 {
		t.Fatalf("labels on a page with no refs: %v", result.Labels)
	}
	for _, call := range fake.Calls() {
		if call.Method == "DOM.resolveNode" {
			t.Fatal("a node was resolved for a page that has no refs, so the draw ran against a" +
				" document with nothing to draw on")
		}
	}
}

// erased says whether the removal script was run against anything.
func erased(fake *cdptest.Browser) bool {
	for _, call := range fake.Calls() {
		if call.Method != "Runtime.evaluate" {
			continue
		}
		if strings.Contains(string(call.Params), overlayID) {
			return true
		}
	}
	return false
}

// TestLookHidesThePanelDuringTheCapture. The picture is the agent's, and the panel is the person's:
// each panel world is told to hide before the capture and to show again after it, in that order.
func TestLookHidesThePanelDuringTheCapture(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	session := opened(t, driver)
	const panelContext = 70
	panelWorldCreated(fake, cdpOf(driver, session.ID), panelContext)
	key := contextKey{session: cdpOf(driver, session.ID), id: panelContext}
	deadline := time.Now().Add(2 * time.Second)
	for {
		driver.mu.Lock()
		_, known := driver.contexts[key]
		driver.mu.Unlock()
		if known {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("the driver never registered the panel world")
		}
		time.Sleep(5 * time.Millisecond)
	}
	fake.Handle("Page.captureScreenshot", func(cdptest.Call) (any, error) {
		return map[string]any{"data": ""}, nil
	})

	if _, err := driver.Look(context.Background(), session.ID); err != nil {
		t.Fatalf("look: %v", err)
	}

	hideAt, showAt, captureAt := -1, -1, -1
	for i, call := range fake.Calls() {
		switch call.Method {
		case "Page.captureScreenshot":
			captureAt = i
		case "Runtime.evaluate":
			var params struct {
				Expression string `json:"expression"`
				ContextID  int64  `json:"contextId"`
			}
			if json.Unmarshal(call.Params, &params) != nil || params.ContextID != panelContext {
				continue
			}
			if !strings.Contains(params.Expression, "__nucleosHide") {
				continue
			}
			if strings.Contains(params.Expression, "(true)") {
				hideAt = i
			} else if strings.Contains(params.Expression, "(false)") {
				showAt = i
			}
		}
	}
	if captureAt < 0 {
		t.Fatal("no capture was taken")
	}
	if hideAt < 0 || hideAt > captureAt {
		t.Errorf("the panel was not hidden before the capture (hide %d, capture %d)", hideAt, captureAt)
	}
	if showAt < 0 || showAt < captureAt {
		t.Errorf("the panel was not shown again after the capture (show %d, capture %d)", showAt, captureAt)
	}
}
