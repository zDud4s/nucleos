// §spec browser-com-painel

package chrome

import (
	"context"
	"encoding/json"
	"log"
	"net/url"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/panelui"
)

const (
	// panelWorld is the isolated world the panel bundle lives in. A page's own scripts run in the main
	// world and can neither see the bundle nor reach the binding below, which is scoped to this name.
	panelWorld = "nucleos-panel"
	// panelBinding is what the panel calls to speak to the driver.
	panelBinding = "__nucleos"
	// panelHistory is how many messages are kept to replay into a panel world that is born later.
	panelHistory = 200
	// panelFallbackDelay is how long a navigated main frame is given to produce a panel world before
	// the bundle is injected by hand.
	panelFallbackDelay = 1500 * time.Millisecond
	// panelSubBuffer is how many messages a subscriber may be behind before the next is dropped.
	panelSubBuffer = 64
)

// panelSub is one listener on a session's panel.
type panelSub struct {
	ch  chan json.RawMessage
	end chan browser.PanelEnd
}

// panelState is what a visible session keeps for its panel.
//
// Lock order: mu before the driver's mu, never the reverse.
type panelState struct {
	mu        sync.Mutex
	history   []json.RawMessage
	collapsed bool
	// live are the panel worlds that have been told the conversation so far. A push only goes to
	// these: a world that is registered but not yet replayed gets the message through the replay, and
	// would otherwise get it twice.
	live map[contextKey]bool
	subs map[*panelSub]struct{}
	// ended is why the channel ended, or empty while it has not.
	ended browser.PanelEnd
}

func newPanelState() *panelState {
	return &panelState{live: map[contextKey]bool{}, subs: map[*panelSub]struct{}{}}
}

// armPanel injects the panel bundle into a target's isolated world and scopes the binding to that
// world. Called before the first navigation, or the first document runs without a panel. Best effort:
// a session without a panel loses a convenience, not a protection.
func (d *Driver) armPanel(ctx context.Context, on cdp.SessionID) {
	if _, err := d.conn.Call(ctx, on, "Page.addScriptToEvaluateOnNewDocument", map[string]any{
		"source":         panelui.Source,
		"worldName":      panelWorld,
		"runImmediately": true,
	}); err != nil {
		return
	}
	_, _ = d.conn.Call(ctx, on, "Runtime.addBinding", map[string]any{
		"name":                 panelBinding,
		"executionContextName": panelWorld,
	})
}

// panelOf is a visible session's panel.
func (d *Driver) panelOf(id browser.SessionID) (*panelState, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	entry, ok := d.sessions[id]
	if !ok {
		return nil, browser.ErrNoSuchSession
	}
	if entry.panel == nil {
		return nil, browser.ErrUnsupported
	}
	return entry.panel, nil
}

// pruneLive forgets the worlds that are gone and returns the rest. Called with p.mu held.
func (d *Driver) pruneLive(p *panelState) []contextKey {
	d.mu.Lock()
	defer d.mu.Unlock()
	keys := make([]contextKey, 0, len(p.live))
	for key := range p.live {
		if _, there := d.contexts[key]; !there {
			delete(p.live, key)
			continue
		}
		keys = append(keys, key)
	}
	return keys
}

// evaluateInto hands one message to the panel entry in one world.
func (d *Driver) evaluateInto(ctx context.Context, key contextKey, msg json.RawMessage) error {
	_, err := d.conn.Call(ctx, key.session, "Runtime.evaluate", map[string]any{
		"expression": "globalThis.__nucleosPush(" + string(msg) + ")",
		"contextId":  key.id,
	})
	return err
}

// PanelPush says something to the panel and keeps it for the worlds that open later.
func (d *Driver) PanelPush(ctx context.Context, id browser.SessionID, msg json.RawMessage) error {
	if !json.Valid(msg) {
		return browser.ErrUnsupported
	}
	p, err := d.panelOf(id)
	if err != nil {
		return err
	}
	msg = append(json.RawMessage(nil), msg...)
	p.mu.Lock()
	defer p.mu.Unlock()
	p.history = append(p.history, msg)
	if over := len(p.history) - panelHistory; over > 0 {
		p.history = append([]json.RawMessage(nil), p.history[over:]...)
	}
	for _, key := range d.pruneLive(p) {
		_ = d.evaluateInto(ctx, key, msg)
	}
	return nil
}

