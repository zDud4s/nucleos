// §spec browser-volante

package chrome

import (
	"context"
	"errors"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// The /input group. A person's keystrokes and pointer moves are applied to their page one CDP command
// each, in the order they were sent, and only while the person holds the session.

// inputCalls is every Input.* call the fake saw, in order.
func inputCalls(fake *cdptest.Browser) []cdptest.Call {
	var out []cdptest.Call
	for _, call := range fake.Calls() {
		if strings.HasPrefix(call.Method, "Input.") {
			out = append(out, call)
		}
	}
	return out
}

// TestInputTranslatesEachEventToItsCDPCommandInOrder. One event is one command, on the person's page,
// with the fields the contract names, and the batch order is the call order.
func TestInputTranslatesEachEventToItsCDPCommandInOrder(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	page := string(cdpOf(driver, id))

	events := []browser.InputEvent{
		{Kind: "mouse", Type: "mouseMoved", X: 10, Y: 20},
		{Kind: "mouse", Type: "mousePressed", X: 10, Y: 20, Button: "left", Buttons: 1, ClickCount: 1, Modifiers: 2},
		{Kind: "mouse", Type: "mouseReleased", X: 10, Y: 20, Button: "left", ClickCount: 1},
		{Kind: "wheel", X: 30, Y: 40, DX: 5, DY: -120},
		{Kind: "key", Type: "keyDown", Key: "a", Code: "KeyA", Text: "a", Modifiers: 8},
		{Kind: "key", Type: "keyUp", Key: "a", Code: "KeyA"},
		{Kind: "text", Value: "héllo"},
	}
	if err := driver.Input(context.Background(), id, events); err != nil {
		t.Fatalf("Input: %v", err)
	}

	calls := inputCalls(fake)
	wantMethods := []string{
		"Input.dispatchMouseEvent", "Input.dispatchMouseEvent", "Input.dispatchMouseEvent",
		"Input.dispatchMouseEvent", "Input.dispatchKeyEvent", "Input.dispatchKeyEvent", "Input.insertText",
	}
	if len(calls) != len(wantMethods) {
		t.Fatalf("got %d Input calls %v, want %d", len(calls), fake.Methods(), len(wantMethods))
	}
	for i, call := range calls {
		if call.Method != wantMethods[i] {
			t.Errorf("call %d = %s, want %s", i, call.Method, wantMethods[i])
		}
		if call.Session != page {
			t.Errorf("call %d went to session %q, want the person's page %q", i, call.Session, page)
		}
	}

	moved := paramsMap(t, calls[0])
	if moved["type"] != "mouseMoved" || moved["x"] != float64(10) || moved["y"] != float64(20) {
		t.Errorf("mouseMoved params = %v", moved)
	}
	pressed := paramsMap(t, calls[1])
	if pressed["type"] != "mousePressed" || pressed["button"] != "left" || pressed["buttons"] != float64(1) ||
		pressed["clickCount"] != float64(1) || pressed["modifiers"] != float64(2) {
		t.Errorf("mousePressed params = %v", pressed)
	}
	wheel := paramsMap(t, calls[3])
	if wheel["type"] != "mouseWheel" || wheel["x"] != float64(30) || wheel["y"] != float64(40) ||
		wheel["deltaX"] != float64(5) || wheel["deltaY"] != float64(-120) {
		t.Errorf("wheel params = %v", wheel)
	}
	down := paramsMap(t, calls[4])
	if down["type"] != "keyDown" || down["key"] != "a" || down["code"] != "KeyA" ||
		down["text"] != "a" || down["modifiers"] != float64(8) {
		t.Errorf("keyDown params = %v", down)
	}
	if up := paramsMap(t, calls[5]); up["type"] != "keyUp" || up["key"] != "a" {
		t.Errorf("keyUp params = %v", up)
	}
	if text := paramsMap(t, calls[6]); text["text"] != "héllo" {
		t.Errorf("insertText params = %v, want the event's value as text", text)
	}
}

// TestInputOutsidePersonModeIsRefused. The agent has its own verbs; /input is the person's and is
// nothing before BeginPerson or after EndPerson.
func TestInputOutsidePersonModeIsRefused(t *testing.T) {
	fake, driver, id := personSession(t)
	events := []browser.InputEvent{{Kind: "mouse", Type: "mouseMoved", X: 1, Y: 1}}

	if err := driver.Input(context.Background(), id, events); !errors.Is(err, browser.ErrNotPerson) {
		t.Fatalf("before BeginPerson: got %v, want ErrNotPerson", err)
	}
	beginPerson(t, driver, id)
	endPerson(t, driver, id)
	if err := driver.Input(context.Background(), id, events); !errors.Is(err, browser.ErrNotPerson) {
		t.Fatalf("after EndPerson: got %v, want ErrNotPerson", err)
	}
	if calls := inputCalls(fake); len(calls) != 0 {
		t.Errorf("a refused batch still sent %d Input calls", len(calls))
	}
}

// TestInputRejectsAnUnknownEventKind. The whole batch is validated before anything is sent, so a bad
// event in the middle does not leave the page with the half of it that came first.
func TestInputRejectsAnUnknownEventKind(t *testing.T) {
	cases := map[string][]browser.InputEvent{
		"unknown kind":         {{Kind: "mouse", Type: "mouseMoved"}, {Kind: "gesture"}},
		"unknown mouse type":   {{Kind: "mouse", Type: "mouseTeleported"}},
		"unknown key type":     {{Kind: "key", Type: "keyHeld", Key: "a"}},
		"empty kind":           {{}},
		"bad event after good": {{Kind: "text", Value: "x"}, {Kind: "key", Type: "nope"}},
	}
	for name, events := range cases {
		t.Run(name, func(t *testing.T) {
			fake, driver, id := personSession(t)
			beginPerson(t, driver, id)
			err := driver.Input(context.Background(), id, events)
			if !errors.Is(err, browser.ErrBadEvent) {
				t.Fatalf("got %v, want ErrBadEvent", err)
			}
			if calls := inputCalls(fake); len(calls) != 0 {
				t.Errorf("a rejected batch still sent %d Input calls: %v", len(calls), fake.Methods())
			}
		})
	}
}

// TestEndPersonDrainsTheInputInFlight. Input holds the gate for its whole batch, so EndPerson, which
// restores the fence, waits for it: nothing the person typed lands after the fence is back.
func TestEndPersonDrainsTheInputInFlight(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)
	var slowed atomic.Bool
	fake.Handle("Input.dispatchMouseEvent", func(cdptest.Call) (any, error) {
		if slowed.CompareAndSwap(false, true) {
			time.Sleep(200 * time.Millisecond)
		}
		return nil, nil
	})

	inputDone := make(chan struct{})
	go func() {
		defer close(inputDone)
		_ = driver.Input(context.Background(), id, []browser.InputEvent{
			{Kind: "mouse", Type: "mousePressed", X: 1, Y: 1, Button: "left", ClickCount: 1},
		})
	}()
	waitForCall(t, fake, "Input.dispatchMouseEvent")

	endPerson(t, driver, id)

	select {
	case <-inputDone:
	default:
		t.Fatal("EndPerson returned while the input was still in flight")
	}
	dispatch := fake.IndexOf("Input.dispatchMouseEvent")
	reloads := 0
	for i, name := range fake.Methods() {
		if name != "Page.reload" {
			continue
		}
		reloads++
		// The first reload is BeginPerson's, before the input; the next is the fence coming back.
		if reloads == 2 && i < dispatch {
			t.Errorf("EndPerson's reload (%d) came before the input's dispatch (%d): %v", i, dispatch, fake.Methods())
		}
	}
	if reloads < 2 {
		t.Fatalf("EndPerson never reloaded: %v", fake.Methods())
	}
}

