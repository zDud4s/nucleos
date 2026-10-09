package chrome

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"sort"
	"strconv"
	"strings"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// The picture's bounds, and why there are any.
//
// A screenshot is one to two orders of magnitude more expensive than a reading — a snapshot is
// kilobytes of text, an unbounded PNG of a retina viewport is megabytes of base64 — and every byte
// of it is paid for out of the agent's context, once per look, forever. So: JPEG rather than PNG,
// because a page is a photograph of text and not a diagram, and the viewport rather than the whole
// document, because `captureBeyondViewport` renders a page nobody has scrolled to and produces an
// image whose height nothing bounds.
//
// There is deliberately no scaling ceiling here, and the reason is worth stating rather than
// leaving as an absence: agent mode is `--headless=new` with no `--window-size` (see launch/args.go),
// so the viewport is Chromium's headless default and every picture this produces is that size. A
// ceiling would be a second, silent bound on top of one that is already fixed — and if the launch
// ever does set a window size, Width and Height below report it, which is the honest way for that
// change to become visible.
const (
	lookMIME    = "image/jpeg"
	lookQuality = 55
	// lookLabels caps how many refs are drawn. Past this the picture is unreadable anyway, and the
	// labels start costing more to draw than the picture costs to send.
	lookLabels = 120
)

// overlayID is the element the labels live under. A fixed id, so the removal below finds it without
// guessing, and so a look that ran twice replaces its own work rather than stacking on it.
const overlayID = "nucleos-look-overlay"

// drawScript paints the labels, and its three unusual choices are all load-bearing.
//
//   - `pointer-events: none` on everything it makes. The overlay must not be able to receive a click,
//     and — the part that is easy to miss — must not change what `document.elementFromPoint` would
//     answer, because that is how the click verb decides an element is reachable. An overlay that
//     covered the page would make every subsequent click land on the label.
//   - `aria-hidden` on the host. A snapshot taken between the injection and the removal must see
//     nothing: the refs are minted from the accessibility tree, so an overlay inside it would mint
//     refs FOR THE LABELS and renumber the page the labels are naming.
//   - Position read at draw time and written as `fixed`. Every rectangle comes from
//     `getBoundingClientRect`, which is viewport-relative, and `fixed` is the one positioning scheme
//     that means the same thing — so a page that is scrolled, or one whose body is itself
//     positioned, does not shift the labels off the things they name.
//
// It draws only what is inside the viewport, because the capture below is viewport-only: a label for
// something off-screen would be a label the picture does not contain, and the agent would be told it
// can see something it cannot.
const drawScript = `function(labels, id) {
  const elements = Array.from(arguments).slice(2);
  const existing = document.getElementById(id);
  if (existing) { existing.remove(); }
  const host = document.createElement('div');
  host.id = id;
  host.setAttribute('aria-hidden', 'true');
  host.style.cssText = 'all:initial;position:fixed;left:0;top:0;width:0;height:0;z-index:2147483647;pointer-events:none;';
  const drawn = [];
  for (let i = 0; i < elements.length; i++) {
    const element = elements[i];
    if (!element || typeof element.getBoundingClientRect !== 'function') { continue; }
    const box = element.getBoundingClientRect();
    if (box.width <= 0 || box.height <= 0) { continue; }
    if (box.bottom <= 0 || box.right <= 0) { continue; }
    if (box.top >= window.innerHeight || box.left >= window.innerWidth) { continue; }
    const outline = document.createElement('div');
    outline.style.cssText = 'all:initial;position:fixed;pointer-events:none;box-sizing:border-box;'
      + 'border:2px solid #ff007f;left:' + box.left + 'px;top:' + box.top + 'px;'
      + 'width:' + box.width + 'px;height:' + box.height + 'px;';
    const tag = document.createElement('div');
    tag.textContent = labels[i];
    tag.style.cssText = 'all:initial;position:fixed;pointer-events:none;background:#ff007f;color:#ffffff;'
      + 'font:bold 12px/1.3 monospace;padding:0 3px;white-space:nowrap;'
      + 'left:' + box.left + 'px;top:' + Math.max(0, box.top - 15) + 'px;';
    host.appendChild(outline);
    host.appendChild(tag);
    drawn.push(labels[i]);
  }
  document.documentElement.appendChild(host);
  return JSON.stringify(drawn);
}`

// eraseScript takes the overlay away. Written to be safe to run against a document that never had
// one, because the removal runs on every path out of Look including the ones where the drawing
// failed halfway.
const eraseScript = `(function() {
  const host = document.getElementById('` + overlayID + `');
  if (host) { host.remove(); }
  return true;
})()`

