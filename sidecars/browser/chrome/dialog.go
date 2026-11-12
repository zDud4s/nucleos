// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"strings"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// This file answers the questions a page puts to a person, because there is no person here.
//
// # Why it has to exist at all
//
// `alert`, `confirm`, `prompt` and `beforeunload` do not merely draw something — they BLOCK the
// renderer until the dialog is answered. Chromium normally answers it itself, but not while a CDP
// client has the Page domain enabled: then the dialog is handed to that client and the page waits.
// This driver enables Page on every target (driver.go), so the answer is ours to give and there was
// nothing here to give it.
//
// MEASURED against the pinned build, 2026-08-20: a click on a button calling confirm() never
// returned. Twenty-three seconds, then the act's own context expired, and every later call on that
// session was gone the same way. That is the worst failure in this whole pillar — not a wrong
// reading, which the agent can doubt, and not a refusal, which it can act on, but a session that
// stops answering. An alert on a cookie notice is enough to cause it.
//
// # Why the answer is no
//
// Dismissing rather than accepting, for everything except one case. The question is on a surface the
// adversary controls (§6.0), and "Delete everything?" is a perfectly ordinary confirm: accepting is
// a DECISION taken on a person's behalf, dismissing is declining to take one. The fence refuses the
// resulting POST either way, but a page that acts on its own answer locally — a client-side list, an
// IndexedDB store, a draft — is not covered by the fence at all.
//
// beforeunload is the exception, and it is accepted. It asks "leave this page?" after the agent has
// already said `goto` or `back`, so dismissing it would silently cancel the act the agent asked for
// and report success. There is nothing destructive on the other side of that one: the destruction it
// warns about is losing what was typed into the page being left.
//
// # Why the agent is told
//
// A dialog answered silently is the page doing less than it meant to, which is the same shape as a
// CSP violation and gets the same treatment: it goes on the reading. An agent that clicked Delete,
// was asked to confirm, and had the confirmation declined would otherwise read a page where nothing
// happened and conclude the button is broken.
const (
	// dialogsRemembered caps how many of a document's questions a reading carries. A page can open
	// them in a loop, and an unbounded list would be the agent's context spent on one hostile page.
	dialogsRemembered = 5
	// dialogAnswerWithin bounds the answering call. Short: the renderer is frozen for exactly as long
	// as this takes, and a driver that hung here would be the bug it exists to fix.
	dialogAnswerWithin = 5 * time.Second
)

// The two answers, spelled out because the agent reads them.
const (
	dialogDismissed = "dismissed"
	dialogAccepted  = "accepted"
)

// onDialog answers a page's question and records what it was asked.
func (d *Driver) onDialog(event cdp.Event) {
	if event.Method != "Page.javascriptDialogOpening" {
		return
	}
	var opening struct {
		Type    string `json:"type"`
		Message string `json:"message"`
		Default string `json:"defaultPrompt"`
	}
	if err := json.Unmarshal(event.Params, &opening); err != nil {
		// Unparseable, and answering anyway is still better than not: the renderer is frozen and the
		// kind is only needed to choose between two answers. No is the safe one.
		opening.Type = ""
	}

	// While a person drives, the question is theirs: it becomes a prompt and the page waits for their
	// answer, or for the prompt's timeout. No CDP call is made here, so this goroutine goes on
	// answering the fence.
	if state := d.person.Load(); state != nil && state.page == event.Session {
		if d.visible {
			// A visible window: the person answers the native dialog there. Nothing to raise, and
			// nothing to answer on their behalf.
			return
		}
		on := event.Session
		raised := d.raisePrompt(state, on, browser.Prompt{
			Kind:          "dialog",
			DialogType:    strings.ToLower(strings.TrimSpace(opening.Type)),
			Message:       opening.Message,
			DefaultPrompt: opening.Default,
		}, func(ctx context.Context) {
			_, _ = d.conn.Call(ctx, on, "Page.handleJavaScriptDialog", map[string]any{"accept": false})
		})
		if raised {
			return
		}
	}

	accept := strings.EqualFold(strings.TrimSpace(opening.Type), "beforeunload")
	answer := dialogDismissed
	if accept {
		answer = dialogAccepted
	}

	// Synchronously, like onFetchPaused: the page is stopped until this call lands, so handing it to
	// a goroutine would only add the chance of it being dropped.
	ctx, cancel := context.WithTimeout(context.Background(), dialogAnswerWithin)
	defer cancel()
	_, err := d.conn.Call(ctx, event.Session, "Page.handleJavaScriptDialog", map[string]any{
		"accept": accept,
	})
	if err != nil {
		// Recorded all the same. A page still frozen is exactly the case where the agent most needs
		// to know a dialog is the reason, and the reading is the only place left to say it.
		answer = "unanswered"
	}

	d.recordDialog(event.Session, browser.Dialog{
		Kind:    strings.ToLower(strings.TrimSpace(opening.Type)),
		Message: strings.TrimSpace(opening.Message),
		Answer:  answer,
	})
}

// recordDialog puts the question on the session that was asked it.
func (d *Driver) recordDialog(on cdp.SessionID, asked browser.Dialog) {
	d.mu.Lock()
	defer d.mu.Unlock()
	owner, known := d.cdpToSession[on]
	if !known {
		return
	}
	entry, live := d.sessions[owner]
	if !live {
		return
	}
	if len(entry.dialogs) >= dialogsRemembered {
		return
	}
	entry.dialogs = append(entry.dialogs, asked)
}

// dialogsSoFar is what to put on a snapshot, or nothing when the page has asked nothing.
func (d *Driver) dialogsSoFar(entry *session) []browser.Dialog {
	d.mu.Lock()
	defer d.mu.Unlock()
	if len(entry.dialogs) == 0 {
		return nil
	}
	// Copied, because the caller builds a value that outlives the lock.
	out := make([]browser.Dialog, len(entry.dialogs))
	copy(out, entry.dialogs)
	return out
}
