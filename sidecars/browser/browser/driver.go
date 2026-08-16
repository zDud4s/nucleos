// Package browser is the contract for driving a real browser, and nothing else.
//
// It contains no CDP, no Chrome, and no process management. That is the point: the spike of
// 2026-08-15 changed the answer to "which driver" twice in one afternoon and never touched a line of
// this package. Whatever ends up behind the interface — PinchTab, go-rod, raw CDP — implements
// Driver, and everything above it is already written and already tested against Fake.
//
// # Two properties that are not type-hygiene
//
// OpenRequest carries a Placement, and the agent never fills it in. The agent chooses WHAT to look
// at; the núcleo chooses WHERE it happens (spec §5.3, §6.1) and attaches that decision on the way
// through. An earlier version of this file expressed the same rule by having no profile field at
// all, which was a stronger-looking guarantee and a worse one: with nowhere to put the decision, the
// núcleo could not transmit it, and the choice of identity would have fallen to whichever process
// had a default handy. The boundary is that the field is filled downstream of the agent, not that it
// is absent.
//
// ActResult separates Done from Refused. A refusal by the fence (spec §6.2) is an ANSWER, not a
// failure: the agent asked for something with a consequence, was told so, and can carry on. If it
// arrived as an error it would be indistinguishable from a crashed browser, and the agent would
// retry the one thing it must not.
package browser

import (
	"context"
	"errors"

	"nucleosbrowser/profile"
)

// SessionID identifies one browsing session for its lifetime.
type SessionID string

// Mode is who is holding the wheel. There are exactly two, and there is deliberately no third:
// spec §4.1 forbids a hidden rendering state, so a session is either headless-and-agent-driven or
// visible-and-person-driven, never rendering somewhere nobody can see.
//
// A session whose wheel has been ASKED for is already ModeHuman, before any window exists. That is
// spec §4.4 rule 1 and not an approximation: the refusal is defined to hold "de volante_pedido em
// diante", because the handover kills one process and starts another and an act in that gap would
// land on a page the person is about to inherit. Two values are enough to say that; a third would be
// a state in which it was unclear who the next click belonged to.
type Mode string

const (
	// ModeAgent is headless, fenced, and consequence-free (spec §6.2).
	ModeAgent Mode = "agent"
	// ModeHuman is a real window, with a person in front of it and no fence.
	ModeHuman Mode = "human"
)

// Placement is the núcleo's answer to "as whom": which profile this session runs in, and what that
// profile is allowed to load.
//
// The two travel together because they are one decision. A profile without its site list is a
// browser holding the owner's logins and no rule about where they may be sent (spec §5.4); a site
// list without its profile is a rule nothing enforces. Splitting them across two calls would create
// a window in which one had arrived and the other had not, and that window is a browser with logins
// and no fence.
//
// Origins is empty for an ephemeral profile, and must be: there are no logins in it to protect, and
// a list there would be silently ignored — fence.Policy refuses that rather than accept a security
// control that does nothing.
type Placement struct {
	Profile profile.Ref `json:"profile"`
	Origins []string    `json:"origins,omitempty"`
}

// OpenRequest asks for a session on a URL, in the profile the núcleo chose.
type OpenRequest struct {
	URL string `json:"url"`
	// Placement is filled by the núcleo, never by the agent. See the package comment.
	Placement Placement `json:"placement"`
}

// Session is what a caller gets back. RequestedURL and FinalURL are both reported because the trust
// decision is a conjunction over the two (spec §5.3) — a redirect that lands somewhere else is the
// case the allowlist exists for, and a driver that reported only one of them would make that
// decision impossible to take.
// Refusal is non-nil when the fence stopped the navigation this session was opened for. The session
// still exists and is still addressable — it is simply empty. That is a value and not an error for
// the same reason ActResult separates the two: "this profile does not admit that host" is an answer
// the agent can act on, and the action it should take is to ask elsewhere rather than to retry.
type Session struct {
	ID           SessionID `json:"id"`
	Mode         Mode      `json:"mode"`
	RequestedURL string    `json:"requested_url"`
	FinalURL     string    `json:"final_url"`
	Title        string    `json:"title"`
	Refusal      *Refusal  `json:"refusal,omitempty"`
}

