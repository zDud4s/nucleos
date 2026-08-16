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
//
// # Why the fence is consulted after the action and not before
//
// The action itself is always allowed: clicking is not what has a consequence, the request the click
// causes is. So Act does the thing, then asks whether the fence stopped anything on the way out, and
// reports that instead of "done" (spec §6.2: "o act que o causou responde ao agente"). The cost is
// [refusalSettle] on every act that is not refused, which is charged in the open rather than traded
// away — telling the agent "done" for a click the fence swallowed leaves it reasoning about a page
// that never changed.
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

	before := d.refusalCount()

	switch action.Kind {
	case browser.ActionClick:
		err = d.callOn(ctx, entry, objectID, "function() { this.click(); }")
	case browser.ActionScroll:
		err = d.callOn(ctx, entry, objectID, "function() { this.scrollIntoView({block: 'center'}); }")
	case browser.ActionType:
		err = d.typeInto(ctx, entry, objectID, action.Text)
	default:
		return browser.Refused(
			browser.ConsequenceMethod,
			fmt.Sprintf("unknown action kind %q", action.Kind),
		), nil
	}
	if err != nil {
		return browser.ActResult{}, err
	}

	if refused := d.refusalFor(ctx, id, before); refused != nil {
		return browser.Refused(refused.Consequence, refused.Detail), nil
	}
	return browser.Done(), nil
}

func (d *Driver) typeInto(ctx context.Context, entry *session, objectID, text string) error {
	if err := d.callOn(ctx, entry, objectID, "function() { this.focus(); }"); err != nil {
		return err
	}
	// Input.insertText rather than synthesising key events: it is what a paste does, it does not
	// need a keymap, and it cannot accidentally send a modifier combination.
	_, err := d.conn.Call(ctx, entry.cdp, "Input.insertText", map[string]any{"text": text})
	return err
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

func (d *Driver) callOn(ctx context.Context, entry *session, objectID, function string) error {
	_, err := d.conn.Call(ctx, entry.cdp, "Runtime.callFunctionOn", map[string]any{
		"objectId":            objectID,
		"functionDeclaration": function,
		"awaitPromise":        true,
	})
	return err
}
