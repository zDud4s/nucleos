// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"sort"
	"strings"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// The verbs that are not a click.
//
// The snapshot has always handed out refs for `combobox`, `listbox`, `switch` and `slider`, and the
// driver has always had three verbs, none of which operates any of them. Showing an agent a control
// it cannot work is worse than hiding it: it will try, and read whatever the page does next as the
// thing it asked for. These are the verbs that close that gap, plus the one that gets a page back
// after a wrong turn.
//
// All of them are still consequence-free in the sense of spec §6.2 — every one is something a person
// does with a mouse and a keyboard, and none of them, on its own, sends anything anywhere. The fence
// still answers for what the page does about it.

// keystroke is one key as CDP wants it spelled.
type keystroke struct {
	key  string
	code string
	vk   int
	// text is what the key inserts, for the keys that insert something. Enter carries a carriage
	// return and Tab a tab, and a form that submits on Enter needs the key event to carry it.
	text string
}

// pressable is the closed set of keys, and it carries NO modifiers.
//
// Deliberately: Ctrl+S is a download and Ctrl+P is a dialog, and neither survives the "nothing an
// act does can leave this machine" rule that the whole tool classification rests on. What is left is
// the vocabulary a form needs — submit, move, correct, dismiss — which is what the gap was actually
// about.
var pressable = map[string]keystroke{
	"enter":      {key: "Enter", code: "Enter", vk: 13, text: "\r"},
	"tab":        {key: "Tab", code: "Tab", vk: 9, text: "\t"},
	"escape":     {key: "Escape", code: "Escape", vk: 27},
	"backspace":  {key: "Backspace", code: "Backspace", vk: 8},
	"delete":     {key: "Delete", code: "Delete", vk: 46},
	"arrowup":    {key: "ArrowUp", code: "ArrowUp", vk: 38},
	"arrowdown":  {key: "ArrowDown", code: "ArrowDown", vk: 40},
	"arrowleft":  {key: "ArrowLeft", code: "ArrowLeft", vk: 37},
	"arrowright": {key: "ArrowRight", code: "ArrowRight", vk: 39},
	"home":       {key: "Home", code: "Home", vk: 36},
	"end":        {key: "End", code: "End", vk: 35},
	"pageup":     {key: "PageUp", code: "PageUp", vk: 33},
	"pagedown":   {key: "PageDown", code: "PageDown", vk: 34},
}

// pressableNames lists the set for a refusal to quote, so an agent that guessed a key name is told
// what it could have said instead of only that it was wrong.
func pressableNames() string {
	names := make([]string, 0, len(pressable))
	for _, stroke := range pressable {
		names = append(names, stroke.key)
	}
	sort.Strings(names)
	return strings.Join(names, ", ")
}

// press sends one key, to the focused element or to a named one.
func (d *Driver) press(ctx context.Context, entry *session, on cdp.SessionID, objectID, name string) (*browser.Refusal, error) {
	stroke, ok := pressable[strings.ToLower(strings.TrimSpace(name))]
	if !ok {
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail:      fmt.Sprintf("no key called %q; this browser presses one of: %s", name, pressableNames()),
		}, nil
	}

	if objectID != "" {
		if err := d.callOn(ctx, on, objectID, "function() { this.focus(); }"); err != nil {
			return nil, err
		}
	}

	// After the focus step and before any key: the keys go to whatever has focus now.
	if refusal := d.panelFocusGuard(ctx, entry, browser.ActionPress, on); refusal != nil {
		return refusal, nil
	}

	down := map[string]any{
		"type":                  "keyDown",
		"key":                   stroke.key,
		"code":                  stroke.code,
		"windowsVirtualKeyCode": stroke.vk,
		"nativeVirtualKeyCode":  stroke.vk,
	}
	if stroke.text != "" {
		down["text"] = stroke.text
	} else {
		// rawKeyDown for the keys that insert nothing. A keyDown with no text still gets a `keypress`
		// synthesised by some builds, which is a character a page did not receive from a person.
		down["type"] = "rawKeyDown"
	}
	if _, err := d.conn.Call(ctx, on, "Input.dispatchKeyEvent", down); err != nil {
		return nil, err
	}
	_, err := d.conn.Call(ctx, on, "Input.dispatchKeyEvent", map[string]any{
		"type":                  "keyUp",
		"key":                   stroke.key,
		"code":                  stroke.code,
		"windowsVirtualKeyCode": stroke.vk,
		"nativeVirtualKeyCode":  stroke.vk,
	})
	return nil, err
}

