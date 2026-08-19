package chrome

import (
	"context"
	"encoding/json"
	"fmt"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
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
	entry, lookupErr := d.lookup(id)
	if lookupErr != nil {
		return browser.ActResult{}, lookupErr
	}

	d.mu.Lock()
	mode := entry.mode
	key, known := entry.refs[action.Ref]
	pageSession, frameID := entry.cdp, entry.frameID
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

	if needsRef(action.Kind) && action.Ref == "" {
		return browser.Refused(
			browser.ConsequenceNotApplicable,
			fmt.Sprintf("%q has to name something the last snapshot showed", action.Kind),
		), nil
	}

	// A ref is resolved when one was given, and three of the six verbs do not give one: back never
	// names an element, a page scroll moves what is not in a snapshot yet, and a key goes wherever
	// focus already is.
	var objectID string
	if action.Ref != "" {
		if !known {
			// Not an error: the agent named something no snapshot showed it. That is exactly the
			// case refs exist to make representable, so it comes back as a refusal it can act on —
			// most often by taking a fresh snapshot, because the page moved underneath it.
			return browser.Refused(
				browser.ConsequenceStaleRef,
				fmt.Sprintf("ref %q is not in the current snapshot; take a new one", action.Ref),
			), nil
		}
		resolved, err := d.resolve(ctx, key)
		if err != nil {
			return browser.ActResult{}, err
		}
		objectID = resolved
	}

	d.mu.Lock()
	before := entry.reportedUpTo
	d.mu.Unlock()

	// Watched from before the action. A click and the navigation it causes are not synchronous
	// either, which is the same fact the refusal window rests on, applied to the other thing an act
	// can do to a page.
	moved := d.watchPage(pageSession, frameID)
	defer moved.stop()

	// Where the act lands. An element carries its own document with it, because a cross-site frame
	// is a separate target and a key dispatched at the page would arrive in the wrong one.
	on := pageSession
	if action.Ref != "" {
		on = key.session
	}

	var refusal *browser.Refusal
	var err error
	switch action.Kind {
	case browser.ActionClick:
		err = d.callOn(ctx, on, objectID, "function() { this.click(); }")
	case browser.ActionScroll:
		if objectID == "" {
			refusal, err = d.scrollPage(ctx, on, action.Text)
		} else {
			err = d.callOn(ctx, on, objectID, "function() { this.scrollIntoView({block: 'center'}); }")
		}
	case browser.ActionType:
		err = d.typeInto(ctx, on, objectID, action.Text)
	case browser.ActionSelect:
		refusal, err = d.choose(ctx, on, objectID, action.Text)
	case browser.ActionPress:
		refusal, err = d.press(ctx, on, objectID, action.Text)
	case browser.ActionBack:
		refusal, err = d.goBack(ctx, entry)
	default:
		return browser.Refused(
			browser.ConsequenceNotApplicable,
			fmt.Sprintf("unknown action kind %q", action.Kind),
		), nil
	}
	if err != nil {
		return browser.ActResult{}, err
	}
	if refusal != nil {
		// Still through afterAct: the verb declined, but a page can have moved for its own reasons
		// while the act was in flight, and the agent's refs are stale either way.
		return d.afterAct(ctx, entry, moved,
			browser.Refused(refusal.Consequence, refusal.Detail)), nil
	}

	refused, consumed := d.refusalForAt(ctx, id, before)
	d.mu.Lock()
	entry.reportedUpTo = consumed
	d.mu.Unlock()

	result := browser.Done()
	if refused != nil {
		result = browser.Refused(refused.Consequence, refused.Detail)
	}
	// Both, and in this order: an act can be refused AND move the page. A click that navigates and
	// also fires a blocked beacon is one act with two things worth saying about it, and reporting
	// only the first would leave the agent holding refs to a document that is gone.
	return d.afterAct(ctx, entry, moved, result), nil
}

// needsRef says which verbs have to name an element. The other three act on the page, on the
// history, or on whatever has focus, and requiring a ref for those would mean an agent could not
// scroll to content that is not in a snapshot yet — which is the only reason to scroll.
func needsRef(kind browser.ActionKind) bool {
	switch kind {
	case browser.ActionClick, browser.ActionType, browser.ActionSelect:
		return true
	default:
		return false
	}
}

// afterAct says what the act did to the page, when it did anything.
//
// The expensive half — waiting for the new page, re-reading where it landed — runs only when the
// document actually changed, so the ordinary click that opens a menu still costs one round trip.
func (d *Driver) afterAct(ctx context.Context, entry *session, moved *watcher, result browser.ActResult) browser.ActResult {
	if !moved.sawNavigation() {
		return result
	}
	result.Navigated = true
	result.StillLoading = d.awaitReady(ctx, moved)
	d.forgetRefs(entry)
	d.readTargetInfo(ctx, entry)
	d.mu.Lock()
	result.URL = entry.final
	d.mu.Unlock()
	return result
}

func (d *Driver) typeInto(ctx context.Context, on cdp.SessionID, objectID, text string) error {
	if err := d.callOn(ctx, on, objectID, "function() { this.focus(); }"); err != nil {
		return err
	}
	// Input.insertText rather than synthesising key events: it is what a paste does, it does not
	// need a keymap, and it cannot accidentally send a modifier combination.
	_, err := d.conn.Call(ctx, on, "Input.insertText", map[string]any{"text": text})
	return err
}

func (d *Driver) resolve(ctx context.Context, key nodeKey) (string, error) {
	result, err := d.conn.Call(ctx, key.session, "DOM.resolveNode", map[string]any{
		"backendNodeId": key.backend,
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

func (d *Driver) callOn(ctx context.Context, on cdp.SessionID, objectID, function string) error {
	_, err := d.conn.Call(ctx, on, "Runtime.callFunctionOn", map[string]any{
		"objectId":            objectID,
		"functionDeclaration": function,
		"awaitPromise":        true,
	})
	return err
}
