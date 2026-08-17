// Package profile owns the directories a browser runs in, and the naming that keeps two of them
// apart.
//
// A profile is the identity (spec §4.2). The process is disposable — it is closed and relaunched to
// hand the wheel over — but the cookies, the logged-in sessions, the localStorage and, since §5.8,
// executable code all live in the directory and survive. So "which profile" is not a placement
// question. It is the same question as "as whom".
//
// Two kinds, and the distance between them is the whole trust boundary of spec §5.1: a project
// profile persists and holds the logins a person made; an ephemeral one is born for a run and dies
// with it. Everything in this package exists to keep the second from becoming the first by accident,
// and to keep the first from being deleted by accident.
//
// # Why an ID is validated rather than trusted
//
// A Ref arrives over the wire from the núcleo and becomes a DIRECTORY NAME. Between those two facts
// sits every path-traversal bug ever written: an ID of "../chromium-1234" turns a profile directory
// into the browser binary's directory, and the caller that removes an ephemeral profile removes the
// browser instead. The núcleo is trusted, and the validation is here anyway — the cost is a loop over
// sixty-four characters, and the thing on the other side of the assumption is a recursive delete.
package profile

import (
	"errors"
	"fmt"
)

// Kind is which of the two a profile is. There is no third and no default: see ErrNoKind.
type Kind string

const (
	// Project persists between runs and holds the logins. Spec §5.2 lets its site list grow only by
	// a human logging in.
	Project Kind = "project"
	// Ephemeral is a throwaway. It carries no site list at all, which is why it may load anything:
	// there is nothing in it to steal.
	Ephemeral Kind = "ephemeral"
)

// MaxIDLength bounds an ID. Long enough for a UUID, short enough that the resulting path is nowhere
// near a filesystem limit even under a deep %LOCALAPPDATA%.
const MaxIDLength = 64

// ErrNoKind refuses to guess.
//
// The two defaults available are both wrong in a way that is invisible: defaulting to Ephemeral
// silently loses the person's logins, and defaulting to Project silently runs a stranger's page in
// the profile that holds them. A caller that did not say which one it meant has a bug, and this is
// where it surfaces.
var ErrNoKind = errors.New("profile: no kind, refusing to guess between a throwaway and the one with the logins in it")

// ErrBadID rejects an ID that cannot safely be a directory name.
var ErrBadID = errors.New("profile: id is not usable as a directory name")

// ErrPersistent refuses to delete a profile that holds logins. See Store.Discard.
var ErrPersistent = errors.New("profile: refusing to delete a project profile")

// Ref names one profile. It is the núcleo's decision (spec §5.3) travelling as a value — the agent
// never constructs one, which is why browser.OpenRequest carries it separately from the URL.
type Ref struct {
	Kind Kind   `json:"kind"`
	ID   string `json:"id"`
}

// Validate reports whether this Ref may be turned into a directory.
func (r Ref) Validate() error {
	switch r.Kind {
	case Project, Ephemeral:
	default:
		return fmt.Errorf("%w: got %q", ErrNoKind, r.Kind)
	}
	if r.ID == "" {
		return fmt.Errorf("%w: empty", ErrBadID)
	}
	if len(r.ID) > MaxIDLength {
		return fmt.Errorf("%w: %d characters, the limit is %d", ErrBadID, len(r.ID), MaxIDLength)
	}
	for _, char := range r.ID {
		if !allowed(char) {
			return fmt.Errorf("%w: %q contains %q", ErrBadID, r.ID, char)
		}
	}
	return nil
}

// allowed is a whitelist, and deliberately excludes the dot.
//
// Excluding '.' costs nothing — no NucleOS identifier contains one — and removes "..", the leading
// dot that hides a directory, and the trailing dot Windows silently strips (which would make "a." and
// "a" the same profile). A blacklist here would have to be right about every one of those; a
// whitelist only has to be right about what an ID actually looks like.
//
// Uppercase is excluded for a subtler reason, spelled out in Name.
func allowed(char rune) bool {
	switch {
	case char >= 'a' && char <= 'z':
		return true
	case char >= '0' && char <= '9':
		return true
	case char == '-' || char == '_':
		return true
	default:
		return false
	}
}

// Name is the directory name for this profile, matching spec §5.6's layout.
//
// # Why uppercase is refused rather than folded
//
// We compare IDs exactly; Windows compares directory names case-insensitively. So a project "Acme"
// and a project "acme" are two identities to the núcleo and ONE directory on disk — they would share
// cookies, which is precisely the cross-identity bleed §5.1 exists to prevent. Lowercasing the ID
// here would produce the same collision while hiding it. Refusing at the door produces a loud error
// at integration time instead, and the fix is one call to to_lowercase on the Rust side.
func (r Ref) Name() string {
	if r.Kind == Ephemeral {
		// "run-" and not "ephemeral-": the ID is a run ID, and the sweeper of spec §9.3 matches on
		// this prefix. Two spellings of the same idea would leave orphans behind forever.
		return "run-" + r.ID
	}
	return "project-" + r.ID
}

// EphemeralPrefix is what Store.SweepEphemeral matches on. Exported so the sweeper's rule and the
// naming above can be tested as the single fact they are.
const EphemeralPrefix = "run-"

// Persistent reports whether this profile survives its run.
func (r Ref) Persistent() bool { return r.Kind == Project }

// String is for logs. It never includes anything from the page.
func (r Ref) String() string { return string(r.Kind) + ":" + r.ID }
