// §spec browser-volante

package browser

import "context"

// BeginPerson on the Fake records the turn, or fails with PersonErr. An unknown session is refused
// first, as a real driver would.
func (f *Fake) BeginPerson(_ context.Context, id SessionID) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if _, ok := f.sessions[id]; !ok {
		return ErrNoSuchSession
	}
	if f.PersonErr != nil {
		return f.PersonErr
	}
	f.person = id
	f.PersonBegun = append(f.PersonBegun, id)
	return nil
}

// EndPerson on the Fake hands back whatever chain a test put in Chain, and refuses a session no
// person holds.
func (f *Fake) EndPerson(_ context.Context, id SessionID) (Returned, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if _, ok := f.sessions[id]; !ok {
		return Returned{}, ErrNoSuchSession
	}
	if f.person == "" || f.person != id {
		return Returned{}, ErrNotPerson
	}
	f.person = ""
	f.PersonEnded = append(f.PersonEnded, id)
	return Returned{Chain: f.Chain}, nil
}

// Input on the Fake records the batch. It refuses an unknown session, a session no person holds, and
// otherwise fails with InputErr if one is set.
func (f *Fake) Input(_ context.Context, id SessionID, events []InputEvent) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if _, ok := f.sessions[id]; !ok {
		return ErrNoSuchSession
	}
	if f.person == "" || f.person != id {
		return ErrNotPerson
	}
	if f.InputErr != nil {
		return f.InputErr
	}
	f.Inputs = append(f.Inputs, append([]InputEvent(nil), events...))
	return nil
}
