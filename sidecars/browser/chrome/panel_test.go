// §spec browser-com-painel

package chrome

import (
	"context"
	"encoding/json"
	"errors"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/cdp/cdptest"
)

// The driver is the panel channel's implementation; the assertion is here so a driver that stops
// being one fails to compile rather than failing at the one place that asks.
var _ browser.Panel = (*Driver)(nil)

// panelWorldCreated is Chromium reporting a new execution context, named as the isolated world the
// panel was injected into. The name is what the driver reads to know which world it is.
func panelWorldCreated(fake *cdptest.Browser, session cdp.SessionID, contextID int64) {
	fake.Emit(string(session), "Runtime.executionContextCreated", map[string]any{
		"context": map[string]any{
			"id":      contextID,
			"origin":  "",
			"name":    panelWorld,
			"auxData": map[string]any{"frameId": "F1", "isDefault": false},
		},
	})
}

// binding is a page calling the panel binding from one execution context.
func binding(fake *cdptest.Browser, session cdp.SessionID, contextID int64, payload string) {
	fake.Emit(string(session), "Runtime.bindingCalled", map[string]any{
		"name":               panelBinding,
		"payload":            payload,
		"executionContextId": contextID,
	})
}

// pushesInto is every expression the driver evaluated into one execution context that calls the
// panel's entry. Matched by the context id in the params, never by the method alone: the history of
// the fake holds startup calls and the ferry's own evaluations.
func pushesInto(fake *cdptest.Browser, contextID int64) []string {
	var out []string
	for _, call := range fake.Calls() {
		if call.Method != "Runtime.evaluate" {
			continue
		}
		var params struct {
			Expression string `json:"expression"`
			ContextID  int64  `json:"contextId"`
		}
		if err := json.Unmarshal(call.Params, &params); err != nil || params.ContextID != contextID {
			continue
		}
		if strings.Contains(params.Expression, "__nucleosPush") {
			out = append(out, params.Expression)
		}
	}
	return out
}

// TestVisibleOpenArmsThePanelWorldAndScopedBinding. The panel lives in its own isolated world and the
// binding is scoped to that world by name, so a page's own scripts can neither see the bundle nor call
// the channel. Both must be armed before the first navigation or the first document runs without a
// panel. A driver that is not visible has no panel and must not arm one.
func TestVisibleOpenArmsThePanelWorldAndScopedBinding(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	opened(t, driver)

	methods := fake.Methods()
	scriptAt, bindingAt := -1, -1
	for i, call := range fake.Calls() {
		params := paramsMap(t, call)
		switch call.Method {
		case "Page.addScriptToEvaluateOnNewDocument":
			if params["worldName"] == panelWorld {
				scriptAt = i
				if params["runImmediately"] != true {
					t.Errorf("the panel script is not run immediately in documents already loaded: %v", params)
				}
				if source, _ := params["source"].(string); !strings.Contains(source, "__nucleosPush") {
					t.Errorf("the injected source is not the panel bundle")
				}
			}
		case "Runtime.addBinding":
			if params["name"] == panelBinding {
				bindingAt = i
				if params["executionContextName"] != panelWorld {
					t.Errorf("the panel binding is not scoped to the panel world: %v", params)
				}
			}
		}
	}
	if scriptAt < 0 {
		t.Fatalf("no script was injected into the %q world: %v", panelWorld, methods)
	}
	if bindingAt < 0 {
		t.Fatalf("no %q binding was added: %v", panelBinding, methods)
	}
	navigateAt := slices.Index(methods, "Page.navigate")
	if navigateAt < 0 || scriptAt > navigateAt || bindingAt > navigateAt {
		t.Errorf("the panel was armed at %d/%d, not before the first navigation at %d: %v", scriptAt, bindingAt, navigateAt, methods)
	}

	// The negative case: a driver without a window arms nothing of the kind.
	plain, _, _ := personSession(t)
	for _, call := range plain.Calls() {
		params := paramsMap(t, call)
		if params["worldName"] == panelWorld || params["name"] == panelBinding {
			t.Errorf("a driver that is not visible armed the panel: %s %v", call.Method, params)
		}
	}
}

