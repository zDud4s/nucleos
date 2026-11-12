// §spec browser-ao-vivo

package pool

import (
	"context"
	"errors"
	"fmt"

	"nucleosbrowser/browser"
)

// viewer is one open Watch. The pool keeps only the means to end it.
type viewer struct {
	cancel context.CancelCauseFunc
}

// Watch streams a session's page to sink until ctx ends, the browser ends, or the session does.
//
// A session that ends under the viewer ends the watch with a browser.WatchEnded carrying the reason, so
// the caller can tell "the agent closed it" from "a person took the wheel" from "the browser went away".
// A browser that is not a Watcher — a person's — is refused as ErrPersonIsDriving: the session exists,
// the pillar will not show it.
func (p *Pool) Watch(ctx context.Context, id browser.SessionID, sink func(browser.Frame)) error {
	session, err := p.lookup(id)
	if err != nil {
		return err
	}
	var watcher browser.Watcher
	if session.holder.driver != nil {
		watcher, _ = session.holder.driver.(browser.Watcher)
	}
	if watcher == nil {
		return fmt.Errorf("%w: %s", browser.ErrPersonIsDriving, id)
	}

	wctx, cancel := context.WithCancelCause(ctx)
	v := &viewer{cancel: cancel}

	// Registered under the same lock that says the session exists, so a viewer can never slip in
	// after the session was taken down and miss being ended.
	p.mu.Lock()
	if _, ok := p.sessions[id]; !ok {
		p.mu.Unlock()
		cancel(nil)
		return browser.ErrNoSuchSession
	}
	if p.watchers == nil {
		p.watchers = map[browser.SessionID]map[*viewer]struct{}{}
	}
	if p.watchers[id] == nil {
		p.watchers[id] = map[*viewer]struct{}{}
	}
	p.watchers[id][v] = struct{}{}
	p.mu.Unlock()

	defer func() {
		p.mu.Lock()
		if set := p.watchers[id]; set != nil {
			delete(set, v)
			if len(set) == 0 {
				delete(p.watchers, id)
			}
		}
		p.mu.Unlock()
		cancel(nil)
	}()

	watchErr := watcher.Watch(wctx, session.inner, sink)

	var ended browser.WatchEnded
	if errors.As(context.Cause(wctx), &ended) {
		return ended
	}
	return watchErr
}

// endWatchers ends every viewer of the given sessions with reason.
//
// It cancels and returns: a viewer whose sink is stuck on a slow client must not hold the wheel, a
// close or a shutdown hostage, so nothing here waits for a viewer to notice. The first reason wins —
// that is how context.WithCancelCause behaves — so ending a viewer twice is harmless. Callers hold no
// lock.
func (p *Pool) endWatchers(reason browser.EndReason, ids ...browser.SessionID) {
	var ending []*viewer
	p.mu.Lock()
	for _, id := range ids {
		for v := range p.watchers[id] {
			ending = append(ending, v)
		}
		delete(p.watchers, id)
	}
	p.mu.Unlock()
	for _, v := range ending {
		v.cancel(browser.WatchEnded{Reason: reason})
	}
}
