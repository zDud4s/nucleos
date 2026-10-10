// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

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
	d.gate.RLock()
	defer d.gate.RUnlock()
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
	if d.personHolds(id) {
		return browser.Refused(
			browser.ConsequencePersonDriving,
			"a person is driving this session; the agent has no wheel until they hand it back",
		), nil
	}
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

	// A ref is resolved when one was given, and half the verbs do not give one: back and goto name
	// no element at all, a page scroll moves what is not in a snapshot yet, and a key goes wherever
	// focus already is. `needsRef` is the list; this comment is the reason.
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

	// The panel is the person's. A key goes wherever focus is and a CDP key is trusted, so the keyboard
	// verbs are checked against it before anything is sent.
	if refusal, err := d.panelKeyboardGuard(ctx, entry, action.Kind, pageSession, objectID, key.session); err != nil {
		return browser.ActResult{}, err
	} else if refusal != nil {
		return browser.Refused(refusal.Consequence, refusal.Detail), nil
	}

	d.mu.Lock()
	before := entry.reportedUpTo
	d.mu.Unlock()

	// Watched from before the action. A click and the navigation it causes are not synchronous
	// either, which is the same fact the refusal window rests on, applied to the other thing an act
	// can do to a page.
	moved := d.watchPage(pageSession, frameID)
	defer moved.stop()
	// Stamped before the act, because the wait afterwards has to tell what this act caused from
	// what the page was already doing.
	began := time.Now()

	// Where the act lands. An element carries its own document with it, because a cross-site frame
	// is a separate target and a key dispatched at the page would arrive in the wrong one.
	on := pageSession
	if action.Ref != "" {
		on = key.session
	}

	// The write window (spec's fifth condition; see chrome/write.go). Armed before the verb and shut
	// when this function returns, so "this act caused it" is what the arrangement literally says
	// rather than a duration somebody guessed. The defer covers every path out, including the ones
	// where the verb declines — a window left open would be a permission outliving the act it
	// belonged to, which is the whole thing this is arranged to prevent.
	if opensAForm(action.Kind) {
		d.armWrite(ctx, entry, on, objectID, action)
	}
	defer d.disarmWrite(entry)

	var refusal *browser.Refusal
	var err error
	switch action.Kind {
	case browser.ActionClick:
		refusal, err = d.click(ctx, entry, on, objectID)
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
	case browser.ActionUpload:
		refusal, err = d.attach(ctx, entry, on, objectID, action)
	case browser.ActionPress:
		refusal, err = d.press(ctx, on, objectID, action.Text)
	case browser.ActionBack:
		refusal, err = d.goBack(ctx, entry)
	case browser.ActionGoto:
		refusal, err = d.goTo(ctx, entry, action.Text)
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
		// while the act was in flight, and the agent's refs are stale either way. A verb that
		// declined ran no page code, so there is nothing for it to have started.
		declined := d.afterAct(ctx, entry, moved, false, began,
			browser.Refused(refusal.Consequence, refusal.Detail))
		// Drained here too, and it is not symmetry for its own sake: a session carries writes that
		// landed after the PREVIOUS act's window closed, and an act that declines is still an act
		// the núcleo is about to file. Dropping them here would lose a record on the one path where
		// nothing else reports anything.
		declined.Writes = d.drainWrites(entry)
		return declined, nil
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
	result = d.afterAct(ctx, entry, moved, ranPageCode(action.Kind), began, result)
	// After afterAct and never before it. A form submission IS a navigation, so the record of it is
	// written while the wait for the new document is still running; draining first would report the
	// act that caused a write as having caused nothing, and hand the write to whatever act came next.
	result.Writes = d.drainWrites(entry)
	return result, nil
}

// opensAForm says which verbs may arm the write window.
//
// Click and press, and nothing else. Type, select and upload change what a form CARRIES and do not
// send it; scroll, back and goto are not acts on a control at all. The set is small because the
// window is a permission, and a permission that a verb opens by accident is one nobody granted.
//
// Upload is the one somebody will be tempted to add, because attaching a file feels like the moment
// something leaves. It is not: the file goes when the form is submitted, by a later click, and that
// click opens the window that judges it. Arming here would open a permission on an act that sends
// nothing — and close it again before the act that does.
func opensAForm(kind browser.ActionKind) bool {
	return kind == browser.ActionClick || kind == browser.ActionPress
}

// ranPageCode says whether this verb handed control to the page's own scripts.
//
// Scroll is in the list, and the reasoning that kept it out was wrong in exactly the place it
// mattered. "It moves the viewport and runs no handler" is false on any page with an infinite list:
// scrolling is THE gesture that loads more, so the one case where a scroll does something was the
// one case nothing waited for it.
//
// Back and goto are not here because they navigate, and a navigation is the other branch of
// afterAct — which waits on the load rather than on a reaction.
func ranPageCode(kind browser.ActionKind) bool {
	switch kind {
	case browser.ActionClick, browser.ActionType, browser.ActionSelect,
		browser.ActionPress, browser.ActionScroll, browser.ActionUpload:
		return true
	default:
		return false
	}
}

// needsRef says which verbs have to name an element. The other three act on the page, on the
// history, or on whatever has focus, and requiring a ref for those would mean an agent could not
// scroll to content that is not in a snapshot yet — which is the only reason to scroll.
func needsRef(kind browser.ActionKind) bool {
	switch kind {
	case browser.ActionClick, browser.ActionType, browser.ActionSelect, browser.ActionUpload:
		return true
	default:
		return false
	}
}

// afterAct says what the act did to the page, when it did anything.
//
// Two ways a page can change under an act, and for a long time only one of them was waited for. A
// navigation replaces the document, and that is handled below. An act that runs the page's own code
// and does NOT navigate is the SPA case — the click that fetches and re-renders — and it used to
// return the instant the CDP call came back, which is before the page had anything to show. See
// [Driver.awaitSettled]: the wait is a short reaction window that costs nothing when nothing
// started, and turns into a real wait when something did.
func (d *Driver) afterAct(ctx context.Context, entry *session, moved *watcher, ranCode bool, began time.Time, result browser.ActResult) browser.ActResult {
	if !moved.sawNavigation() {
		if ranCode {
			result.StillLoading = d.awaitSettled(ctx, entry, began)
		}
		return result
	}
	result.Navigated = true
	result.StillLoading = d.awaitReady(ctx, moved, entry)
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
