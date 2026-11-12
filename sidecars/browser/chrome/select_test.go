// §spec browser-volante

package chrome

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp/cdptest"
)

// The select group. A native <select> opens a popup in the real browser that a screencast never shows,
// so while the person drives, a press on one is not forwarded: it becomes a prompt the person answers
// in the shell, and the answer sets the value on the page.

// twoOptions is what the probe reports for a single select with two options.
const twoOptions = `{"multiple":false,"options":[` +
	`{"value":"one","label":"One","selected":true},{"value":"two","label":"Two","selected":false}]}`

// pageWithSelect makes the fake page answer the press probe: the node under the pointer resolves to an
// object, and the probe function (the one that looks for an enclosing select) answers with `probe`. Any
// other function run on that object answers "ok".
func pageWithSelect(fake *cdptest.Browser, probe string) {
	fake.Handle("DOM.getNodeForLocation", func(cdptest.Call) (any, error) {
		return map[string]any{"backendNodeId": 7, "frameId": "F1"}, nil
	})
	fake.Handle("DOM.resolveNode", func(cdptest.Call) (any, error) {
		return map[string]any{"object": map[string]any{"objectId": "OBJ"}}, nil
	})
	fake.Handle("Runtime.callFunctionOn", func(call cdptest.Call) (any, error) {
		if strings.Contains(string(call.Params), "closest") {
			return map[string]any{"result": map[string]any{"type": "string", "value": probe}}, nil
		}
		return map[string]any{"result": map[string]any{"type": "string", "value": "ok"}}, nil
	})
}

// mouseTypes is the type of every Input.dispatchMouseEvent the fake received, in order.
func mouseTypes(t *testing.T, fake *cdptest.Browser) []string {
	t.Helper()
	var out []string
	for _, call := range callsTo(fake, "Input.dispatchMouseEvent") {
		kind, _ := paramsMap(t, call)["type"].(string)
		out = append(out, kind)
	}
	return out
}

func pressAndRelease(x, y float64) []browser.InputEvent {
	return []browser.InputEvent{
		{Kind: "mouse", Type: "mousePressed", X: x, Y: y, Button: "left", Buttons: 1, ClickCount: 1},
		{Kind: "mouse", Type: "mouseReleased", X: x, Y: y, Button: "left", ClickCount: 1},
	}
}

// TestAMousedownOnASelectIsNotForwardedAndRaisesAPrompt. The press and its release are both held back
// from the page, and the person is asked instead, with the options the select has.
func TestAMousedownOnASelectIsNotForwardedAndRaisesAPrompt(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	pageWithSelect(fake, twoOptions)

	if err := driver.Input(context.Background(), id, pressAndRelease(30, 40)); err != nil {
		t.Fatalf("Input: %v", err)
	}

	for _, kind := range mouseTypes(t, fake) {
		if kind == "mousePressed" || kind == "mouseReleased" {
			t.Errorf("a %s reached the page although it landed on a select: the native popup would open", kind)
		}
	}
	lookup := paramsMap(t, waitForCall(t, fake, "DOM.getNodeForLocation"))
	if lookup["x"] != float64(30) || lookup["y"] != float64(40) || lookup["includeUserAgentShadowDOM"] != true {
		t.Errorf("the node was looked up with %v, want the press position and the user-agent shadow DOM", lookup)
	}

	prompt := log.openPrompt(t, "select")
	if prompt.ID == "" {
		t.Error("the prompt has no id: the answer could not name it")
	}
	if prompt.Multiple == nil || *prompt.Multiple {
		t.Errorf("multiple = %v, want an explicit false", prompt.Multiple)
	}
	if len(prompt.Options) != 2 || prompt.Options[0] != (browser.PromptOption{Value: "one", Label: "One", Selected: true}) ||
		prompt.Options[1] != (browser.PromptOption{Value: "two", Label: "Two"}) {
		t.Errorf("options = %+v, want the select's two options as the probe reported them", prompt.Options)
	}
}

// TestAMousedownElsewhereIsForwarded. A press that is not on a select is the page's: nothing is held
// back and nobody is asked.
func TestAMousedownElsewhereIsForwarded(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	pageWithSelect(fake, "")

	if err := driver.Input(context.Background(), id, pressAndRelease(30, 40)); err != nil {
		t.Fatalf("Input: %v", err)
	}

	got := mouseTypes(t, fake)
	if len(got) != 2 || got[0] != "mousePressed" || got[1] != "mouseReleased" {
		t.Errorf("mouse events = %v, want the press and the release forwarded in order", got)
	}
	time.Sleep(50 * time.Millisecond)
	for _, prompt := range log.all() {
		if prompt.Kind == "select" {
			t.Errorf("a select prompt was raised for a press that was not on a select: %+v", prompt)
		}
	}
}

// TestAnsweringASelectPromptSetsTheValueAndFiresChange. The person's pick is run on the select's own
// object, in the page that asked, and the script is the kind that fires input and change.
func TestAnsweringASelectPromptSetsTheValueAndFiresChange(t *testing.T) {
	fake, driver, id, log := personWatched(t)
	pageWithSelect(fake, twoOptions)
	on := string(cdpOf(driver, id))
	if err := driver.Input(context.Background(), id, pressAndRelease(30, 40)); err != nil {
		t.Fatalf("Input: %v", err)
	}
	prompt := log.openPrompt(t, "select")

	if err := driver.Answer(context.Background(), id, prompt.ID, answerJSON(t, map[string]any{"value": "two"})); err != nil {
		t.Fatalf("Answer: %v", err)
	}

	var pick map[string]any
	for _, call := range callsTo(fake, "Runtime.callFunctionOn") {
		if !strings.Contains(string(call.Params), "closest") {
			if call.Session != on {
				t.Errorf("the pick ran on session %q, want the page that asked, %q", call.Session, on)
			}
			pick = paramsMap(t, call)
		}
	}
	if pick == nil {
		t.Fatal("no function was run on the select to set the value")
	}
	if pick["objectId"] != "OBJ" {
		t.Errorf("the pick ran on %v, want the select's own object", pick["objectId"])
	}
	declaration, _ := pick["functionDeclaration"].(string)
	if !strings.Contains(declaration, "'input'") || !strings.Contains(declaration, "'change'") {
		t.Errorf("the pick script does not fire input and change: %s", declaration)
	}
	args, _ := pick["arguments"].([]any)
	if len(args) != 1 || args[0].(map[string]any)["value"] != "two" {
		t.Errorf("the pick carried %v, want the value two", pick["arguments"])
	}
	log.resolved(t, prompt.ID)
}
