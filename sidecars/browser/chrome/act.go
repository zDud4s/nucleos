package chrome

import (
	"context"
	"encoding/json"
	"fmt"

	"nucleosbrowser/browser"
)

// Act performs one action against a ref from the last snapshot.
//
// Refusals from the fence are values, not errors (see browser.ActResult). This function returns
// errors only for things that are actually broken; "you may not do that" arrives as a refusal so the
// agent can read it and carry on.
func (d *Driver) Act(ctx context.Context, id browser.SessionID, action browser.Action) (browser.ActResult, error) {
	entry, err := d.lookup(id)
	if err != nil {
		return browser.ActResult{}, err
	}

	d.mu.Lock()
	backendNodeID, known := entry.refs[action.Ref]
	d.mu.Unlock()
	if !known {
		// Not an error: the agent named something no snapshot showed it. That is exactly the case
		// refs exist to make representable, so it comes back as a refusal it can act on — most
		// often by taking a fresh snapshot, because the page moved underneath it.
		return browser.Refused(
			browser.ConsequenceOffAllowlist,
			fmt.Sprintf("ref %q is not in the current snapshot; take a new one", action.Ref),
		), nil
	}

	objectID, err := d.resolve(ctx, entry, backendNodeID)
	if err != nil {
		return browser.ActResult{}, err
	}

	switch action.Kind {
	case browser.ActionClick:
		return d.callOn(ctx, entry, objectID, "function() { this.click(); }")
	case browser.ActionScroll:
		return d.callOn(ctx, entry, objectID,
			"function() { this.scrollIntoView({block: 'center'}); }")
	case browser.ActionType:
		if _, err := d.callOn(ctx, entry, objectID, "function() { this.focus(); }"); err != nil {
			return browser.ActResult{}, err
		}
		// Input.insertText rather than synthesising key events: it is what a paste does, it does
		// not need a keymap, and it cannot accidentally send a modifier combination.
		if _, err := d.conn.Call(ctx, entry.cdp, "Input.insertText", map[string]any{
			"text": action.Text,
		}); err != nil {
			return browser.ActResult{}, err
		}
		return browser.Done(), nil
	default:
		return browser.Refused(
			browser.ConsequenceMethod,
			fmt.Sprintf("unknown action kind %q", action.Kind),
		), nil
	}
}

func (d *Driver) resolve(ctx context.Context, entry *session, backendNodeID int64) (string, error) {
	result, err := d.conn.Call(ctx, entry.cdp, "DOM.resolveNode", map[string]any{
		"backendNodeId": backendNodeID,
	})
	if err != nil {
		return "", fmt.Errorf("resolving the node behind the ref: %w", err)
	}
	var payload struct {
		Object struct {
			ObjectID string `json:"objectId"`
		} `json:"object"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return "", err
	}
	if payload.Object.ObjectID == "" {
		return "", fmt.Errorf("chrome: the node behind the ref is gone")
	}
	return payload.Object.ObjectID, nil
}

func (d *Driver) callOn(ctx context.Context, entry *session, objectID, function string) (browser.ActResult, error) {
	if _, err := d.conn.Call(ctx, entry.cdp, "Runtime.callFunctionOn", map[string]any{
		"objectId":            objectID,
		"functionDeclaration": function,
		"awaitPromise":        true,
	}); err != nil {
		return browser.ActResult{}, err
	}
	return browser.Done(), nil
}