// stateMessage is the panel's current state. Called with p.mu held.
func (d *Driver) stateMessage(id browser.SessionID, p *panelState) json.RawMessage {
	d.mu.Lock()
	var mode, place string
	if entry, ok := d.sessions[id]; ok {
		mode = string(entry.mode)
		place = entry.final
		if place == "" {
			place = entry.requested
		}
	}
	d.mu.Unlock()
	host := ""
	if u, err := url.Parse(place); err == nil {
		host = u.Host
	}
	msg, _ := json.Marshal(struct {
		V         int    `json:"v"`
		Kind      string `json:"kind"`
		Mode      string `json:"mode"`
		Host      string `json:"host"`
		Collapsed bool   `json:"collapsed"`
	}{1, "state", mode, host, p.collapsed})
	return msg
}

// pushState tells every live panel world who holds the wheel and where the page is.
func (d *Driver) pushState(ctx context.Context, id browser.SessionID) {
	p, err := d.panelOf(id)
	if err != nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	msg := d.stateMessage(id, p)
	for _, key := range d.pruneLive(p) {
		_ = d.evaluateInto(ctx, key, msg)
	}
}

// replayPanel opens a new panel world onto the conversation as it stood, and its state.
func (d *Driver) replayPanel(on cdp.SessionID, contextID int64) {
	key := contextKey{session: on, id: contextID}
	d.mu.Lock()
	owner, known := d.cdpToSession[on]
	entry, live := d.sessions[owner]
	d.mu.Unlock()
	if !known || !live || entry.panel == nil {
		return
	}
	p := entry.panel
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.live[key] {
		return
	}
	d.mu.Lock()
	_, there := d.contexts[key]
	d.mu.Unlock()
	if !there {
		return
	}
	for _, msg := range p.history {
		_ = d.evaluateInto(ctx, key, msg)
	}
	_ = d.evaluateInto(ctx, key, d.stateMessage(owner, p))
	p.live[key] = true
}

// adoptPanelWorlds replays into the panel worlds that appeared before the session was registered,
// which is what arming with runImmediately does on a blank document.
func (d *Driver) adoptPanelWorlds(on cdp.SessionID) {
	d.mu.Lock()
	var ids []int64
	for key, c := range d.contexts {
		if key.session == on && c.name == panelWorld {
			ids = append(ids, key.id)
		}
	}
	d.mu.Unlock()
	for _, id := range ids {
		go d.replayPanel(on, id)
	}
}

// panelCalled hears the panel. Only the panel's own world of a visible session is believed: the same
// call from a page's world is an attempt to speak as the panel, and is dropped.
func (d *Driver) panelCalled(on cdp.SessionID, contextID int64, payload string) {
	d.mu.Lock()
	world, placed := d.contexts[contextKey{session: on, id: contextID}]
	owner, known := d.cdpToSession[on]
	entry, live := d.sessions[owner]
	visible := d.visible
	d.mu.Unlock()
	if !visible || !placed || world.name != panelWorld || !known || !live || entry.panel == nil || !json.Valid([]byte(payload)) {
		// The payload is the caller's, and is not logged.
		log.Printf("browser: dropped a panel binding call from outside the panel world")
		return
	}
	p := entry.panel

	var head struct {
		Kind      string `json:"kind"`
		Collapsed *bool  `json:"collapsed"`
	}
	_ = json.Unmarshal([]byte(payload), &head)
	if head.Kind == "collapse" {
		// Handled here and not forwarded: collapsing is the panel's own, and the state goes back to
		// every world so a reopened panel keeps it.
		go func() {
			p.mu.Lock()
			p.collapsed = head.Collapsed == nil || *head.Collapsed
			p.mu.Unlock()
			ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
			defer cancel()
			d.pushState(ctx, owner)
		}()
		return
	}

	msg := json.RawMessage(payload)
	p.mu.Lock()
	defer p.mu.Unlock()
	for sub := range p.subs {
		select {
		case sub.ch <- msg:
		default:
			log.Printf("browser: a panel listener is behind; a message was dropped")
		}
	}
}