// TestPanelBindingFromAnotherContextIsDropped. The binding is only trusted from the panel's own
// world of a visible session: the same call from a page's world is an attempt to speak as the panel.
// The control comes first and last, so the drop is shown to be about the context and not about a
// subscriber that was never listening.
func TestPanelBindingFromAnotherContextIsDropped(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID
	inWorld(fake, "https://example.org", 7)
	panelWorldCreated(fake, cdpOf(driver, id), 70)

	var mu sync.Mutex
	var heard []string
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() {
		_ = driver.PanelEvents(ctx, id, func(msg json.RawMessage) {
			mu.Lock()
			heard = append(heard, string(msg))
			mu.Unlock()
		})
	}()
	time.Sleep(100 * time.Millisecond)

	heardNow := func() []string {
		mu.Lock()
		defer mu.Unlock()
		return slices.Clone(heard)
	}

	binding(fake, cdpOf(driver, id), 70, `{"v":1,"kind":"say","text":"from the panel"}`)
	eventually(t, "the panel's own message reaching its subscriber", 3*time.Second, func() bool {
		return len(heardNow()) == 1
	})

	binding(fake, cdpOf(driver, id), 7, `{"v":1,"kind":"say","text":"from the page"}`)
	binding(fake, cdpOf(driver, id), 99, `{"v":1,"kind":"say","text":"from a world nobody told the driver about"}`)
	time.Sleep(200 * time.Millisecond)
	if got := heardNow(); len(got) != 1 {
		t.Errorf("a binding call from outside the panel world was delivered: %v", got)
	}

	// Still alive afterwards: the drop did not take the channel down with it.
	binding(fake, cdpOf(driver, id), 70, `{"v":1,"kind":"say","text":"again"}`)
	eventually(t, "the panel's second message reaching its subscriber", 3*time.Second, func() bool {
		return len(heardNow()) == 2
	})
}

// TestANewPanelWorldIsReplayedTheHistory. A navigation destroys the panel's world and makes another;
// the new one has to open onto the conversation as it stood, not onto an empty panel. Only a world
// named as the panel's gets it.
func TestANewPanelWorldIsReplayedTheHistory(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID
	ctx := context.Background()

	first := json.RawMessage(`{"v":1,"kind":"message","text":"first-message"}`)
	second := json.RawMessage(`{"v":1,"kind":"message","text":"second-message"}`)

	// Said before any panel world exists: it can only reach a world through the replay.
	if err := driver.PanelPush(ctx, id, first); err != nil {
		t.Fatalf("PanelPush: %v", err)
	}
	panelWorldCreated(fake, cdpOf(driver, id), 70)
	eventually(t, "the history reaching the first panel world", 3*time.Second, func() bool {
		return strings.Contains(strings.Join(pushesInto(fake, 70), "\n"), "first-message")
	})

	// Said while a world is live: it goes into that world as it is said.
	if err := driver.PanelPush(ctx, id, second); err != nil {
		t.Fatalf("PanelPush: %v", err)
	}
	eventually(t, "a message reaching the live panel world", 3*time.Second, func() bool {
		return strings.Contains(strings.Join(pushesInto(fake, 70), "\n"), "second-message")
	})

	// The navigation: the old world is gone, a new one is born.
	fake.Emit(string(cdpOf(driver, id)), "Runtime.executionContextsCleared", map[string]any{})
	inWorld(fake, "https://example.org", 7)
	panelWorldCreated(fake, cdpOf(driver, id), 71)
	eventually(t, "the whole history reaching the new panel world", 3*time.Second, func() bool {
		joined := strings.Join(pushesInto(fake, 71), "\n")
		return strings.Contains(joined, "first-message") && strings.Contains(joined, "second-message")
	})
	joined := strings.Join(pushesInto(fake, 71), "\n")
	if strings.Index(joined, "first-message") > strings.Index(joined, "second-message") {
		t.Errorf("the history was replayed out of order: %q", joined)
	}
	if got := pushesInto(fake, 7); len(got) != 0 {
		t.Errorf("the page's own world was pushed the panel's messages: %v", got)
	}
}

