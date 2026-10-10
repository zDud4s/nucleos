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

// guardWorld is the isolated world the keyboard guard is expected to ask in.
const guardWorld = 900

// isolatedGuard answers the guard's questions the way a page that lies in its main world would
// have them answered: the main world always says "not the panel", the isolated world (context
// guardWorld) says what is true. focusIn answers the focus question; nodeIn answers the climb from a
// node resolved into the isolated world (object ISO1; the main-world object is O1).
func isolatedGuard(fake *cdptest.Browser, focusIn, nodeIn string) {
	fake.Handle("Page.createIsolatedWorld", func(cdptest.Call) (any, error) {
		return map[string]any{"executionContextId": guardWorld}, nil
	})
	fake.Handle("Runtime.evaluate", func(call cdptest.Call) (any, error) {
		var params map[string]any
		_ = json.Unmarshal(call.Params, &params)
		if expr, _ := params["expression"].(string); strings.Contains(expr, "activeElement") {
			if id, _ := params["contextId"].(float64); int64(id) == guardWorld {
				return map[string]any{"result": map[string]any{"type": "string", "value": focusIn}}, nil
			}
			return map[string]any{"result": map[string]any{"type": "string", "value": ""}}, nil
		}
		return map[string]any{}, nil
	})
	fake.Handle("DOM.resolveNode", func(call cdptest.Call) (any, error) {
		var params map[string]any
		_ = json.Unmarshal(call.Params, &params)
		if id, _ := params["executionContextId"].(float64); int64(id) == guardWorld {
			return map[string]any{"object": map[string]any{"objectId": "ISO1"}}, nil
		}
		return map[string]any{"object": map[string]any{"objectId": "O1"}}, nil
	})
	fake.Handle("Runtime.callFunctionOn", func(call cdptest.Call) (any, error) {
		var params map[string]any
		_ = json.Unmarshal(call.Params, &params)
		f, _ := params["functionDeclaration"].(string)
		if params["objectId"] == "ISO1" && strings.Contains(f, "getRootNode") {
			return aimAnswer(nodeIn), nil
		}
		return map[string]any{}, nil
	})
}

func evalAnswers(fake *cdptest.Browser, focusIn string) {
	isolatedGuard(fake, focusIn, "")
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
	isolatedGuard(fake, "", "panel")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionType, Ref: "e1", Text: "hello"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("typing into the panel came back as %q", result.Outcome)
	}
	if n := keysSent(fake); n != 0 {
		t.Errorf("%d input events reached the panel", n)
	}
}

// TestTypeToANonFocusableRefIsRefusedWhileFocusIsInThePanel. focus() on a node that cannot take focus
// leaves it where it was, and an earlier Tab can have left it in the panel: what decides is where
// focus is AFTER the focus step.
func TestTypeToANonFocusableRefIsRefusedWhileFocusIsInThePanel(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	isolatedGuard(fake, "panel", "")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionType, Ref: "e1", Text: "hello"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("text meant for a page node came back as %q with focus in the panel", result.Outcome)
	}
	if n := keysSent(fake); n != 0 {
		t.Errorf("%d input events reached the panel", n)
	}
}

// TestPressWithARefIsRefusedWhileFocusIsInThePanel is the same hole through press.
func TestPressWithARefIsRefusedWhileFocusIsInThePanel(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	isolatedGuard(fake, "panel", "")

	result := act(t, driver, id, browser.Action{Kind: browser.ActionPress, Ref: "e1", Text: "Enter"})
	if result.Outcome != browser.OutcomeRefused {
		t.Fatalf("a key meant for a page node came back as %q with focus in the panel", result.Outcome)
	}
	if n := keysSent(fake); n != 0 {
		t.Errorf("%d key events reached the panel", n)
	}
}

// TestTheGuardAsksInAnIsolatedWorld. A page can redefine activeElement, parentNode and tagName in its
// own world, so neither question may be put there.
func TestTheGuardAsksInAnIsolatedWorld(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	isolatedGuard(fake, "", "")

	if result := act(t, driver, id, browser.Action{Kind: browser.ActionPress, Text: "Enter"}); result.Outcome != browser.OutcomeDone {
		t.Fatalf("press: %+v", result.Refusal)
	}
	if result := act(t, driver, id, browser.Action{Kind: browser.ActionType, Ref: "e1", Text: "x"}); result.Outcome != browser.OutcomeDone {
		t.Fatalf("type: %+v", result.Refusal)
	}

	world := callIndex(fake, func(m string, p map[string]any) bool {
		return m == "Page.createIsolatedWorld" && p["frameId"] != nil && p["frameId"] != ""
	})
	if world < 0 {
		t.Fatalf("no isolated world was made for the page's frame: %v", fake.Methods())
	}
	for _, call := range fake.Calls() {
		var p map[string]any
		_ = json.Unmarshal(call.Params, &p)
		switch call.Method {
		case "Runtime.evaluate":
			if expr, _ := p["expression"].(string); strings.Contains(expr, "activeElement") {
				if id, _ := p["contextId"].(float64); int64(id) != guardWorld {
					t.Errorf("the focus question ran outside the isolated world: %v", p["contextId"])
				}
			}
		case "Runtime.callFunctionOn":
			if f, _ := p["functionDeclaration"].(string); strings.Contains(f, "getRootNode") && p["objectId"] != "ISO1" {
				t.Errorf("the node climb ran on a main-world object: %v", p["objectId"])
			}
		}
	}
	if callIndex(fake, func(m string, p map[string]any) bool {
		id, _ := p["executionContextId"].(float64)
		return m == "DOM.resolveNode" && int64(id) == guardWorld
	}) < 0 {
		t.Errorf("the ref's node was not resolved into the isolated world")
	}
}