// Look returns a picture of the page with the agent's own refs drawn on it.
//
// # Why the labels are drawn INSIDE each document rather than composed here
//
// A cross-site frame is a separate renderer process, and where Chromium has decided to draw it is
// precisely the information that isolation hides from everyone outside it. Composing the labels on
// this side would mean computing each frame's position in the page — which is the one number that
// cannot be got right from here — so instead each document draws in its own coordinates, and
// `Page.captureScreenshot` on the top target composes the result the same way it composes the
// frames themselves. The correctness comes free from the thing that made the problem.
//
// # Why the overlay is removed on every path
//
// An overlay left behind is not a cosmetic defect. It is DOM that the next snapshot reads as
// content — and the aria-hidden that keeps it out of the accessibility tree is exactly what would
// make it invisible to anyone debugging why the page grew a hundred divs. So the removal is
// deferred before the first stroke, and it runs against every document that was drawn on rather
// than against the one that failed.
func (d *Driver) Look(ctx context.Context, id browser.SessionID) (browser.LookResult, error) {
	d.gate.RLock()
	defer d.gate.RUnlock()
	entry, err := d.lookup(id)
	if err != nil {
		return browser.LookResult{}, err
	}
	if d.personHolds(id) {
		return browser.LookResult{}, browser.ErrPersonIsDriving
	}

	d.mu.Lock()
	top := entry.cdp
	byDocument := map[cdp.SessionID][]string{}
	nodes := map[string]nodeKey{}
	for ref, key := range entry.refs {
		byDocument[key.session] = append(byDocument[key.session], ref)
		nodes[ref] = key
	}
	d.mu.Unlock()

	// Sorted by the number in the ref and not by the string, so "e10" comes after "e9" rather than
	// after "e1". The order decides which refs survive the cap below, and a lexicographic cap would
	// keep an arbitrary set — the earliest-minted are the ones most likely still on the page.
	for session := range byDocument {
		sort.Slice(byDocument[session], func(a, b int) bool {
			return refNumber(byDocument[session][a]) < refNumber(byDocument[session][b])
		})
	}

	// Every document that was touched, so the erase runs even for one whose draw failed partway.
	drawnOn := make([]cdp.SessionID, 0, len(byDocument))
	defer func() {
		for _, session := range drawnOn {
			d.erase(ctx, session)
		}
	}()

	var labels []string
	budget := lookLabels
	for _, session := range documentOrder(byDocument, top) {
		if budget <= 0 {
			break
		}
		refs := byDocument[session]
		if len(refs) > budget {
			refs = refs[:budget]
		}
		drawnOn = append(drawnOn, session)
		drawn, drawErr := d.draw(ctx, session, refs, nodes)
		if drawErr != nil {
			// One frame that would not answer is not a failed look. It is a frame with no labels,
			// and the picture is still worth having: refusing the whole thing because a nested
			// document detached mid-draw would make the verb fail on exactly the busy pages it is
			// most useful on.
			continue
		}
		labels = append(labels, drawn...)
		budget -= len(drawn)
	}

	defer d.hidePanel(ctx, entry)()
	result, err := d.capture(ctx, top)
	if err != nil {
		return browser.LookResult{}, err
	}
	result.Labels = labels
	return result, nil
}

// draw resolves one document's refs into element handles and paints them in one call.
//
// One `callFunctionOn` per DOCUMENT rather than per element: the handles ride along as arguments, so
// a page with fifty controls costs fifty resolves and one draw instead of a hundred round trips. The
// resolves cannot be batched — CDP has no plural form of `DOM.resolveNode` — and a node that will
// not resolve is skipped rather than fatal, because a ref naming an element that just left the page
// is the ordinary case a snapshot exists to correct.
func (d *Driver) draw(ctx context.Context, on cdp.SessionID, refs []string, nodes map[string]nodeKey) ([]string, error) {
	arguments := []map[string]any{nil, {"value": overlayID}}
	kept := make([]string, 0, len(refs))
	var this string
	for _, ref := range refs {
		objectID, err := d.resolve(ctx, nodes[ref])
		if err != nil || objectID == "" {
			continue
		}
		if this == "" {
			this = objectID
		}
		kept = append(kept, ref)
		arguments = append(arguments, map[string]any{"objectId": objectID})
	}
	if len(kept) == 0 {
		return nil, nil
	}
	arguments[0] = map[string]any{"value": kept}

	raw, err := d.conn.Call(ctx, on, "Runtime.callFunctionOn", map[string]any{
		// Any element of this document will do as the receiver — the function never touches `this`.
		// It is here because `callFunctionOn` needs something to name an execution context, and an
		// element the caller already holds is one fewer thing to go looking for.
		"objectId":            this,
		"functionDeclaration": drawScript,
		"arguments":           arguments,
		"returnByValue":       true,
		"awaitPromise":        true,
	})
	if err != nil {
		return nil, err
	}
	var payload struct {
		Result struct {
			Value string `json:"value"`
		} `json:"result"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return nil, err
	}
	var drawn []string
	if err := json.Unmarshal([]byte(payload.Result.Value), &drawn); err != nil {
		return nil, fmt.Errorf("chrome: the page answered %q to a draw", payload.Result.Value)
	}
	return drawn, nil
}

// erase takes one document's overlay away. Errors are swallowed on purpose: this runs on the way out
// of a verb that has already produced its answer, and a frame that detached between the draw and the
// erase has removed the overlay in the only way that matters.
func (d *Driver) erase(ctx context.Context, on cdp.SessionID) {
	_, _ = d.conn.Call(ctx, on, "Runtime.evaluate", map[string]any{
		"expression":    eraseScript,
		"returnByValue": true,
	})
}

// capture takes the picture, and takes it of the VIEWPORT.
//
// `captureBeyondViewport` is deliberately not set. It renders the whole scrollable document, which
// on an infinite list is a page nobody has scrolled to and an image whose height nothing bounds —
// and the labels above are drawn only for what is on screen, so the extra pixels would be a picture
// of things the agent was told it cannot see.
func (d *Driver) capture(ctx context.Context, on cdp.SessionID) (browser.LookResult, error) {
	raw, err := d.conn.Call(ctx, on, "Page.captureScreenshot", map[string]any{
		"format":  "jpeg",
		"quality": lookQuality,
		// Chromium reduces the whole capture, labels included, so a shrunk picture is a smaller
		// picture of the same thing rather than a picture missing its annotations.
		"captureBeyondViewport": false,
	})
	if err != nil {
		return browser.LookResult{}, err
	}
	var payload struct {
		Data string `json:"data"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return browser.LookResult{}, err
	}
	bytes, err := base64.StdEncoding.DecodeString(payload.Data)
	if err != nil {
		return browser.LookResult{}, fmt.Errorf("chrome: the screenshot was not base64: %w", err)
	}
	width, height := jpegSize(bytes)
	return browser.LookResult{
		Image:  payload.Data,
		MIME:   lookMIME,
		Width:  width,
		Height: height,
	}, nil
}

