package chrome

import (
	"context"
	"encoding/json"
	"fmt"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// This file makes a click a CLICK, and it replaced one line: `this.click()`.
//
// # What that line did not do
//
// It fired one synthetic `click` event on the element and nothing else. No `mousemove`, no
// `mouseover`, no `pointerdown`, no `mousedown`, no `mouseup` — and `isTrusted` false on the one
// event it did fire. The asymmetry is the tell: `press` has always gone through
// Input.dispatchKeyEvent and `type` through Input.insertText, so the keyboard drove the browser's
// own input pipeline and the mouse never touched it.
//
// The damage is not exotic. A great many modern components — menus, dropdowns, popovers, anything
// built on the common headless UI libraries — open on `pointerdown` or `mousedown`, because that is
// what makes them feel immediate. Under `this.click()` none of them opened. The act reported DONE,
// the reading afterwards was perfectly CORRECT, and it showed the menu closed. That is this pillar's
// whole failure shape, sitting in the acting half instead of the reading half: an agent concluding
// the button is broken, with nothing anywhere to contradict it.
//
// Hover comes back with it and without a new verb. Moving the pointer to the element before pressing
// fires mouseover and mouseenter, so a menu that opens on hover is open by the time the press lands,
// and its items are in the next reading. A separate `hover` verb would be the obvious way to get
// that and is not needed for it.
//
// # Why it can now refuse
//
// `this.click()` worked on things a person could not click: an element of zero size, an element
// behind a cookie banner, an element scrolled somewhere the viewport is not. It reported success for
// all of them. A real mouse event lands at a POINT, so the point has to be found, which means the
// two failures become visible and can be said out loud instead of being reported as done.
//
// Being told "the consent banner is on top of it" is worth more than a click that silently missed:
// the agent can dismiss the banner. Being told nothing is what produced the loop where it clicks the
// same button five times.
type aim struct {
	X       float64 `json:"x"`
	Y       float64 `json:"y"`
	Sized   bool    `json:"sized"`
	Reached bool    `json:"reached"`
	OnTop   string  `json:"on_top"`
}

// aimQuestion brings the element into view and reports where a click would land and what it would
// hit. A raw string, so there is no backtick anywhere inside it.
//
// `behavior: 'instant'` is load-bearing rather than tidy: the default respects a page's
// `scroll-behavior: smooth`, and a rectangle measured while the page is still gliding is a
// rectangle for somewhere the element is about to stop being.
//
// The reachability walk climbs parents AND shadow hosts. elementFromPoint returns the host for
// anything inside a shadow root, so a plain `contains` check would call every web component on the
// page obscured — a refusal storm on pages that are working perfectly.
const aimQuestion = `function() {
  this.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'});
  const box = this.getBoundingClientRect();
  const sized = box.width > 0 && box.height > 0;
  const x = box.left + box.width / 2;
  const y = box.top + box.height / 2;
  const describe = (el) => {
    if (!el) { return 'nothing at all'; }
    const said = (el.getAttribute('aria-label') || el.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 60);
    return el.tagName.toLowerCase() + (said ? ' "' + said + '"' : '');
  };
  const within = (hit) => {
    if (!hit) { return false; }
    if (this.contains(hit) || hit.contains(this)) { return true; }
    let node = hit;
    while (node) {
      if (node === this) { return true; }
      const root = node.getRootNode ? node.getRootNode() : null;
      node = node.parentNode || (root && root.host) || null;
    }
    return false;
  };
  const hit = sized ? document.elementFromPoint(x, y) : null;
  const reached = within(hit);
  return JSON.stringify({
    x: x, y: y, sized: sized, reached: reached,
    on_top: reached ? '' : describe(hit),
  });
}`

// click presses the mouse where the element actually is.
func (d *Driver) click(ctx context.Context, on cdp.SessionID, objectID string) (*browser.Refusal, error) {
	answer, err := d.callOnValue(ctx, on, objectID, aimQuestion, "")
	if err != nil {
		return nil, err
	}
	var where aim
	if err := json.Unmarshal([]byte(answer), &where); err != nil {
		return nil, fmt.Errorf("asking the page where the element is: %w", err)
	}

	if !where.Sized {
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail: "that element has no size on the page, so there is nowhere to click it; it is" +
				" hidden or collapsed, and something has to open it first",
		}, nil
	}
	if !where.Reached {
		return &browser.Refusal{
			Consequence: browser.ConsequenceNotApplicable,
			Detail: fmt.Sprintf("%s is on top of it, so a click there would hit that instead;"+
				" deal with what is covering it first", where.OnTop),
		}, nil
	}

	// Three events and in this order, because that is the sequence a page listens for. The move is
	// not ceremony: it is what fires mouseover and mouseenter, and it is the whole reason a hover
	// menu is open by the time the press arrives.
	for _, event := range []map[string]any{
		{"type": "mouseMoved", "x": where.X, "y": where.Y, "button": "none", "buttons": 0},
		{"type": "mousePressed", "x": where.X, "y": where.Y, "button": "left", "buttons": 1, "clickCount": 1},
		{"type": "mouseReleased", "x": where.X, "y": where.Y, "button": "left", "buttons": 0, "clickCount": 1},
	} {
		if _, err := d.conn.Call(ctx, on, "Input.dispatchMouseEvent", event); err != nil {
			return nil, fmt.Errorf("clicking: %w", err)
		}
	}
	return nil, nil
}