// TestAGuardErrorOnAVisibleSessionRefuses. Undecided is not allowed.
func TestAGuardErrorOnAVisibleSessionRefuses(t *testing.T) {
	for _, action := range []browser.Action{
		{Kind: browser.ActionPress, Text: "Enter"},
		{Kind: browser.ActionType, Ref: "e1", Text: "x"},
		{Kind: browser.ActionSelect, Ref: "e1", Text: "x"},
	} {
		fake, driver, id := withPanelWorld(t)
		isolatedGuard(fake, "", "")
		fake.Handle("Page.createIsolatedWorld", func(cdptest.Call) (any, error) {
			return nil, errors.New("no world")
		})

		result, err := driver.Act(context.Background(), id, action)
		if err == nil && result.Outcome != browser.OutcomeRefused {
			t.Errorf("%s went ahead although the guard could not decide: %q", action.Kind, result.Outcome)
		}
		if n := keysSent(fake); n != 0 {
			t.Errorf("%s: %d input events were sent unchecked", action.Kind, n)
		}
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

// TestAPendingAskIsReplayedIntoALaterWorld. An ask still unanswered must survive a navigation; the
// latest of each kind is kept, and an ask is not a message so it is never in the history.
func TestAPendingAskIsReplayedIntoALaterWorld(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	ctx := context.Background()
	markLive(driver, id, 70)

	for _, msg := range []string{
		`{"v":1,"kind":"ask_keep","hosts":["old-host"]}`,
		`{"v":1,"kind":"ask_keep","hosts":["a-host"]}`,
		`{"v":1,"kind":"ask_wheel","reason":"need-help"}`,
		`{"v":1,"kind":"message","role":"agent","ts":"t","text":"kept-message"}`,
	} {
		if err := driver.PanelPush(ctx, id, json.RawMessage(msg)); err != nil {
			t.Fatalf("%s: %v", msg, err)
		}
	}

	panelWorldCreated(fake, cdpOf(driver, id), 71)
	eventually(t, "state reaching the later world", 3*time.Second, func() bool {
		return strings.Contains(strings.Join(pushesInto(fake, 71), "\n"), `"kind":"state"`)
	})
	got := strings.Join(pushesInto(fake, 71), "\n")
	for _, want := range []string{"kept-message", "a-host", "need-help"} {
		if !strings.Contains(got, want) {
			t.Errorf("%q was not replayed into the later world: %q", want, got)
		}
	}
	if strings.Contains(got, "old-host") {
		t.Errorf("a superseded ask was replayed: %q", got)
	}
}

// TestAnAnsweredAskIsNotReplayed. The panel answers an ask_keep with keep and an ask_wheel with
// take_wheel; after that a new world must not re-ask.
func TestAnAnsweredAskIsNotReplayed(t *testing.T) {
	fake, driver, id := withPanelWorld(t)
	ctx := context.Background()
	markLive(driver, id, 70)

	for _, msg := range []string{
		`{"v":1,"kind":"ask_keep","hosts":["a-host"]}`,
		`{"v":1,"kind":"ask_wheel","reason":"need-help"}`,
	} {
		if err := driver.PanelPush(ctx, id, json.RawMessage(msg)); err != nil {
			t.Fatalf("%s: %v", msg, err)
		}
	}
	on := cdpOf(driver, id)
	binding(fake, on, 70, `{"v":1,"kind":"keep","keep":true,"writable":false}`)
	binding(fake, on, 70, `{"v":1,"kind":"take_wheel"}`)
	p, _ := driver.panelOf(id)
	eventually(t, "both answers being heard", 2*time.Second, func() bool {
		p.mu.Lock()
		defer p.mu.Unlock()
		return len(p.asks) == 0
	})

	panelWorldCreated(fake, on, 71)
	eventually(t, "state reaching the later world", 3*time.Second, func() bool {
		return strings.Contains(strings.Join(pushesInto(fake, 71), "\n"), `"kind":"state"`)
	})
	if got := strings.Join(pushesInto(fake, 71), "\n"); strings.Contains(got, "ask_") {
		t.Errorf("an answered ask was replayed: %q", got)
	}
}