// TestPanelStateFollowsBeginAndEndPerson. The panel shows who holds the wheel, and it learns from the
// driver, not from the page: human after a successful BeginPerson, agent once EndPerson is done.
func TestPanelStateFollowsBeginAndEndPerson(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID
	panelWorldCreated(fake, cdpOf(driver, id), 70)
	eventually(t, "the panel's opening state", 3*time.Second, func() bool {
		return len(pushesInto(fake, 70)) > 0
	})
	if joined := strings.Join(pushesInto(fake, 70), "\n"); !strings.Contains(joined, `"kind":"state"`) || !strings.Contains(joined, `"mode":"agent"`) {
		t.Fatalf("a new panel world was not told the state of an agent-driven session: %q", joined)
	}

	before := len(pushesInto(fake, 70))
	beginPerson(t, driver, id)
	eventually(t, "the panel hearing that a person holds the wheel", 3*time.Second, func() bool {
		pushes := pushesInto(fake, 70)
		return len(pushes) > before && strings.Contains(pushes[len(pushes)-1], `"mode":"human"`)
	})

	before = len(pushesInto(fake, 70))
	endPerson(t, driver, id)
	pushes := pushesInto(fake, 70)
	if len(pushes) <= before {
		t.Fatalf("EndPerson returned and the panel was not told: %v", pushes)
	}
	if last := pushes[len(pushes)-1]; !strings.Contains(last, `"kind":"state"`) || !strings.Contains(last, `"mode":"agent"`) {
		t.Errorf("the last state the panel heard after EndPerson is not agent: %q", last)
	}
}

// TestLastTargetDestroyedEmitsPersonClosed. A person who closes the window has ended the panel, and
// the subscribers are told that is why. A popup closing is not the window closing; and a session the
// agent closes ends it as "closed", which is not the person's doing.
func TestLastTargetDestroyedEmitsPersonClosed(t *testing.T) {
	fake, driver, _ := visiblePersonDriver(t)
	id := opened(t, driver).ID

	ended := make(chan error, 1)
	go func() { ended <- driver.PanelEvents(context.Background(), id, func(json.RawMessage) {}) }()
	time.Sleep(100 * time.Millisecond)

	// Not the session's main target: nothing ends.
	fake.Emit("", "Target.targetDestroyed", map[string]any{"targetId": "TPOP"})
	select {
	case err := <-ended:
		t.Fatalf("a popup going away ended the panel: %v", err)
	case <-time.After(200 * time.Millisecond):
	}

	fake.Emit("", "Target.targetDestroyed", map[string]any{"targetId": "T" + strings.TrimPrefix(string(cdpOf(driver, id)), "S")})
	select {
	case err := <-ended:
		var closed browser.PanelClosed
		if !errors.As(err, &closed) {
			t.Fatalf("PanelEvents ended with %v, want a PanelClosed", err)
		}
		if string(closed.Reason) != "person-closed" {
			t.Errorf("reason = %q, want person-closed", closed.Reason)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("the main target was destroyed and PanelEvents never returned")
	}

	// The other ending: the session is closed from this side.
	_, driver2, _ := visiblePersonDriver(t)
	id2 := opened(t, driver2).ID
	ended2 := make(chan error, 1)
	go func() { ended2 <- driver2.PanelEvents(context.Background(), id2, func(json.RawMessage) {}) }()
	time.Sleep(100 * time.Millisecond)
	if err := driver2.Close(context.Background(), id2); err != nil {
		t.Fatalf("Close: %v", err)
	}
	select {
	case err := <-ended2:
		var closed browser.PanelClosed
		if !errors.As(err, &closed) {
			t.Fatalf("PanelEvents ended with %v, want a PanelClosed", err)
		}
		if string(closed.Reason) != "closed" {
			t.Errorf("reason = %q, want closed", closed.Reason)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("the session was closed and PanelEvents never returned")
	}
}