// Element is one thing on the page the agent may refer to.
//
// Ref is a stable handle minted by the driver ("e5"), not a CSS selector. Selectors are long, break
// on a class rename, and invite the agent to synthesise one for an element it never saw. A ref can
// only name something that was actually in a snapshot.
type Element struct {
	Ref  string `json:"ref"`
	Role string `json:"role"`
	Name string `json:"name"`
}

// Snapshot is the accessibility view of a page: what is there and what it is called.
type Snapshot struct {
	SessionID SessionID `json:"session_id"`
	URL       string    `json:"url"`
	Title     string    `json:"title"`
	Elements  []Element `json:"elements"`
}

// ActionKind is the verb. The set is small and closed on purpose (spec §6.2, "consequence-free in
// v1"): every member is something a person could do with a mouse and a keyboard and that cannot, on
// its own, leave the machine.
type ActionKind string

const (
	ActionClick  ActionKind = "click"
	ActionType   ActionKind = "type"
	ActionScroll ActionKind = "scroll"
)

// Action is one attempt to touch the page.
type Action struct {
	Kind ActionKind `json:"kind"`
	Ref  string     `json:"ref"`
	Text string     `json:"text,omitempty"`
}

// Outcome is the shape of an ActResult.
type Outcome string

const (
	OutcomeDone    Outcome = "done"
	OutcomeRefused Outcome = "refused"
)

// Consequence names WHY the fence refused. It is a closed vocabulary rather than a message because
// the núcleo has to be able to tell these apart without reading prose, and because the agent is
// shown the reason — a string assembled at the refusal site would drift into something a page could
// influence.
type Consequence string

const (
	// ConsequenceMethod — anything that is not GET or HEAD (spec §6.2).
	ConsequenceMethod Consequence = "non-get-method"
	// ConsequenceForm — a form submission, whatever its method.
	ConsequenceForm Consequence = "form-submission"
	// ConsequenceChannel — a channel that is not HTTP(S): WebSocket, WebRTC (spec §6.2, §6.2b).
	ConsequenceChannel Consequence = "non-http-channel"
	// ConsequenceServiceWorker — code that would stay in the profile (spec §5.8). Named apart from
	// the method and the scheme because it is the only refusal whose damage OUTLIVES the session:
	// a registered worker runs after the page closes and survives the headless→headful→headless
	// cycle, so an agent shown "blocked" would have no way to tell that from a click that did
	// nothing.
	ConsequenceServiceWorker Consequence = "service-worker"
	// ConsequenceDownload — a GET that would write to disk.
	ConsequenceDownload Consequence = "download"
	// ConsequenceNewTarget — a popup or new tab (spec §5.4).
	ConsequenceNewTarget Consequence = "new-target"
	// ConsequenceScheme — data:, blob:, javascript: (spec §6.2; contained by CSP, not cancellable).
	ConsequenceScheme Consequence = "schemeless-navigation"
	// ConsequenceOffAllowlist — a document from a host the profile does not admit (spec §5.4).
	ConsequenceOffAllowlist Consequence = "off-allowlist"
	// ConsequenceWheelRequested — the agent asked for the wheel, so it no longer has it (spec §4.4
	// rule 1). Refused and NOT queued, and refused from the REQUEST rather than from the window
	// opening: spec §4.2 hands over by killing one process and starting another, which is not atomic,
	// and an act landing in that gap would touch a page the person is about to inherit.
	ConsequenceWheelRequested Consequence = "wheel-requested"
	// ConsequenceLoopback — this machine's own services. Separate from off-allowlist because it is
	// the only refusal aimed at US: the núcleo's HTTP API, the sidecars, and the browser's own
	// debugging port all live on loopback behind a bearer token, and agent mode is launched with
	// --proxy-bypass-list=<-loopback> so that the fence sees loopback at all rather than letting the
	// page reach it directly.
	ConsequenceLoopback Consequence = "loopback"
)

