// §spec pilar-de-browser

package browser

import (
	"context"
	"fmt"
	"sync"

	"nucleosbrowser/profile"
)

// Fake is the driver the tests drive. Like search.Fake in the web sidecar it ships in the binary
// rather than hiding behind a build tag, because `serve` is tested against a real HTTP server and a
// browser sidecar that needs Chrome to be tested is a browser sidecar that is not tested.
//
// # Why the zero value refuses to open anything
//
// FenceAttached defaults to false, so a Fake nobody configured fails every Open with
// ErrFenceNotAttached. That is inconvenient on purpose. Spec §6.2a says the fence failing to attach
// must stop navigation, and a Fake whose zero value pretended the fence was up would model the
// exact opposite of the invariant — every test would pass with the fence missing, which is the one
// outcome the invariant exists to prevent.
type Fake struct {
	mu sync.Mutex

	// FenceAttached must be set true for Open to succeed. See above.
	FenceAttached bool

	// OpenErr, if set, is what Open returns instead of a session.
	OpenErr error
	// FinalURL, if set, is reported as the session's landing url — for exercising the
	// requested-vs-final conjunction of spec §5.3.
	FinalURL string
	// Snap is what Snapshot returns.
	Snap Snapshot
	// Refuse, if set, makes every Act return a refusal carrying it.
	Refuse *Refusal
	// ActErr, if set, makes Act fail outright — a broken browser, not a fenced one.
	ActErr error
	// Shot is what Screenshot returns.
	Shot []byte
	// Looked is what Look returns.
	Looked LookResult

	// Chain is what ReturnWheel reports as the navigation a person's window recorded (spec §5.3a).
	Chain []string

	// Recorded calls, so a test can assert what the driver actually received rather than what the
	// caller believed it sent.
	Opened    []OpenRequest
	Actions   []Action
	Snapshots []SessionID
	// Asked is the SnapshotRequest each of those carried, and it is recorded because the ONE thing
	// a wire shape can get wrong silently is dropping a field: the caller sends it, the JSON decoder
	// finds no home for it, and the answer is a correct reading of a request nobody made. That is
	// how `controls_from` was accepted, ignored, and answered with page one for two days.
	Asked     []SnapshotRequest
	Closed    []SessionID
	Wheels    []WheelRequest
	Handed    []SessionID
	Forgotten []profile.Ref

	sessions map[SessionID]Session
	counter  int
	// human is the session a person is driving, if any. The Fake keeps it for the same reason the
	// pool does: a return has to be refusable for a session nobody was ever handed.
	human SessionID
	// person is the session a person drives in the agent's own browser (see BeginPerson).
	person SessionID

	// PersonErr, if set, is what BeginPerson returns instead of beginning.
	PersonErr error
	// PersonBegun and PersonEnded record the sessions BeginPerson and EndPerson accepted.
	PersonBegun []SessionID
	PersonEnded []SessionID

	// Inputs is every batch Input accepted, in order. InputErr, if set, is what Input returns instead.
	Inputs   [][]InputEvent
	InputErr error
}

func (f *Fake) Name() string { return "fake" }

func (f *Fake) Open(_ context.Context, req OpenRequest) (Session, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.Opened = append(f.Opened, req)
	if f.OpenErr != nil {
		return Session{}, f.OpenErr
	}
	if !f.FenceAttached {
		return Session{}, ErrFenceNotAttached
	}
	f.counter++
	final := req.URL
	if f.FinalURL != "" {
		final = f.FinalURL
	}
	session := Session{
		ID:           SessionID(fmt.Sprintf("s%d", f.counter)),
		Mode:         ModeAgent,
		RequestedURL: req.URL,
		FinalURL:     final,
	}
	if f.sessions == nil {
		f.sessions = map[SessionID]Session{}
	}
	f.sessions[session.ID] = session
	return session, nil
}

func (f *Fake) Snapshot(_ context.Context, id SessionID, req SnapshotRequest) (Snapshot, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.Snapshots = append(f.Snapshots, id)
	f.Asked = append(f.Asked, req)
	if _, ok := f.sessions[id]; !ok {
		return Snapshot{}, ErrNoSuchSession
	}
	snap := f.Snap
	snap.SessionID = id
	return snap, nil
}

