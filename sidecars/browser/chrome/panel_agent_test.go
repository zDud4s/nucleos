// §spec browser-com-painel

package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// withPanelWorld opens a visible session whose panel world (context 70) the driver has registered,
// and gives it one ref, e1.
func withPanelWorld(t *testing.T) (*cdptest.Browser, *Driver, browser.SessionID) {
	t.Helper()
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID
	on := cdpOf(driver, id)
	panelWorldCreated(fake, on, 70)
	key := contextKey{session: on, id: 70}
	eventually(t, "the panel world being registered", 2*time.Second, func() bool {
		driver.mu.Lock()
		defer driver.mu.Unlock()
		_, known := driver.contexts[key]
		return known
	})
	driver.mu.Lock()
	driver.sessions[id].refs["e1"] = nodeKey{session: on, backend: 42}
	driver.mu.Unlock()
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "O1"}}, nil
	})
	return fake, driver, id
}

func callIndex(fake *cdptest.Browser, match func(method string, params map[string]any) bool) int {
	for i, call := range fake.Calls() {
		var params map[string]any
		_ = json.Unmarshal(call.Params, &params)
		if match(call.Method, params) {
			return i
		}
	}
	return -1
}

func hideCall(want string) func(string, map[string]any) bool {
	return func(method string, params map[string]any) bool {
		expr, _ := params["expression"].(string)
		return method == "Runtime.evaluate" && strings.Contains(expr, "__nucleosHide") && strings.Contains(expr, want)
	}
}

// TestAnAgentClickHidesThePanelAroundThePointer. The pellicle covers the page in agent mode, so an
// aim check or a mouse event with the panel up lands on the panel and not on the page.
func TestAnAgentClickHidesThePanelAroundThePointer(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return aimAnswer(`{"x":120,"y":48,"sized":true,"reached":true,"on_top":""}`), nil
	})

	if result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"}); result.Outcome != browser.OutcomeDone {
		t.Fatalf("the click was refused: %+v", result.Refusal)
	}

	hide := callIndex(fake, hideCall("(true)"))
	show := callIndex(fake, hideCall("(false)"))
	aimAt := callIndex(fake, func(m string, p map[string]any) bool {
		f, _ := p["functionDeclaration"].(string)
		return m == "Runtime.callFunctionOn" && strings.Contains(f, "elementFromPoint")
	})
	released := callIndex(fake, func(m string, p map[string]any) bool {
		return m == "Input.dispatchMouseEvent" && p["type"] == "mouseReleased"
	})
	if hide < 0 || aimAt < 0 || hide > aimAt {
		t.Errorf("the panel was not hidden before the aim check (hide %d, aim %d)", hide, aimAt)
	}
	if show < 0 || released < 0 || show < released {
		t.Errorf("the panel was not restored after the mouse events (show %d, release %d)", show, released)
	}
}

// TestAnAgentClickRestoresThePanelWhenRefused. A refusal is a path out like any other.
func TestAnAgentClickRestoresThePanelWhenRefused(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	fake.Handle("Runtime.callFunctionOn", func(cdptest.Call) (any, error) {
		return aimAnswer(`{"x":1,"y":1,"sized":true,"reached":false,"on_top":"div"}`), nil
	})
	if result := act(t, driver, id, browser.Action{Kind: browser.ActionClick, Ref: "e1"}); result.Outcome != browser.OutcomeRefused {
		t.Fatalf("expected a refusal, got %q", result.Outcome)
	}
	if callIndex(fake, hideCall("(true)")) < 0 || callIndex(fake, hideCall("(false)")) < 0 {
		t.Errorf("the panel was not hidden and restored around a refused click: %v", fake.Methods())
	}
}

func evalAnswers(fake *cdptest.Browser, focusIn string) {
	fake.Handle("Runtime.evaluate", func(call cdptest.Call) (any, error) {
		var params map[string]any
		_ = json.Unmarshal(call.Params, &params)
		if expr, _ := params["expression"].(string); strings.Contains(expr, "activeElement") {
			return map[string]any{"result": map[string]any{"type": "string", "value": focusIn}}, nil
		}
		return map[string]any{}, nil
	})
}

func keysSent(fake *cdptest.Browser) int {
	return countCalls(fake, "Input.dispatchKeyEvent") + countCalls(fake, "Input.insertText")
}

