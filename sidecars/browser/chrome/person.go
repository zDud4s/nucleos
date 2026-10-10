// §spec browser-volante

package chrome

import (
	"context"
	"encoding/json"
	"sync"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// personState is the person's turn in this browser. One at a time: BeginPerson refuses a browser with
// a second session, because the fence it lifts is the browser's and not the session's.
type personState struct {
	session browser.SessionID
	page    cdp.SessionID
	// frame is the session's main frame. The fence records a Document request only when it belongs to
	// this frame: an advertisement's iframe is not somewhere the person went.
	frame       string
	recorder    *chainRecorder
	unsubscribe func()

	// pmu guards the prompts the page has put to the person: pending by id, in the order they were
	// raised, and the counter their ids come from. See prompt.go.
	pmu     sync.Mutex
	pending map[string]*pendingPrompt
	order   []string
	seq     int

	// selMu guards the button of a press that landed on a select and was held back, so that the
	// matching release is held back too. Empty when no press is held. See input.go.
	selMu      sync.Mutex
	heldButton string
}

// personHolds reports whether a person's turn is on this session.
func (d *Driver) personHolds(id browser.SessionID) bool {
	state := d.person.Load()
	return state != nil && state.session == id
}

// BeginPerson lifts the fence for one person and marks the session theirs.
//
// Refused unless the browser has exactly this one session. Waits for any agent verb in flight (the
// gate), then reloads the page, so nothing the agent's last act left running keeps running unfenced.
// A failed reload leaves the session the person's and not the agent's: the one wrong thing this can
// do is hand the wheel back to the agent without the fence having been looked at.
func (d *Driver) BeginPerson(ctx context.Context, id browser.SessionID) error {
	if d.personHolds(id) {
		return nil
	}
	d.mu.Lock()
	_, known := d.sessions[id]
	only := len(d.sessions) == 1
	d.mu.Unlock()
	if !known {
		return browser.ErrNoSuchSession
	}
	if !only {
		return browser.ErrNotSoleSession
	}

	d.gate.Lock()
	defer d.gate.Unlock()
	if d.personHolds(id) {
		return nil
	}
	d.mu.Lock()
	entry, ok := d.sessions[id]
	if !ok {
		d.mu.Unlock()
		return browser.ErrNoSuchSession
	}
	if len(d.sessions) != 1 {
		d.mu.Unlock()
		return browser.ErrNotSoleSession
	}
	entry.mode = browser.ModeHuman
	// Marked in this critical section, with the check above: an Open inserting a session afterwards
	// sees it and refuses, and one that inserted before made the count two.
	d.personBegun = true
	final, page, frame := entry.final, entry.cdp, entry.frameID
	d.mu.Unlock()

	recorder := &chainRecorder{}
	if final != "" {
		recorder.record(final)
	}
	unsubscribe := d.conn.OnEvent(func(event cdp.Event) {
		if event.Session != page || event.Method != "Page.frameNavigated" {
			return
		}
		var params struct {
			Frame struct {
				ID       string `json:"id"`
				ParentID string `json:"parentId"`
				URL      string `json:"url"`
			} `json:"frame"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil {
			return
		}
		if params.Frame.ParentID != "" || (frame != "" && params.Frame.ID != frame) || params.Frame.URL == "" {
			return
		}
		recorder.record(params.Frame.URL)
	})
	d.person.Store(&personState{
		session:     id,
		page:        page,
		frame:       frame,
		recorder:    recorder,
		unsubscribe: unsubscribe,
	})

	if d.visible {
		// A visible window: the person sees the native file dialog and the page as it stands, so
		// neither the interception nor the reload applies. The proxy switch comes on LAST, once
		// nothing can fail any more.
		if d.personSwitch != nil {
			d.personSwitch(true)
		}
		d.pushState(ctx, id)
		return nil
	}

	// A native file dialog is invisible in a screencast, so the page's choosers are intercepted for the
	// turn and come back as prompts. A page that cannot be made to do that is not one a person can use.
	if _, err := d.conn.Call(ctx, page, "Page.setInterceptFileChooserDialog", map[string]any{"enabled": true}); err != nil {
		d.person.Store(nil)
		d.clearPersonBegun()
		unsubscribe()
		return err
	}
	if _, err := d.conn.Call(ctx, page, "Page.reload", nil); err != nil {
		d.person.Store(nil)
		d.clearPersonBegun()
		unsubscribe()
		return err
	}
	return nil
}

// MakeVisible marks the driver as the one of a visible (panel) browser. personSwitch is the proxy's
// person switch. Call it before any Open.
func (d *Driver) MakeVisible(personSwitch func(bool)) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.visible = true
	d.personSwitch = personSwitch
	d.personTargets = map[string]cdp.SessionID{}
}

// clearPersonBegun lets Open insert sessions again.
func (d *Driver) clearPersonBegun() {
	d.mu.Lock()
	d.personBegun = false
	d.mu.Unlock()
}

// EndPerson takes the person's turn back and restores the fence, and returns where they went.
//
// The order is the security argument. The person state is cleared first, so everything the reloads
// below fetch is judged by the fence again; every page is reloaded, so nothing the person left
// running survives the fence's return; the profile is swept of workers, because a worker the person's
// pages registered is code from a host nobody listed. Only then is the session the agent's. An error
// at any step is returned as-is, but the fence is already back by then: the person state was cleared
// first, so the mode stays ModeHuman while nobody holds the wheel, and a retry of EndPerson answers
// ErrNotPerson.
func (d *Driver) EndPerson(ctx context.Context, id browser.SessionID) (browser.Returned, error) {
	d.gate.Lock()
	defer d.gate.Unlock()
	state := d.person.Load()
	if state == nil || state.session != id {
		return browser.Returned{}, browser.ErrNotPerson
	}
	// Every question still open is declined while the person still holds the session: the page it
	// froze is unfrozen before the fence is back, and the viewers are told each one is over.
	d.cancelAllPrompts(state)
	if !d.visible {
		// Best effort: the page may be gone, and the fence's reloads below are what matter.
		_, _ = d.conn.Call(ctx, state.page, "Page.setInterceptFileChooserDialog", map[string]any{"enabled": false})
	}
	d.person.Store(nil)
	if d.visible {
		// The proxy's switch goes off in the same step as the person state, before any reload or the
		// sweep, so everything those fetch is judged again.
		if d.personSwitch != nil {
			d.personSwitch(false)
		}
		d.mu.Lock()
		popups := d.personTargets
		d.personTargets = map[string]cdp.SessionID{}
		d.mu.Unlock()
		for target := range popups {
			_, _ = d.conn.Call(ctx, cdp.BrowserSession, "Target.closeTarget", map[string]any{"targetId": target})
		}
	}
	d.clearPersonBegun()
	// The recording ends here: the reloads below are the fence coming back, not somewhere the person
	// went, and they must not reach the chain a grant is read from.
	state.unsubscribe()
	state.recorder.stop()

	d.mu.Lock()
	pages := make([]cdp.SessionID, 0, len(d.sessions))
	for _, entry := range d.sessions {
		pages = append(pages, entry.cdp)
	}
	d.mu.Unlock()
	for _, page := range pages {
		if _, err := d.conn.Call(ctx, page, "Page.reload", nil); err != nil {
			return browser.Returned{}, err
		}
	}
	if err := d.sweepServiceWorkers(ctx); err != nil {
		return browser.Returned{}, err
	}

	d.mu.Lock()
	for _, entry := range d.sessions {
		// Whatever the fence refused while the person drove, or while the reloads ran, was not the
		// agent's doing and is not news for its next act.
		entry.reportedUpTo = d.refusalTotal
	}
	if entry, live := d.sessions[id]; live {
		entry.mode = browser.ModeAgent
	}
	d.mu.Unlock()
	if d.visible {
		d.pushState(ctx, id)
	}
	return browser.Returned{Chain: state.recorder.Chain()}, nil
}