// TestAKeyCarriesItsVirtualKeyCode. Chrome applies an editing key (Backspace, Enter, the arrows) only
// from a key event that names its virtual key code, and a keyDown with no text is a rawKeyDown, as
// puppeteer sends it. A key that types a character stays a keyDown.
func TestAKeyCarriesItsVirtualKeyCode(t *testing.T) {
	fake, driver, id := personSession(t)
	beginPerson(t, driver, id)

	events := []browser.InputEvent{
		{Kind: "key", Type: "keyDown", Key: "Backspace", Code: "Backspace", KeyCode: 8},
		{Kind: "key", Type: "keyDown", Key: "a", Code: "KeyA", Text: "a", KeyCode: 65},
		{Kind: "key", Type: "keyUp", Key: "Backspace", Code: "Backspace", KeyCode: 8},
	}
	if err := driver.Input(context.Background(), id, events); err != nil {
		t.Fatalf("Input: %v", err)
	}

	calls := inputCalls(fake)
	if len(calls) != 3 {
		t.Fatalf("got %d Input calls, want 3: %v", len(calls), fake.Methods())
	}
	raw := paramsMap(t, calls[0])
	if raw["type"] != "rawKeyDown" || raw["windowsVirtualKeyCode"] != float64(8) || raw["nativeVirtualKeyCode"] != float64(8) {
		t.Errorf("a text-less keyDown = %v, want rawKeyDown with windowsVirtualKeyCode 8", raw)
	}
	typed := paramsMap(t, calls[1])
	if typed["type"] != "keyDown" || typed["text"] != "a" || typed["windowsVirtualKeyCode"] != float64(65) {
		t.Errorf("a keyDown with text = %v, want keyDown carrying keyCode 65", typed)
	}
	up := paramsMap(t, calls[2])
	if up["type"] != "keyUp" || up["windowsVirtualKeyCode"] != float64(8) {
		t.Errorf("keyUp = %v, want keyUp with windowsVirtualKeyCode 8", up)
	}
}