func (f *Fake) Act(_ context.Context, id SessionID, action Action) (ActResult, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.Actions = append(f.Actions, action)
	session, ok := f.sessions[id]
	if !ok {
		return ActResult{}, ErrNoSuchSession
	}
	// Spec §4.4 rule 1, modelled here so everything above the driver meets it in tests: once the
	// wheel has been ASKED for, the agent's actions are refused and not queued. A queued click lands
	// on a page the person has already navigated away from.
	if session.Mode != ModeAgent {
		return Refused(ConsequenceWheelRequested, "the wheel has been asked for"), nil
	}
	if f.ActErr != nil {
		return ActResult{}, f.ActErr
	}
	if f.Refuse != nil {
		return Refused(f.Refuse.Consequence, f.Refuse.Detail), nil
	}
	return Done(), nil
}

func (f *Fake) Screenshot(_ context.Context, id SessionID) ([]byte, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if _, ok := f.sessions[id]; !ok {
		return nil, ErrNoSuchSession
	}
	return f.Shot, nil
}

// Look answers from Looked, and records nothing beyond the session check: a fake that invented an
// image would let a caller test its own handling of a picture nobody produced.
func (f *Fake) Look(_ context.Context, id SessionID) (LookResult, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if _, ok := f.sessions[id]; !ok {
		return LookResult{}, ErrNoSuchSession
	}
	return f.Looked, nil
}

func (f *Fake) Handoff(_ context.Context, id SessionID, reason string) (HandoffTicket, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	session, ok := f.sessions[id]
	if !ok {
		return HandoffTicket{}, ErrNoSuchSession
	}
	session.Mode = ModeHuman
	f.sessions[id] = session
	return HandoffTicket{
		SessionID: id,
		Mode:      ModeHuman,
		URL:       session.FinalURL,
		Reason:    reason,
	}, nil
}

func (f *Fake) Close(_ context.Context, id SessionID) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.Closed = append(f.Closed, id)
	if _, ok := f.sessions[id]; !ok {
		return ErrNoSuchSession
	}
	delete(f.sessions, id)
	return nil
}

// Unavailable stands in when no driver could be built — Chromium not downloaded, the driver process
// not running, an unknown driver name.
//
// It is a Driver rather than a nil check at every call site for the reason search.Unavailable gives
// in the web sidecar: a nil interface travelling through `serve` is a panic waiting for the one path
// nobody tested. This one answers honestly, every time, and it answers CLOSED.
type Unavailable struct {
	Reason error
}

func (u Unavailable) Name() string { return "unavailable" }

func (u Unavailable) err() error {
	if u.Reason != nil {
		return u.Reason
	}
	return ErrFenceNotAttached
}

func (u Unavailable) Open(context.Context, OpenRequest) (Session, error) {
	return Session{}, u.err()
}
func (u Unavailable) Snapshot(context.Context, SessionID, SnapshotRequest) (Snapshot, error) {
	return Snapshot{}, u.err()
}
func (u Unavailable) Act(context.Context, SessionID, Action) (ActResult, error) {
	return ActResult{}, u.err()
}
func (u Unavailable) Screenshot(context.Context, SessionID) ([]byte, error) {
	return nil, u.err()
}
func (u Unavailable) Look(context.Context, SessionID) (LookResult, error) {
	return LookResult{}, u.err()
}
func (u Unavailable) Handoff(context.Context, SessionID, string) (HandoffTicket, error) {
	return HandoffTicket{}, u.err()
}
func (u Unavailable) Close(context.Context, SessionID) error { return u.err() }

// Unavailable implements Wheelhouse too, so a handover into a sidecar with no browser reports WHY
// there is none — Chromium not downloaded, the driver unknown — instead of the 501 an unimplemented
// interface would produce, which says the wrong thing: the wheel is supported, the browser is missing.
func (u Unavailable) TakeWheel(context.Context, WheelRequest) (Wheel, error) {
	return Wheel{}, u.err()
}
func (u Unavailable) ReturnWheel(context.Context, SessionID) (Returned, error) {
	return Returned{}, u.err()
}
