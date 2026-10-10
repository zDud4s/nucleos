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

// focusInPanelQuestion asks the main world whether focus rests inside the panel. activeElement is
// retargeted to the outermost host, so the panel's host is what it answers however deep the focus is.
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

// panelKeyboardGuard refuses a keyboard or value verb that would reach the panel: press and type and
// select act on whatever has focus or on the ref's node, and both can be the panel's. Only a visible
// session has a panel, so any other costs no CDP call.
func (d *Driver) panelKeyboardGuard(ctx context.Context, entry *session, kind browser.ActionKind, page cdp.SessionID, objectID string, refSession cdp.SessionID) (*browser.Refusal, error) {
	if kind != browser.ActionPress && kind != browser.ActionType && kind != browser.ActionSelect {
		return nil, nil
	}
	d.mu.Lock()
	has := d.visible && entry.panel != nil
	d.mu.Unlock()
	if !has {
		return nil, nil
	}
	var inside string
	var err error
	if objectID != "" {
		inside, err = d.callOnValue(ctx, refSession, objectID, nodeInPanelQuestion, "")
	} else {
		inside, err = d.evaluateString(ctx, page, focusInPanelQuestion)
	}
	if err != nil {
		return nil, err
	}
	if inside != "panel" {
		return nil, nil
	}
	return &browser.Refusal{
		Consequence: browser.ConsequenceNotApplicable,
		Detail: fmt.Sprintf("%q would land in the browser panel; the panel is the person's and the agent"+
			" never acts on it", kind),
	}, nil
}

// evaluateString runs an expression in the page's main world and returns its string result.
func (d *Driver) evaluateString(ctx context.Context, on cdp.SessionID, expression string) (string, error) {
	raw, err := d.conn.Call(ctx, on, "Runtime.evaluate", map[string]any{
		"expression":    expression,
		"returnByValue": true,
	})
	if err != nil {
		return "", err
	}
	var out struct {
		Result struct {
			Value string `json:"value"`
		} `json:"result"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return "", err
	}
	return out.Result.Value, nil
}
