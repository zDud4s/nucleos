// §spec browser-volante

package pool

import (
	"context"
	"encoding/json"

	"nucleosbrowser/browser"
)

// BeginPerson hands the session's browser to a person, fence lifted. The browser must hold exactly
// this one session: the fence is browser-wide, so lifting it for one session lifts it for all.
//
// The profile is marked in the same critical section as the pool's sole-session check, but that check
// only counts sessions an Open has already finished, so it cannot see one in flight. The driver closes
// that gap: chrome.Driver.BeginPerson marks the person in the same d.mu section as its own session
// count, and Driver.Open refuses to insert a session once it is marked. An Open that inserts first
// makes the count two; one that comes after is refused before it navigates. Viewers are not ended:
// the browser and the page stay the same.
func (p *Pool) BeginPerson(ctx context.Context, id browser.SessionID) error {
	session, err := p.lookup(id)
	if err != nil {
		return err
	}
	seat, ok := session.holder.driver.(browser.PersonSeat)
	if !ok {
		return browser.ErrUnsupported
	}

	p.mu.Lock()
	if len(session.holder.sessions) != 1 {
		p.mu.Unlock()
		return browser.ErrNotSoleSession
	}
	session.holder.person = true
	p.mu.Unlock()

	if err := seat.BeginPerson(ctx, session.inner); err != nil {
		p.mu.Lock()
		session.holder.person = false
		p.mu.Unlock()
		return err
	}
	return nil
}

// EndPerson takes the browser back from the person and returns the chain they walked. On failure the
// mark stays: the fence may not be back up, and core closes the session, which clears it.
func (p *Pool) EndPerson(ctx context.Context, id browser.SessionID) (browser.Returned, error) {
	session, err := p.lookup(id)
	if err != nil {
		return browser.Returned{}, err
	}
	seat, ok := session.holder.driver.(browser.PersonSeat)
	if !ok {
		return browser.Returned{}, browser.ErrUnsupported
	}
	returned, err := seat.EndPerson(ctx, session.inner)
	if err != nil {
		return browser.Returned{}, err
	}
	p.mu.Lock()
	session.holder.person = false
	p.mu.Unlock()
	return returned, nil
}

// Input applies a person's input batch in the session's browser. The pool does not interpret it.
func (p *Pool) Input(ctx context.Context, id browser.SessionID, events []browser.InputEvent) error {
	session, err := p.lookup(id)
	if err != nil {
		return err
	}
	input, ok := session.holder.driver.(browser.PersonInput)
	if !ok {
		return browser.ErrUnsupported
	}
	return input.Input(ctx, session.inner, events)
}

// Answer delivers a person's answer to a pending prompt in the session's browser. The pool does not
// interpret it.
func (p *Pool) Answer(ctx context.Context, id browser.SessionID, prompt string, answer json.RawMessage) error {
	session, err := p.lookup(id)
	if err != nil {
		return err
	}
	answerer, ok := session.holder.driver.(browser.PersonAnswer)
	if !ok {
		return browser.ErrUnsupported
	}
	return answerer.Answer(ctx, session.inner, prompt, answer)
}
