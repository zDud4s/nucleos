package chrome

import (
	"encoding/json"
	"strings"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// aimAnswer is what the page says about where a click would land, for a test that needs to say
// something other than "it is right there and nothing is on top of it".
func aimAnswer(payload string) map[string]any {
	return map[string]any{"result": map[string]any{"type": "string", "value": payload}}
}

// mouseEvents is every Input.dispatchMouseEvent the driver sent, in order. The ORDER is half the
// claim: a press before the move is a press with no hover in front of it.
func mouseEvents(fake *cdptest.Browser) []map[string]any {
	var sent []map[string]any
	for _, call := range fake.Calls() {
		if call.Method != "Input.dispatchMouseEvent" {
			continue
		}
		var params map[string]any
		if err := json.Unmarshal(call.Params, &params); err != nil {
			continue
		}
		sent = append(sent, params)
	}
	return sent
}

// TestAClickMovesThePointerBeforeItPresses.
//
// The click used to be `this.click()`: one synthetic event on the element, isTrusted false, and no
// mousemove, mouseover, pointerdown, mousedown or mouseup anywhere. The keyboard had gone through
// the browser's own input pipeline for as long as `press` has existed; the mouse never had.
//
// It is not a purity argument. A great many components — menus, dropdowns, popovers — open on
// pointerdown or mousedown rather than on click, so under the old verb they did not open, the act
// reported DONE, and the reading afterwards correctly showed them closed. The move is what fires
// mouseover, which is also the whole of why hover menus work now without a verb of their own.
func TestAClickMovesThePointerBeforeItPresses(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return aimAnswer(`{"x":120,"y":48,"sized":true,"reached":true,"on_top":""}`), nil
	})

	if result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"}); result.Outcome != browser.OutcomeDone {
		t.Fatalf("an ordinary click was refused: %+v", result.Refusal)
	}

	sent := mouseEvents(fake)
	if len(sent) != 3 {
		t.Fatalf("the click sent %d mouse events; a page hears a move, a press and a release", len(sent))
	}
	for i, want := range []string{"mouseMoved", "mousePressed", "mouseReleased"} {
		if sent[i]["type"] != want {
			t.Errorf("event %d was %v, want %s", i, sent[i]["type"], want)
		}
		if sent[i]["x"] != 120.0 || sent[i]["y"] != 48.0 {
			t.Errorf("event %d landed at %v,%v rather than where the element is", i, sent[i]["x"], sent[i]["y"])
		}
	}
}

// TestSomethingOnTopOfAnElementIsSaidRatherThanClickedThrough.
//
// The refusal this verb could not previously make. `this.click()` fired on the element whatever was
// drawn over it, so a button under a consent banner was clicked, reported done, and did nothing —
// and the agent, reading a page where nothing changed, clicked it again.
//
// A real mouse event lands at a POINT, so what is at that point becomes a question with an answer,
// and the answer is worth more than the click was: an agent told a banner is in the way can dismiss
// the banner.
func TestSomethingOnTopOfAnElementIsSaidRatherThanClickedThrough(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return aimAnswer(`{"x":10,"y":10,"sized":true,"reached":false,"on_top":"div \"We use cookies\""}`), nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("a click through a banner came back as %q", result.Outcome)
	}
	if !strings.Contains(result.Refusal.Detail, "We use cookies") {
		t.Errorf("the refusal does not name what is in the way, which is the only part the agent can"+
			" act on: %q", result.Refusal.Detail)
	}
	if sent := mouseEvents(fake); len(sent) != 0 {
		t.Errorf("%d mouse events were sent at a point that is not the element", len(sent))
	}
}

// TestAnElementWithNoSizeIsNotClicked. The other thing `this.click()` did happily: fire on something
// with no box at all — collapsed, display:none, a menu item that has not been opened yet. There is
// nowhere to click it, and saying so points at the real next move, which is opening whatever holds
// it.
func TestAnElementWithNoSizeIsNotClicked(t *testing.T) {
	fake, driver := connected(t)
	id := withRef(t, fake, driver)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return aimAnswer(`{"x":0,"y":0,"sized":false,"reached":false,"on_top":"nothing at all"}`), nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("a click on something with no size came back as %q", result.Outcome)
	}
	if !strings.Contains(result.Refusal.Detail, "no size") {
		t.Errorf("the refusal does not say what is wrong: %q", result.Refusal.Detail)
	}
	if sent := mouseEvents(fake); len(sent) != 0 {
		t.Errorf("%d mouse events were sent at an element that is not on the page", len(sent))
	}
}
