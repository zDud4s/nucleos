package chrome

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/fence"
)

// This file gives the fence's third layer a voice.
//
// The fence has three: the CDP interception, the loopback proxy, and the CSP injected into every
// document. The first two REFUSE things, and a refusal is a value the agent is handed. The third
// one is silent, and silent in the worst possible way: a CSP violation is enforced inside the
// renderer, so no request is ever made, so `Fetch.requestPaused` never fires and there is nothing
// for the fence to report. The page simply does less than it meant to.
//
// Two consequences, both measured in this repository before this file existed:
//
//   - A form submission stopped by `form-action 'none'` came back to the agent as "done". The gate
//     test that would have caught it had its assertion REMOVED, with a comment recording the
//     measurement: a POST is stopped twice, by the method rule and by the CSP, and when the CSP wins
//     the race there is no request to report. That was the stronger outcome wearing the weaker
//     report.
//
//   - `connect-src 'none'` closes fetch, XHR, EventSource and beacons. A page that arrives empty and
//     fills itself from an API therefore renders a shell — and the agent reads the shell, correctly,
//     as a page with nothing on it. Nothing anywhere says otherwise.
//
// Chromium does log both, on the browser's own log rather than in the page: `Log.entryAdded` with
// source `security`. Reading it there and not from a `securitypolicyviolation` listener injected
// into the page is the same rule the rest of the fence follows — the report must not come from the
// thing being reported on, or a page can decide what the agent hears about it.

// ourDirectives is the set of directives THIS fence sends, derived from the one place they are
// written. A page may ship a CSP of its own, and what that policy blocks is the page's business:
// attributing it to the fence would tell the agent that the machine refused something when what
// happened is that the site did.
//
// Derived rather than repeated, so a directive added to fence.Directives is understood here without
// anyone remembering that this file exists. The match is on the whole `directive value` pair, which
// is a heuristic: a page that ships a byte-identical `connect-src 'none'` is credited to us. The
// misattribution is cheap — the content is not coming either way, and that is what the agent acts on.
var ourDirectives = func() map[string]bool {
	set := map[string]bool{}
	for _, one := range strings.Split(fence.Directives, ";") {
		if trimmed := strings.TrimSpace(one); trimmed != "" {
			set[trimmed] = true
		}
	}
	return set
}()

// watchCSP enables the browser's log on a target, so what the CSP stops can be heard.
//
// Best effort, and deliberately not fatal: a driver that refused to open a page because the log
// would not enable would be trading a real capability for a better report. The fence itself is
// unaffected — this layer is about SAYING what was blocked, never about blocking it.
func (d *Driver) watchCSP(ctx context.Context, on cdp.SessionID) {
	_, _ = d.conn.Call(ctx, on, "Log.enable", nil)
}

// onLogEntry hears a CSP violation and turns it into something the agent is told.
func (d *Driver) onLogEntry(event cdp.Event) {
	if event.Method != "Log.entryAdded" {
		return
	}
	var params struct {
		Entry struct {
			Source string `json:"source"`
			Text   string `json:"text"`
		} `json:"entry"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		return
	}
	if params.Entry.Source != "security" {
		return
	}
	directive, blocked, ours := violation(params.Entry.Text)
	if !ours {
		return
	}

	d.mu.Lock()
	owner, known := d.cdpToSession[event.Session]
	d.mu.Unlock()
	if !known {
		return
	}

	if directive == "form-action" {
		// A form submission is always caused by an act, so it goes where an act will find it: the
		// refusal chain, in the vocabulary §6.2 already gave it. This is the half that used to come
		// back as "done".
		d.recordSessionRefusal(owner, browser.ConsequenceForm,
			fmt.Sprintf("the fence stopped a form submission to %s", describe(blocked)))
		return
	}

	// Everything else is something the page did for itself, on its own schedule. It goes on the
	// SNAPSHOT rather than into the refusal chain: a page that polls would otherwise poison the next
	// twenty acts with answers about requests none of them caused, and the thing the agent actually
	// needs to know — that what it is reading may be a shell — is a fact about the reading.
	d.recordBlocked(owner, consequenceOf(blocked), directive, blocked)
}

// violation reads a CSP log line, and says whether it was ours.
//
// The text Chromium writes is, in full:
//
//	Refused to connect to 'https://api.example.org/data' because it violates the following
//	Content Security Policy directive: "connect-src 'none'".
func violation(text string) (directive, blocked string, ours bool) {
	const marker = `Content Security Policy directive: "`
	at := strings.Index(text, marker)
	if at < 0 {
		return "", "", false
	}
	rest := text[at+len(marker):]
	end := strings.IndexByte(rest, '"')
	if end < 0 {
		return "", "", false
	}
	whole := rest[:end]
	if !ourDirectives[whole] {
		return "", "", false
	}
	directive = whole
	if space := strings.IndexByte(whole, ' '); space > 0 {
		directive = whole[:space]
	}

	// The blocked url is the first quoted thing in the sentence, and it is looked for BEFORE the
	// directive marker: the directive's own value is quoted too, and searching the whole line would
	// find `none` on a page that made no request at all.
	head := text[:at]
	if open := strings.IndexByte(head, '\''); open >= 0 {
		if close := strings.IndexByte(head[open+1:], '\''); close >= 0 {
			blocked = head[open+1 : open+1+close]
		}
	}
	return directive, blocked, true
}

// consequenceOf names what kind of thing was stopped, from the url it was aimed at.
//
// A WebSocket is already a named consequence and keeps its name, because it IS a different thing:
// the proxy refuses `ws:` under that name, and `wss:` is invisible to both other layers and reaches
// the agent only through here. Two names for one channel depending on which layer caught it would be
// the vocabulary lying about the machine.
func consequenceOf(blocked string) browser.Consequence {
	lower := strings.ToLower(blocked)
	if strings.HasPrefix(lower, "ws://") || strings.HasPrefix(lower, "wss://") {
		return browser.ConsequenceChannel
	}
	return browser.ConsequencePageRequest
}

func describe(blocked string) string {
	if blocked == "" {
		return "somewhere it did not name"
	}
	return blocked
}

// recordBlocked counts one thing the CSP stopped, against the document it happened in.
func (d *Driver) recordBlocked(id browser.SessionID, consequence browser.Consequence, directive, blocked string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	entry, live := d.sessions[id]
	if !live {
		return
	}
	entry.blocked++
	entry.blockedLast = browser.Refusal{
		Consequence: consequence,
		Detail: fmt.Sprintf("the page tried to reach %s on its own; the fence allows no request a page makes for itself (%s)",
			describe(blocked), directive),
	}
}

// blockedSoFar is what to put on a snapshot, or nothing when the page has been left alone.
func (d *Driver) blockedSoFar(entry *session) *browser.Blocked {
	d.mu.Lock()
	defer d.mu.Unlock()
	if entry.blocked == 0 {
		return nil
	}
	return &browser.Blocked{
		Count:       entry.blocked,
		Consequence: entry.blockedLast.Consequence,
		Detail:      entry.blockedLast.Detail,
	}
}