// chooseScript picks an option by what a person would call it, and tells the caller what happened.
//
// It reports the options it found when it cannot match, because "no such option" alone leaves the
// agent to guess a second time from the same information that produced the first guess. It matches
// on the label, the trimmed text and the value, since a snapshot shows one of those and the page
// decides which.
const chooseScript = `function(want) {
	if (this.tagName !== 'SELECT') { return 'not-a-select:' + this.tagName.toLowerCase(); }
	const options = Array.from(this.options);
	const found = options.find(o =>
		o.label === want || o.text.trim() === want || o.value === want);
	if (!found) {
		return 'no-such-option:' + options.map(o => o.text.trim()).filter(Boolean).join(' | ');
	}
	this.value = found.value;
	this.dispatchEvent(new Event('input', {bubbles: true}));
	this.dispatchEvent(new Event('change', {bubbles: true}));
	return 'ok';
}`

// choose sets a dropdown to one of its options.
//
// It sets the value and fires input and change rather than clicking the control open, because a
// native dropdown's list is drawn by the operating system and there is nothing in a headless page to
// click. An ARIA dropdown built out of divs is not a SELECT and is refused as such — clicking those
// open and picking from the list they reveal is what the ordinary click verb is for, and pretending
// otherwise here would silently do nothing on half the pages that have one.
func (d *Driver) choose(ctx context.Context, on cdp.SessionID, objectID, want string) (*browser.Refusal, error) {
	outcome, err := d.callOnValue(ctx, on, objectID, chooseScript, want)
	if err != nil {
		return nil, err
	}
	switch {
	case outcome == "ok":
		return nil, nil
	case strings.HasPrefix(outcome, "not-a-select:"):
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail: fmt.Sprintf("that is a <%s>, not a dropdown; open it with a click and pick from what appears",
				strings.TrimPrefix(outcome, "not-a-select:")),
		}, nil
	case strings.HasPrefix(outcome, "no-such-option:"):
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail: fmt.Sprintf("no option called %q; it offers: %s",
				want, strings.TrimPrefix(outcome, "no-such-option:")),
		}, nil
	default:
		return nil, fmt.Errorf("chrome: the page answered %q to a select", outcome)
	}
}

// scrollPage moves the whole page, for the content that is not there until it is scrolled to.
//
// Distinct from scrolling an element into view, which needs a ref and therefore needs the thing to
// already be in a snapshot. This is how an agent reaches a list that loads as it goes, which by
// definition is not in one yet.
func (d *Driver) scrollPage(ctx context.Context, on cdp.SessionID, direction string) (*browser.Refusal, error) {
	var expression string
	switch strings.ToLower(strings.TrimSpace(direction)) {
	case "", "down":
		expression = "window.scrollBy(0, Math.round(window.innerHeight * 0.9))"
	case "up":
		expression = "window.scrollBy(0, -Math.round(window.innerHeight * 0.9))"
	case "top":
		expression = "window.scrollTo(0, 0)"
	case "bottom":
		expression = "window.scrollTo(0, document.documentElement.scrollHeight)"
	default:
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail:      fmt.Sprintf("no direction called %q; a page scrolls down, up, top or bottom", direction),
		}, nil
	}
	_, err := d.conn.Call(ctx, on, "Runtime.evaluate", map[string]any{
		"expression":    expression,
		"returnByValue": true,
	})
	return nil, err
}