// Refusal is a refusal by the fence: a named consequence, and a detail for the human reading a log.
type Refusal struct {
	Consequence Consequence `json:"consequence"`
	Detail      string      `json:"detail,omitempty"`
}

// ActResult is the answer to an Act. Exactly one of the two states is meaningful, and Refusal is
// non-nil precisely when Outcome is OutcomeRefused — see Valid.
type ActResult struct {
	Outcome Outcome  `json:"outcome"`
	Refusal *Refusal `json:"refusal,omitempty"`
}

// Valid reports whether an ActResult is internally consistent. A driver that returns a refusal with
// no consequence, or a "done" carrying one, has a bug that would otherwise surface as the agent
// being told nothing at all.
func (r ActResult) Valid() bool {
	switch r.Outcome {
	case OutcomeDone:
		return r.Refusal == nil
	case OutcomeRefused:
		return r.Refusal != nil && r.Refusal.Consequence != ""
	default:
		return false
	}
}

// Refused builds a refusal result.
func Refused(consequence Consequence, detail string) ActResult {
	return ActResult{
		Outcome: OutcomeRefused,
		Refusal: &Refusal{Consequence: consequence, Detail: detail},
	}
}

// Done builds a successful result.
func Done() ActResult { return ActResult{Outcome: OutcomeDone} }

// HandoffTicket is the driver's half of passing the wheel: the session is ready to be shown to a
// person. The núcleo turns this into a proposal (spec §4.4); the driver does not decide that a
// human's attention gets spent, it only reports that the session can take one.
type HandoffTicket struct {
	SessionID SessionID `json:"session_id"`
	Mode      Mode      `json:"mode"`
	URL       string    `json:"url"`
	Reason    string    `json:"reason"`
}

// ErrFenceNotAttached is the failure that spec §6.2a exists to force:
//
//	"Se o interceptor não estiver atado, o separador não navega."
//
// A driver returns it rather than opening an unfenced session. It is separate from every other
// error because a fence that is absent is indistinguishable, from the outside, from a fence that is
// open — and the whole non-Acts classification of these tools (spec §6.0) rests on it being there.
var ErrFenceNotAttached = errors.New("browser: fence is not attached, refusing to navigate")

// ErrNoSuchSession is returned for an unknown or already-closed SessionID.
var ErrNoSuchSession = errors.New("browser: no such session")

// ErrNotInstalled means the pinned Chromium is not on disk yet (spec §9.5).
//
// It is a state and not a fault: the download happens when the pillar is activated, takes minutes,
// and retries on its own. Spec §9.5 asks for "indisponível COM A RAZÃO, que é diferente de responder
// 501" — 501 would say browsing is not a thing this build does, and this says it is not a thing this
// machine can do yet. The wrapped text names the revision and the path, because the difference
// between "still downloading" and "the download keeps failing" is the whole of what a person can act
// on.
var ErrNotInstalled = errors.New("browser: the pinned chromium is not installed")

// ErrUnsupported is returned by a driver that cannot do something the contract allows.
var ErrUnsupported = errors.New("browser: unsupported by this driver")

// Driver is the seam. Six verbs, matching spec §6.1.
type Driver interface {
	// Open starts a session. It MUST fail with ErrFenceNotAttached rather than navigate without
	// the fence in place, in agent mode.
	Open(ctx context.Context, req OpenRequest) (Session, error)
	// Snapshot returns the accessibility view. Cheap enough to call between every action.
	Snapshot(ctx context.Context, id SessionID) (Snapshot, error)
	// Act performs one action. A fence refusal is a value, not an error.
	Act(ctx context.Context, id SessionID, action Action) (ActResult, error)
	// Screenshot returns PNG bytes, for a person to look at.
	Screenshot(ctx context.Context, id SessionID) ([]byte, error)
	// Handoff prepares the session to be driven by a person.
	Handoff(ctx context.Context, id SessionID, reason string) (HandoffTicket, error)
	// Close ends the session and releases its profile. It is the only verb of the six that reads
	// nothing from the page, which is why it is the only ReadsOwn tool of the set (spec §6.1a).
	Close(ctx context.Context, id SessionID) error
	// Name identifies the driver in logs and in the health readout.
	Name() string
}
