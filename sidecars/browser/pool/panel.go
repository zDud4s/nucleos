// §spec browser-com-painel

package pool

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"nucleosbrowser/browser"
)

// panelListener is one open PanelEvents. Like a viewer, the pool keeps only the means to end it.
type panelListener struct {
	cancel context.CancelCauseFunc
}

// panelOf is the session's browser as a Panel, or ErrUnsupported when it carries none.
func (p *Pool) panelOf(id browser.SessionID) (browser.Panel, *placed, error) {
	session, err := p.lookup(id)
	if err != nil {
		return nil, nil, err
	}
	var panel browser.Panel
	if session.holder.driver != nil {
		panel, _ = session.holder.driver.(browser.Panel)
	}
	if panel == nil {
		return nil, nil, fmt.Errorf("%w: %s has no panel", browser.ErrUnsupported, id)
	}
	return panel, &session, nil
}

// PanelPush says something to the panel of the session's browser.
func (p *Pool) PanelPush(ctx context.Context, id browser.SessionID, msg json.RawMessage) error {
	panel, session, err := p.panelOf(id)
	if err != nil {
		return err
	}
	return panel.PanelPush(ctx, session.inner, msg)
}

// PanelEvents streams the panel's events to sink until ctx ends, the browser ends the channel, or the
// session does; a session closed from this side ends it with browser.PanelClosed{PanelSessionClosed}.
func (p *Pool) PanelEvents(ctx context.Context, id browser.SessionID, sink func(json.RawMessage)) error {
	panel, session, err := p.panelOf(id)
	if err != nil {
		return err
	}

	pctx, cancel := context.WithCancelCause(ctx)
	l := &panelListener{cancel: cancel}

	// Registered under the lock that says the session exists, so a listener cannot slip in after the
	// session was taken down and miss being ended.
	p.mu.Lock()
	if _, ok := p.sessions[id]; !ok {
		p.mu.Unlock()
		cancel(nil)
		return browser.ErrNoSuchSession
	}
	if p.panelListeners == nil {
		p.panelListeners = map[browser.SessionID]map[*panelListener]struct{}{}
	}
	if p.panelListeners[id] == nil {
		p.panelListeners[id] = map[*panelListener]struct{}{}
	}
	p.panelListeners[id][l] = struct{}{}
	p.mu.Unlock()

	defer func() {
		p.mu.Lock()
		if set := p.panelListeners[id]; set != nil {
			delete(set, l)
			if len(set) == 0 {
				delete(p.panelListeners, id)
			}
		}
		p.mu.Unlock()
		cancel(nil)
	}()

	eventsErr := panel.PanelEvents(pctx, session.inner, sink)

	var closed browser.PanelClosed
	if errors.As(context.Cause(pctx), &closed) {
		return closed
	}
	return eventsErr
}

// endPanelListeners ends every panel listener of the given sessions with reason. It cancels and
// returns, like endWatchers: nothing here waits for a listener to notice. Callers hold no lock.
func (p *Pool) endPanelListeners(reason browser.PanelEnd, ids ...browser.SessionID) {
	var ending []*panelListener
	p.mu.Lock()
	for _, id := range ids {
		for l := range p.panelListeners[id] {
			ending = append(ending, l)
		}
		delete(p.panelListeners, id)
	}
	p.mu.Unlock()
	for _, l := range ending {
		l.cancel(browser.PanelClosed{Reason: reason})
	}
}
