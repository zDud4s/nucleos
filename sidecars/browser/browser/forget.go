package browser

import (
	"context"

	"nucleosbrowser/profile"
)

// Profiles is the other thing only whoever owns the directories can do: delete one.
//
// Spec §10 calls it "Esquecer", and it is the counterweight the design needs rather than a
// convenience. The site list grows only by a person finishing a login (§5.2), which means it grows
// for ever unless there is a way back — and what it grows by is a permanent right to load a host
// inside a profile holding live session cookies. What is given has to be removable, in the same
// place, with the same certainty.
//
// Separate from Wheelhouse because the two answer different questions and a driver could sensibly
// have one without the other. Separate from Driver for the reason Wheelhouse is: a single browser
// cannot delete the directory it is running out of.
type Profiles interface {
	// Forget stops whatever is running in a profile and deletes it, returning the sessions it took.
	Forget(ctx context.Context, ref profile.Ref) ([]SessionID, error)
}

// Forget on the Fake drops every session and records the profile it was asked about.
func (f *Fake) Forget(_ context.Context, ref profile.Ref) ([]SessionID, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if err := ref.Validate(); err != nil {
		return nil, err
	}
	f.Forgotten = append(f.Forgotten, ref)
	stopped := make([]SessionID, 0, len(f.sessions))
	for id := range f.sessions {
		stopped = append(stopped, id)
	}
	f.sessions = map[SessionID]Session{}
	f.human = ""
	return stopped, nil
}

// Forget on Unavailable says why there is no browser, rather than 501's "this is not supported".
func (u Unavailable) Forget(context.Context, profile.Ref) ([]SessionID, error) {
	return nil, u.err()
}