// documentOrder puts the top document first and the frames after it, in a stable order.
//
// Stable because the label budget is spent in this order, so an unordered map would mean two looks
// at the same page could label different halves of it — and an agent comparing the two would be
// comparing a difference this code invented.
func documentOrder(byDocument map[cdp.SessionID][]string, top cdp.SessionID) []cdp.SessionID {
	order := make([]cdp.SessionID, 0, len(byDocument))
	if _, ok := byDocument[top]; ok {
		order = append(order, top)
	}
	rest := make([]cdp.SessionID, 0, len(byDocument))
	for session := range byDocument {
		if session != top {
			rest = append(rest, session)
		}
	}
	sort.Slice(rest, func(a, b int) bool { return rest[a] < rest[b] })
	return append(order, rest...)
}

// refNumber reads the number out of "e12". A ref that does not have that shape sorts last rather
// than erroring: the shape is minted in one place and this is a sort key, not a validation.
func refNumber(ref string) int {
	number, err := strconv.Atoi(strings.TrimPrefix(ref, "e"))
	if err != nil {
		return 1 << 30
	}
	return number
}

// jpegSize reads the picture's dimensions out of its own header.
//
// Read from the bytes rather than asked of the page, because what the result reports has to be the
// size of the IMAGE — a `window.innerWidth` would be the size of the viewport in CSS pixels, which
// on a scaled display is a different number, and reporting it would be reporting a measurement of
// something else entirely. Zero for a picture whose header does not parse, which the caller reports
// as zero rather than guessing.
func jpegSize(data []byte) (int, int) {
	for i := 2; i+9 < len(data); {
		if data[i] != 0xFF {
			i++
			continue
		}
		marker := data[i+1]
		// The SOFn markers carry the dimensions. SOF4, SOF8 and SOF12 are not frame headers.
		if marker >= 0xC0 && marker <= 0xCF && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
			height := int(data[i+5])<<8 | int(data[i+6])
			width := int(data[i+7])<<8 | int(data[i+8])
			return width, height
		}
		if marker == 0xD8 || (marker >= 0xD0 && marker <= 0xD9) {
			i += 2
			continue
		}
		i += 2 + int(data[i+2])<<8 + int(data[i+3])
	}
	return 0, 0
}

// hidePanel tells each of a visible session's panel worlds to hide before a capture, and returns the
// call that shows them again. The picture is the agent's and the panel is the person's. Best effort,
// and a no-op (no CDP call at all) for a session without a panel.
func (d *Driver) hidePanel(ctx context.Context, entry *session) func() {
	d.mu.Lock()
	visible := d.visible
	var ids []int64
	if visible && entry.panel != nil {
		for key, c := range d.contexts {
			if key.session == entry.cdp && c.name == panelWorld {
				ids = append(ids, key.id)
			}
		}
	}
	d.mu.Unlock()
	if len(ids) == 0 {
		return func() {}
	}
	say := func(hidden bool) {
		for _, id := range ids {
			_, _ = d.conn.Call(ctx, entry.cdp, "Runtime.evaluate", map[string]any{
				"expression": fmt.Sprintf("globalThis.__nucleosHide?.(%t)", hidden),
				"contextId":  id,
			})
		}
	}
	say(true)
	return func() { say(false) }
}
