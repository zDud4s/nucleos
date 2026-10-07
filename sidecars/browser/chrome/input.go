// §spec browser-volante

package chrome

import (
	"context"
	"encoding/json"
	"fmt"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// Input applies a person's batch to their page, one CDP command per event, in order.
//
// It holds the gate for the whole batch, so EndPerson (which restores the fence) waits for it: nothing
// the person typed lands after the fence is back. The batch is validated first, so a bad event in the
// middle does not leave the page with the half that came before it. The first failing call stops the
// batch.
func (d *Driver) Input(ctx context.Context, id browser.SessionID, events []browser.InputEvent) error {
	d.gate.RLock()
	defer d.gate.RUnlock()
	state := d.person.Load()
	if state == nil || state.session != id {
		return browser.ErrNotPerson
	}
	for _, event := range events {
		if !validInput(event) {
			return browser.ErrBadEvent
		}
	}
	for _, event := range events {
		if d.holdBack(ctx, state, event) {
			continue
		}
		method, params := inputCommand(event)
		if _, err := d.conn.Call(ctx, state.page, method, params); err != nil {
			return err
		}
	}
	return nil
}

// validInput reports whether the event is a kind and type the contract names.
func validInput(event browser.InputEvent) bool {
	switch event.Kind {
	case "mouse":
		switch event.Type {
		case "mouseMoved", "mousePressed", "mouseReleased":
			return true
		}
	case "wheel", "text":
		return true
	case "key":
		switch event.Type {
		case "keyDown", "keyUp", "rawKeyDown", "char":
			return true
		}
	}
	return false
}

// inputCommand maps a validated event to its CDP method and params.
func inputCommand(event browser.InputEvent) (string, map[string]any) {
	switch event.Kind {
	case "mouse":
		return "Input.dispatchMouseEvent", map[string]any{
			"type": event.Type, "x": event.X, "y": event.Y, "button": mouseButton(event.Button),
			"buttons": event.Buttons, "clickCount": event.ClickCount, "modifiers": event.Modifiers,
		}
	case "wheel":
		return "Input.dispatchMouseEvent", map[string]any{
			"type": "mouseWheel", "x": event.X, "y": event.Y, "deltaX": event.DX, "deltaY": event.DY,
		}
	case "key":
		params := map[string]any{
			"type": event.Type, "key": event.Key, "code": event.Code, "text": event.Text,
			"modifiers": event.Modifiers,
		}
		// Chrome applies an editing key only when it knows the virtual key code, and a keyDown that
		// types nothing is a rawKeyDown (puppeteer's behaviour); otherwise Backspace, Enter, Delete
		// and the arrows do nothing.
		if event.KeyCode != 0 {
			params["windowsVirtualKeyCode"] = event.KeyCode
			params["nativeVirtualKeyCode"] = event.KeyCode
		}
		if event.Type == "keyDown" && event.Text == "" {
			params["type"] = "rawKeyDown"
		}
		return "Input.dispatchKeyEvent", params
	}
	return "Input.insertText", map[string]any{"text": event.Value}
}

// mouseButton defaults an unnamed button to "none", which is what CDP expects for a plain move.
func mouseButton(button string) string {
	if button == "" {
		return "none"
	}
	return button
}

// selectProbe finds the select a press landed on, if any, and reports its options. A text node's
// enclosing element is looked at, and an element that is not inside a select answers "".
const selectProbe = `function() {
  const el = this.nodeType === 1 ? this : this.parentElement;
  const select = el && el.closest ? el.closest('select') : null;
  if (!select) { return ''; }
  const options = Array.from(select.options).slice(0, 500).map(o => ({
    value: o.value, label: (o.label || o.text || '').trim(), selected: o.selected,
  }));
  return JSON.stringify({multiple: select.multiple, options: options});
}`

// selectPick sets a select to the option with the wanted value. A multiple select toggles that option
// and a single one takes it; either way the page is told the way a person's pick would tell it.
const selectPick = `function(want) {
  if (this.tagName !== 'SELECT') { return 'not-a-select'; }
  const option = Array.from(this.options).find(o => o.value === want);
  if (!option) { return 'no-such-option'; }
  if (this.multiple) { option.selected = !option.selected; } else { this.value = option.value; }
  this.dispatchEvent(new Event('input', {bubbles: true}));
  this.dispatchEvent(new Event('change', {bubbles: true}));
  return 'ok';
}`

// selectTarget is what a select prompt needs to answer its page: the select's own object.
type selectTarget struct {
	objectID string
}

// holdBack reports whether the event is not to be forwarded because it belongs to a select.
//
// A press on a select is answered by a prompt instead, since the native popup it would open is drawn
// by the operating system and never reaches the screencast. The release that follows is held back
// too, or the page would see a mouseup with no mousedown. Looking is best effort: a probe that fails
// forwards the press as usual.
func (d *Driver) holdBack(ctx context.Context, state *personState, event browser.InputEvent) bool {
	if event.Kind != "mouse" {
		return false
	}
	switch event.Type {
	case "mouseReleased":
		state.selMu.Lock()
		defer state.selMu.Unlock()
		if state.heldButton != "" && state.heldButton == mouseButton(event.Button) {
			state.heldButton = ""
			return true
		}
	case "mousePressed":
		if !d.pressOnSelect(ctx, state, event) {
			return false
		}
		state.selMu.Lock()
		state.heldButton = mouseButton(event.Button)
		state.selMu.Unlock()
		return true
	}
	return false
}

// pressOnSelect probes the node under a press and, when it is inside a select, files the prompt.
func (d *Driver) pressOnSelect(ctx context.Context, state *personState, event browser.InputEvent) bool {
	located, err := d.conn.Call(ctx, state.page, "DOM.getNodeForLocation", map[string]any{
		"x": event.X, "y": event.Y, "includeUserAgentShadowDOM": true,
	})
	if err != nil {
		return false
	}
	var node struct {
		BackendNodeID int `json:"backendNodeId"`
	}
	if json.Unmarshal(located, &node) != nil || node.BackendNodeID == 0 {
		return false
	}
	resolved, err := d.conn.Call(ctx, state.page, "DOM.resolveNode", map[string]any{"backendNodeId": node.BackendNodeID})
	if err != nil {
		return false
	}
	var object struct {
		Object struct {
			ObjectID string `json:"objectId"`
		} `json:"object"`
	}
	if json.Unmarshal(resolved, &object) != nil || object.Object.ObjectID == "" {
		return false
	}
	objectID := object.Object.ObjectID

	answer, err := d.callOnValue(ctx, state.page, objectID, selectProbe, "")
	var found struct {
		Multiple bool                   `json:"multiple"`
		Options  []browser.PromptOption `json:"options"`
	}
	if err != nil || answer == "" || json.Unmarshal([]byte(answer), &found) != nil {
		d.releaseObject(ctx, state.page, objectID)
		return false
	}
	multiple := found.Multiple
	prompt := browser.Prompt{Kind: "select", Multiple: &multiple, Options: found.Options}
	if !d.raisePromptFor(state, state.page, prompt, nil, "", selectTarget{objectID: objectID}) {
		d.releaseObject(ctx, state.page, objectID)
		return false
	}
	return true
}

// releaseObject lets the page drop an object nobody will use again. Best effort.
func (d *Driver) releaseObject(ctx context.Context, on cdp.SessionID, objectID string) {
	_, _ = d.conn.Call(ctx, on, "Runtime.releaseObject", map[string]any{"objectId": objectID})
}

// answerSelect applies a person's answer to a select prompt: a cancel sets nothing, a value must be
// one of the options the prompt offered. A bad answer leaves the prompt open.
func (d *Driver) answerSelect(ctx context.Context, state *personState, pending *pendingPrompt, promptID string, answer json.RawMessage) error {
	var reply struct {
		Cancel bool    `json:"cancel"`
		Value  *string `json:"value"`
	}
	if err := json.Unmarshal(answer, &reply); err != nil {
		return browser.ErrBadAnswer
	}
	target, ok := pending.target.(selectTarget)
	if !ok {
		return browser.ErrBadAnswer
	}
	if reply.Cancel {
		if d.takePrompt(state, promptID) == nil {
			return browser.ErrNoPrompt
		}
		d.releaseObject(ctx, pending.on, target.objectID)
		return nil
	}
	if reply.Value == nil {
		return browser.ErrBadAnswer
	}
	offered := false
	for _, option := range pending.prompt.Options {
		if option.Value == *reply.Value {
			offered = true
			break
		}
	}
	if !offered {
		return browser.ErrBadAnswer
	}
	if d.takePrompt(state, promptID) == nil {
		return browser.ErrNoPrompt
	}
	outcome, err := d.callOnValue(ctx, pending.on, target.objectID, selectPick, *reply.Value)
	if err != nil {
		return err
	}
	switch outcome {
	case "ok":
		return nil
	case "no-such-option", "not-a-select":
		return browser.ErrBadAnswer
	}
	return fmt.Errorf("chrome: the page answered %q to a select", outcome)
}
