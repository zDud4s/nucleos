package browser

import (
	"context"
	"errors"
	"fmt"

	"nucleosbrowser/profile"
)

// The wheel — spec §4.4 — is the one part of this contract a single browser cannot implement.
//
// Handing it over is not a mode flag. Chrome does not switch from headless to headful while it runs,
// so spec §4.2 makes the swap a process swap: close the headless browser gracefully, start a headful
// one over the SAME --user-data-dir, at the same url. A browser.Driver is already bound to a profile
// and a fence that were fixed on its command line; it has no way to become a different process.
// Whatever owns the profiles and the launcher does, and that is why this lives behind its own
// interface instead of growing two more methods onto Driver.
//
// # Why a Driver that cannot do this is not a broken Driver
//
// The Fake implements Wheelhouse, and so does the pool. A single chrome.Driver does not, and serve
// answers 501 for it rather than pretending. The six verbs are the agent's surface and work with any
// driver; the wheel is the person's, and it needs the half of the system that can start processes.

// ErrPersonIsDriving is returned for an agent operation on a profile a person holds.
//
// Spec §4.1 allows exactly one browser per profile and no hidden third state, so while a headful
// window is up there is nowhere for an agent session on that profile to go. The refusal is named
// rather than folded into ErrNoSuchSession because the two ask for opposite responses: a lost session
// is retried elsewhere, and this one is waited out.
var ErrPersonIsDriving = errors.New("browser: a person is driving this profile")

// ErrNoWheelToReturn is a return for a session no person is driving.
var ErrNoWheelToReturn = errors.New("browser: this session is not a person's to give back")

// ErrNotAProjectProfile refuses a handover into a throwaway.
//
// Spec §4.5: a wheel request from an ephemeral profile is always a request to ESTABLISH a session,
// so the window opens in the project's profile and not in the one that dies with the run. A handover
// into a throwaway would ask a person to log in somewhere the login is deleted minutes later — and
// the proposal that survives the run would then point at a profile that no longer exists.
var ErrNotAProjectProfile = errors.New("browser: the wheel is only handed over into a project profile")

// WheelRequest is the núcleo saying a person accepted (spec §4.4).
//
// Session names the agent session the request came from, so it can be closed on the way — it may
// already be gone, and that is not an error. URL and Placement are what the headful window opens,
// and both are the núcleo's: the agent chose neither the profile nor, after a redirect, the url.
type WheelRequest struct {
	Session   SessionID `json:"session,omitempty"`
	URL       string    `json:"url"`
	Placement Placement `json:"placement"`
}

// Wheel is the window, open, with a person in front of it.
//
// Displaced names the agent sessions that were closed to make room. It is reported rather than
// silently absorbed because the núcleo has rows for them: spec §4.1 says one browser per profile, so
// a handover into a profile that already had a headless browser takes that browser down, and rows
// left saying "open" about it would be rows the UI offers to hand over a second time.
type Wheel struct {
	Session   SessionID   `json:"session"`
	Mode      Mode        `json:"mode"`
	URL       string      `json:"url"`
	Displaced []SessionID `json:"displaced,omitempty"`
}

// Returned is the wheel coming back, and the only thing it carries is the reason it matters.
//
// Chain is the navigation the headful window recorded while the person was driving (spec §5.3a): the
// destination and every host the login crossed on the way, in order. It is CANDIDATES and not
// permissions — the núcleo shows it and the person grants the set or nothing.
//
// Recorded under human control, which is what makes it safe to offer. The confused-deputy problem of
// spec §5.2 is that the agent picks the host in the dialogue; here nothing the agent said reaches
// this list, because the list is what a person's own clicks produced.
type Returned struct {
	Chain []string `json:"chain"`
}

// Wheelhouse is spec §4.4's half of the contract: hand a session to a person, and take it back.
type Wheelhouse interface {
	// TakeWheel closes the agent's browser and opens a headful one over the same profile.
	TakeWheel(ctx context.Context, req WheelRequest) (Wheel, error)
	// ReturnWheel closes the person's window and reports what it recorded. The graceful close is the
	// point of it: spec §4.2 measured that a killed browser loses the last writes to its profile, and
	// the last write of a login session is the login.
	ReturnWheel(ctx context.Context, id SessionID) (Returned, error)
}

// TakeWheel on the Fake swaps the session's mode and records the request, so everything above the
// driver can be tested without a browser.
func (f *Fake) TakeWheel(_ context.Context, req WheelRequest) (Wheel, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if req.Placement.Profile.Kind != profile.Project {
		return Wheel{}, ErrNotAProjectProfile
	}
	f.Wheels = append(f.Wheels, req)

	var displaced []SessionID
	for id := range f.sessions {
		if id != req.Session {
			displaced = append(displaced, id)
		}
	}
	for _, id := range displaced {
		delete(f.sessions, id)
	}
	delete(f.sessions, req.Session)

	f.counter++
	session := Session{
		ID:           SessionID(fmt.Sprintf("h%d", f.counter)),
		Mode:         ModeHuman,
		RequestedURL: req.URL,
		FinalURL:     req.URL,
	}
	if f.sessions == nil {
		f.sessions = map[SessionID]Session{}
	}
	f.sessions[session.ID] = session
	f.human = session.ID
	return Wheel{
		Session:   session.ID,
		Mode:      ModeHuman,
		URL:       session.FinalURL,
		Displaced: displaced,
	}, nil
}

// ReturnWheel on the Fake hands back whatever chain a test put in Chain.
func (f *Fake) ReturnWheel(_ context.Context, id SessionID) (Returned, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.human == "" || f.human != id {
		return Returned{}, ErrNoWheelToReturn
	}
	delete(f.sessions, id)
	f.human = ""
	f.Handed = append(f.Handed, id)
	return Returned{Chain: f.Chain}, nil
}
