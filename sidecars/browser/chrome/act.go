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
// reports that instead of "done" (spec §6.2: "o act que o causou responde ao agente").
//
// # The contract is "this act or the next", and it is not a weaker promise by accident
//
// A click and the request it causes are not synchronous, so the only thing available is a window —
// and the gate measured a form submission missing a 1.5s one under load. A longer window would still
// be a guess, so the guarantee is elsewhere: a refusal past the window is carried on the session's
// cursor and reported by the following act (see session.reportedUpTo). The agent therefore learns
// late rather than never, and "never" is the failure that matters — it would leave the agent
// reasoning about a page that never changed.
func (d *Driver) Act(ctx context.Context, id browser.SessionID, action browser.Action) (browser.ActResult, error) {
	entry, err := d.lookup(id)
	if err != nil {
		return browser.ActResult{}, err
	}

	d.mu.Lock()
	mode := entry.mode
	backendNodeID, known := entry.refs[action.Ref]
	d.mu.Unlock()

	// Spec §4.4 rule 1. Refused and not queued, and refused from the moment the wheel was ASKED for
	// rather than from the window opening — the two are separated by a process swap (§4.2), and an
	// act that landed in between would touch a page the person is about to be handed.
	//
	// Second layer. The núcleo refuses this too, from its own record of the session, and neither
	// layer is redundant: this one holds even if the núcleo's row and the browser disagree about who
	// is driving, which is exactly the state a crash between the two produces.
	if mode != browser.ModeAgent {
		return browser.Refused(
			browser.ConsequenceWheelRequested,
			"the wheel has been asked for; this session is the person's now",
		), nil
	}

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

	d.mu.Lock()
	before := entry.reportedUpTo
	d.mu.Unlock()

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

	refused, consumed := d.refusalForAt(ctx, id, before)
	d.mu.Lock()
	entry.reportedUpTo = consumed
	d.mu.Unlock()
	if refused != nil {
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