// PanelEvents delivers what the panel says until ctx ends or the panel does.
func (d *Driver) PanelEvents(ctx context.Context, id browser.SessionID, sink func(json.RawMessage)) error {
	p, err := d.panelOf(id)
	if err != nil {
		return err
	}
	sub := &panelSub{ch: make(chan json.RawMessage, panelSubBuffer), end: make(chan browser.PanelEnd, 1)}
	p.mu.Lock()
	if p.ended != "" {
		reason := p.ended
		p.mu.Unlock()
		return browser.PanelClosed{Reason: reason}
	}
	p.subs[sub] = struct{}{}
	p.mu.Unlock()
	defer func() {
		p.mu.Lock()
		delete(p.subs, sub)
		p.mu.Unlock()
	}()
	for {
		select {
		case msg := <-sub.ch:
			sink(msg)
		case reason := <-sub.end:
			return browser.PanelClosed{Reason: reason}
		case <-d.conn.Done():
			return browser.PanelClosed{Reason: browser.PanelPersonClosed}
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

// endPanel ends the channel with a reason. The first reason wins: closing a session from this side
// makes Chromium report the target gone, and that is not the person's doing.
func (d *Driver) endPanel(p *panelState, reason browser.PanelEnd) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ended != "" {
		return
	}
	p.ended = reason
	for sub := range p.subs {
		select {
		case sub.end <- reason:
		default:
		}
	}
}

// onPanelEvent ends a panel whose window went away, and re-injects the bundle into a main frame that
// navigated without producing a panel world.
func (d *Driver) onPanelEvent(event cdp.Event) {
	switch event.Method {
	case "Target.targetDestroyed":
		var params struct {
			TargetID string `json:"targetId"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil {
			return
		}
		d.mu.Lock()
		var p *panelState
		if owner, ok := d.targets[params.TargetID]; ok {
			// A popup is mapped to its opener too; only the session's own main target is the window.
			if entry, live := d.sessions[owner]; live && entry.target == params.TargetID {
				p = entry.panel
			}
		}
		d.mu.Unlock()
		if p != nil {
			d.endPanel(p, browser.PanelPersonClosed)
		}

	case "Page.frameNavigated":
		var params struct {
			Frame struct {
				ID       string `json:"id"`
				ParentID string `json:"parentId"`
			} `json:"frame"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil || params.Frame.ParentID != "" {
			return
		}
		d.mu.Lock()
		owner, known := d.cdpToSession[event.Session]
		entry, live := d.sessions[owner]
		mine := known && live && entry.panel != nil && entry.cdp == event.Session
		d.mu.Unlock()
		if mine {
			go d.reinjectPanel(event.Session, params.Frame.ID)
		}
	}
}

// reinjectPanel is the fallback for a document that came up without a panel world. Best effort.
func (d *Driver) reinjectPanel(on cdp.SessionID, frame string) {
	select {
	case <-time.After(panelFallbackDelay):
	case <-d.conn.Done():
		return
	}
	d.mu.Lock()
	for key, c := range d.contexts {
		if key.session == on && c.name == panelWorld {
			d.mu.Unlock()
			return
		}
	}
	d.mu.Unlock()
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	raw, err := d.conn.Call(ctx, on, "Page.createIsolatedWorld", map[string]any{
		"frameId":   frame,
		"worldName": panelWorld,
	})
	if err != nil {
		return
	}
	var world struct {
		ExecutionContextID int64 `json:"executionContextId"`
	}
	if err := json.Unmarshal(raw, &world); err != nil || world.ExecutionContextID == 0 {
		return
	}
	_, _ = d.conn.Call(ctx, on, "Runtime.evaluate", map[string]any{
		"expression": panelui.Source,
		"contextId":  world.ExecutionContextID,
	})
}