// goBack returns to the previous page in this session's history.
//
// The navigation goes through the fence like any other, so this grants nothing: every entry in the
// history is a document the fence already admitted for this profile. What it buys is that a wrong
// link is recoverable at all — the alternative was re-opening a url the agent may not have kept, on
// a profile that may no longer admit it.
func (d *Driver) goBack(ctx context.Context, entry *session) (*browser.Refusal, error) {
	d.mu.Lock()
	on := entry.cdp
	d.mu.Unlock()

	result, err := d.conn.Call(ctx, on, "Page.getNavigationHistory", nil)
	if err != nil {
		return nil, fmt.Errorf("reading the history: %w", err)
	}
	var history struct {
		CurrentIndex int `json:"currentIndex"`
		Entries      []struct {
			ID int64 `json:"id"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(result, &history); err != nil {
		return nil, err
	}
	if history.CurrentIndex <= 0 || history.CurrentIndex >= len(history.Entries) {
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail:      "there is nowhere to go back to; this is the first page of this session",
		}, nil
	}
	_, err = d.conn.Call(ctx, on, "Page.navigateToHistoryEntry", map[string]any{
		"entryId": history.Entries[history.CurrentIndex-1].ID,
	})
	return nil, err
}

// callOnValue runs a function on an element and reads back what it returned.
func (d *Driver) callOnValue(ctx context.Context, on cdp.SessionID, objectID, function, argument string) (string, error) {
	result, err := d.conn.Call(ctx, on, "Runtime.callFunctionOn", map[string]any{
		"objectId":            objectID,
		"functionDeclaration": function,
		"arguments":           []map[string]any{{"value": argument}},
		"returnByValue":       true,
		"awaitPromise":        true,
	})
	if err != nil {
		return "", err
	}
	var payload struct {
		Result struct {
			Value string `json:"value"`
		} `json:"result"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return "", err
	}
	return payload.Result.Value, nil
}

// goTo follows a url in the session that is already open.
//
// # The scheme check is load-bearing and is not tidiness
//
// Every other verb acts on something a snapshot showed, so the only urls that could be reached were
// ones the page itself offered. This one takes an address from the agent, and an agent's context is
// full of text a page put there. The fence answers for http and https — the interception sees those
// requests and the allowlist decides — but `file:` reads the disk and `data:` is a document out of
// thin air, and NEITHER goes through the interception. So they are refused here, before the
// navigation, rather than trusted to a layer that never sees them.
//
// # Relative urls resolve against the page
//
// Because that is how they appear in the text an agent is reading: "see /docs/setup". Resolving
// them keeps the agent from having to reassemble an origin by hand, which is the operation most
// likely to be got wrong in the direction of somebody else's host.
func (d *Driver) goTo(ctx context.Context, entry *session, raw string) (*browser.Refusal, error) {
	wanted := strings.TrimSpace(raw)
	if wanted == "" {
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail:      "goto needs a url to go to",
		}, nil
	}

	d.mu.Lock()
	on, id, here := entry.cdp, entry.id, entry.final
	d.mu.Unlock()

	parsed, err := url.Parse(wanted)
	if err != nil {
		return &browser.Refusal{
			Consequence: browser.ConsequenceScheme,
			Detail:      fmt.Sprintf("%q is not a url", wanted),
		}, nil
	}
	if !parsed.IsAbs() {
		base, baseErr := url.Parse(here)
		if baseErr != nil || !base.IsAbs() {
			return &browser.Refusal{
				Consequence: browser.ConsequenceNotApplicable,
				Detail:      fmt.Sprintf("%q is relative and this session has no page to resolve it against", wanted),
			}, nil
		}
		parsed = base.ResolveReference(parsed)
	}
	if scheme := strings.ToLower(parsed.Scheme); scheme != "http" && scheme != "https" {
		return &browser.Refusal{
			Consequence: browser.ConsequenceScheme,
			Detail:      fmt.Sprintf("%s: is not a scheme this browser follows; http and https are", scheme),
		}, nil
	}

	// Counted before, so a refusal the fence raises while the navigation is in flight belongs to
	// this act and not to the next one.
	before := d.refusalCount()

	result, err := d.conn.Call(ctx, on, "Page.navigate", map[string]any{"url": parsed.String()})
	if err != nil {
		return nil, fmt.Errorf("navigating: %w", err)
	}
	var outcome struct {
		ErrorText string `json:"errorText"`
	}
	if err := json.Unmarshal(result, &outcome); err != nil {
		return nil, err
	}
	if outcome.ErrorText != "" {
		if d.refusalFor(ctx, id, before) != nil {
			// The fence stopped it, and Act reports that in the fence's own vocabulary. Saying
			// nothing here is what lets the one answer through rather than two.
			return nil, nil
		}
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail:      fmt.Sprintf("%s did not load: %s", parsed.String(), outcome.ErrorText),
		}, nil
	}
	return nil, nil
}