// TestPressIsRefusedWhileFocusIsInThePanel. A key goes wherever focus is, and a CDP key is trusted.
func TestPressIsRefusedWhileFocusIsInThePanel(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	evalAnswers(fake, "panel")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionPress, Text: "Enter"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("a key into the panel came back as %q", result.Outcome)
	}
	if !strings.Contains(result.Refusal.Detail, "panel") {
		t.Errorf("the refusal does not name the panel: %q", result.Refusal.Detail)
	}
	if n := keysSent(fake); n != 0 {
		t.Errorf("%d key events were sent into the panel", n)
	}
}

// TestPressIsAllowedWhileFocusIsOnThePage is the control for the refusal above.
func TestPressIsAllowedWhileFocusIsOnThePage(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	evalAnswers(fake, "")

	if result := act(t, driver, id, browser.Action{Kind: browser.ActionPress, Text: "Enter"}); result.Outcome != browser.OutcomeDone {
		t.Fatalf("a key on the page was refused: %+v", result.Refusal)
	}
	if countCalls(fake, "Input.dispatchKeyEvent") == 0 {
		t.Errorf("no key event was sent")
	}
}

// TestTypeByRefIsRefusedInsideThePanel. A ref whose node lives under the panel's host is the panel's.
func TestTypeByRefIsRefusedInsideThePanel(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	fake.Handle("Runtime.callFunctionOn", func(call cdptest.Call) (any, error) {
		var params map[string]any
		_ = json.Unmarshal(call.Params, &params)
		if f, _ := params["functionDeclaration"].(string); strings.Contains(f, "getRootNode") {
			return aimAnswer("panel"), nil
		}
		return map[string]any{}, nil
	})

	result := act(t, driver, id, browser.Action{Kind: browser.ActionType, Ref: "e1", Text: "hello"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("typing into the panel came back as %q", result.Outcome)
	}
	if n := keysSent(fake); n != 0 {
		t.Errorf("%d input events reached the panel", n)
	}
}

func markLive(driver *Driver, id browser.SessionID, contextID int64) {
	driver.mu.Lock()
	p := driver.sessions[id].panel
	driver.mu.Unlock()
	p.mu.Lock()
	p.live[contextKey{session: cdpOf(driver, id), id: contextID}] = true
	p.mu.Unlock()
}

// TestPanelPushRefusesStateFromTheCore. State is the driver's own.
func TestPanelPushRefusesStateFromTheCore(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	markLive(driver, id, 70)

	err := driver.PanelPush(context.Background(), id, json.RawMessage(`{"v":1,"kind":"state","mode":"human","host":"evil","collapsed":false}`))
	if !errors.Is(err, browser.ErrUnsupported) {
		t.Fatalf("a state from the core was accepted: %v", err)
	}
	// The driver's own state pushes may appear; the core's one (host "evil") must not.
	if got := strings.Join(pushesInto(fake, 70), "\n"); strings.Contains(got, "evil") {
		t.Errorf("a refused state was forwarded: %v", got)
	}
}

// TestOnlyMessagesAreReplayed. An ask is live-only; a world opened later must not re-ask.
func TestOnlyMessagesAreReplayed(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	ctx := context.Background()
	markLive(driver, id, 70)

	if err := driver.PanelPush(ctx, id, json.RawMessage(`{"v":1,"kind":"ask_keep","hosts":["a-host"]}`)); err != nil {
		t.Fatalf("ask_keep: %v", err)
	}
	if err := driver.PanelPush(ctx, id, json.RawMessage(`{"v":1,"kind":"message","role":"agent","ts":"t","text":"kept-message"}`)); err != nil {
		t.Fatalf("message: %v", err)
	}
	if got := strings.Join(pushesInto(fake, 70), "\n"); !strings.Contains(got, "ask_keep") {
		t.Errorf("the ask was not forwarded live: %q", got)
	}

	panelWorldCreated(fake, cdpOf(driver, id), 71)
	eventually(t, "the message reaching the later world", 3*time.Second, func() bool {
		return strings.Contains(strings.Join(pushesInto(fake, 71), "\n"), "kept-message")
	})
	if got := strings.Join(pushesInto(fake, 71), "\n"); strings.Contains(got, "ask_keep") {
		t.Errorf("an ask was replayed into a later world: %q", got)
	}
}
