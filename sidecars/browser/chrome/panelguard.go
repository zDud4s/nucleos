// §spec browser-com-painel

package chrome

import (
	"context"
	"encoding/json"
	"fmt"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// panelHostTag is the custom element the panel bundle mounts its closed shadow root on.
const panelHostTag = "NUCLEOS-PANEL"

// focusInPanelQuestion asks whether focus rests inside the panel. activeElement is retargeted to the
// outermost host, so the panel's host is what it answers however deep the focus is.
const focusInPanelQuestion = `(() => {
  let el = document.activeElement;
  while (el) {
    if (el.tagName === '` + panelHostTag + `') { return 'panel'; }
    el = el.parentNode || (el.getRootNode && el.getRootNode().host) || null;
  }
  return '';
})()`

// nodeInPanelQuestion asks whether the node a ref resolved to lives under the panel's host, climbing
// shadow roots to any depth.
const nodeInPanelQuestion = `function() {
  let node = this;
  while (node) {
    if (node.tagName === '` + panelHostTag + `') { return 'panel'; }
    const root = node.getRootNode ? node.getRootNode() : null;
    node = node.parentNode || (root && root.host) || null;
  }
  return '';
}`

// guardWorldName names the isolated world the guard asks in. A page's scripts run in the main world
// and cannot see or patch an isolated one, so a redefined activeElement, parentNode, getRootNode or
// tagName getter cannot make the answer "not the panel".
const guardWorldName = "nucleos-guard"

// hasPanel says whether this session has a panel to protect. Only a visible session has one, so any
// other costs no CDP call.
func (d *Driver) hasPanel(entry *session) bool {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.visible && entry.panel != nil
}

// panelRefusal is the refusal for something that would land in the panel, or that could not be told
// apart from it.
func panelRefusal(kind browser.ActionKind, detail string) *browser.Refusal {
	return &browser.Refusal{Consequence: browser.ConsequenceNotApplicable, Detail: fmt.Sprintf("%q %s", kind, detail)}
}

// askInGuardWorld puts a question to an isolated world of the page's main frame. Any failure to ask
// or to read the answer is a refusal: undecided is not allowed on a session with a panel.
func (d *Driver) askInGuardWorld(ctx context.Context, entry *session, kind browser.ActionKind, ask func(world int64) (string, error)) *browser.Refusal {
	d.mu.Lock()
	page, frame := entry.cdp, entry.frameID
	d.mu.Unlock()
	undecided := panelRefusal(kind, "was not sent: the browser could not tell whether it would land in the panel, and the panel is the person's")
	if frame == "" {
		return undecided
	}
	raw, err := d.conn.Call(ctx, page, "Page.createIsolatedWorld", map[string]any{
		"frameId":   frame,
		"worldName": guardWorldName,
	})
	if err != nil {
		return undecided
	}
	var world struct {
		ExecutionContextID int64 `json:"executionContextId"`
	}
	if err := json.Unmarshal(raw, &world); err != nil || world.ExecutionContextID == 0 {
		return undecided
	}
	inside, err := ask(world.ExecutionContextID)
	if err != nil {
		return undecided
	}
	if inside == "panel" {
		return panelRefusal(kind, "would land in the browser panel; the panel is the person's and the agent never acts on it")
	}
	return nil
}

// panelNodeGuard refuses a keyboard or value verb whose ref is a node of the panel (or under its
// host, to any depth of shadow roots). The node is resolved into an isolated world and the climb runs
// there. A ref in another target's document cannot be the panel's, which lives in the page's own.
func (d *Driver) panelNodeGuard(ctx context.Context, entry *session, kind browser.ActionKind, page cdp.SessionID, key nodeKey) *browser.Refusal {
	if kind != browser.ActionPress && kind != browser.ActionType && kind != browser.ActionSelect {
		return nil
	}
	if key.session != page || !d.hasPanel(entry) {
		return nil
	}
	return d.askInGuardWorld(ctx, entry, kind, func(world int64) (string, error) {
		raw, err := d.conn.Call(ctx, page, "DOM.resolveNode", map[string]any{
			"backendNodeId":      key.backend,
			"executionContextId": world,
		})
		if err != nil {
			return "", err
		}
		var node struct {
			Object struct {
				ObjectID string `json:"objectId"`
			} `json:"object"`
		}
		if err := json.Unmarshal(raw, &node); err != nil || node.Object.ObjectID == "" {
			return "", fmt.Errorf("chrome: the node behind the ref did not resolve in the guard world")
		}
		return d.callOnValue(ctx, page, node.Object.ObjectID, nodeInPanelQuestion, "")
	})
}

// panelFocusGuard refuses a key or text that would go to whatever has focus when focus is inside the
// panel. It must run AFTER the verb's own focus step and immediately before the dispatch: a ref to a
// node that cannot take focus leaves focus where it was, and an earlier Tab can have left it in the
// panel, so the ref's own node says nothing about where the keys land. on is the session the keys go
// to; another target's document has no panel in it.
func (d *Driver) panelFocusGuard(ctx context.Context, entry *session, kind browser.ActionKind, on cdp.SessionID) *browser.Refusal {
	d.mu.Lock()
	page := entry.cdp
	d.mu.Unlock()
	if on != page || !d.hasPanel(entry) {
		return nil
	}
	return d.askInGuardWorld(ctx, entry, kind, func(world int64) (string, error) {
		return d.evaluateString(ctx, page, focusInPanelQuestion, world)
	})
}

// evaluateString runs an expression in one execution context and returns its string result.
func (d *Driver) evaluateString(ctx context.Context, on cdp.SessionID, expression string, contextID int64) (string, error) {
	raw, err := d.conn.Call(ctx, on, "Runtime.evaluate", map[string]any{
		"expression":    expression,
		"contextId":     contextID,
		"returnByValue": true,
	})
	if err != nil {
		return "", err
	}
	var out struct {
		Result struct {
			Value string `json:"value"`
		} `json:"result"`
		Exception json.RawMessage `json:"exceptionDetails"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return "", err
	}
	if len(out.Exception) > 0 {
		return "", fmt.Errorf("chrome: the guard question threw")
	}
	return out.Result.Value, nil
}
