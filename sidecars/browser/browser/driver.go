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
	// StillLoading says the page had not finished arriving when this answer was produced. Opening
	// waits for it — the alternative was a first snapshot that raced the load and read a
	// script-rendered page as an empty one — but the wait is bounded, and when the bound is reached
	// the agent is TOLD rather than handed a silence that looks like readiness.
	StillLoading bool `json:"still_loading,omitempty"`
}

// Element is one thing on the page the agent may refer to.
//
// Ref is a stable handle minted by the driver ("e5"), not a CSS selector. Selectors are long, break
// on a class rename, and invite the agent to synthesise one for an element it never saw. A ref can
// only name something that was actually in a snapshot.
type Element struct {
	// Ref is what an act names. Empty on prose: nothing in the action set does anything to a
	// paragraph, and a ref per paragraph is a dozen tokens each buying a capability that does not
	// exist. Scrolling reaches text by its nearest heading, which does have one.
	Ref  string `json:"ref,omitempty"`
	Role string `json:"role"`
	// Name is what it is called, and on a `text` element it is what it SAYS. One field rather than
	// two because a snapshot is read top to bottom: prose and controls interleave in document order,
	// and splitting them into separate lists would lose which paragraph belongs to which button.
	Name string `json:"name"`
	// Value is what is IN it — the characters in a textbox, the number on a slider. Without it an
	// agent that types cannot read back what it typed, which turns every form into an open loop.
	Value string `json:"value,omitempty"`
	// State is the handful of accessibility properties that change what an act would MEAN, and only
	// those: `checked`/`unchecked`/`mixed`, `disabled`, `expanded`/`collapsed`, `selected`,
	// `required`. Both halves of the booleans are spelled out rather than left implied, because an
	// absent `checked` is indistinguishable from an element that has no checked state at all — and
	// that is exactly the distinction a checkbox turns on.
	State []string `json:"state,omitempty"`
}

// SnapshotRequest is what to read and how much of it.
//
// A struct rather than a growing list of parameters, because every field here is an ANSWER to a
// question about cost: a reading of a whole long page is correct and can be most of a turn, so the
// caller gets to say which part of it it needs.
type SnapshotRequest struct {
	// ChangesOnly asks for what moved since the last snapshot of this session instead of the whole
	// page. The same reading, filtered — never a different one.
	ChangesOnly bool
	// TextFrom resumes prose at a character offset, which is what makes truncation survivable. A
	// snapshot that says it was cut and offers no way to see the rest is a dead end: the agent knows
	// something is there and has no verb that reaches it, because the budget is not about the
	// viewport and no amount of scrolling moves it. Pass back the TextNext of the previous snapshot.
	TextFrom int
	// ControlsFrom resumes the actionable elements, counted rather than measured. The same idea as
	// TextFrom and a separate cursor on purpose: prose and controls fail differently, and bounding
	// them together would mean a long article costing a page its buttons — which is the rule this
	// whole design started from.
	ControlsFrom int
	// Find keeps only the lines that say this, matched case-insensitively against a control's role,
	// name and value and against a paragraph's or a row's text.
	//
	// Paging is not searching. TextFrom and ControlsFrom make a long page READABLE, in order, at a
	// cost proportional to the page; finding one link in a directory of two thousand still meant
	// carrying the two thousand. That is the difference between a page an agent can read and a page
	// it can use, and on anything catalogue-shaped it was the whole turn.
	//
	// It filters what is REPORTED and never what is read: refs are still minted for everything on
	// the page, so an act on something an earlier snapshot showed is not turned stale by a search.
	Find string
}

// Blocked is what the page tried to do for itself and the fence stopped, since this document loaded.
//
// A count and the most recent one, rather than the list. The list is unbounded — a page that polls
// produces one every second — and after the first the agent has learned everything it can act on:
// that what it is reading may be less than the page.
type Blocked struct {
	Count       int         `json:"count"`
	Consequence Consequence `json:"consequence"`
	Detail      string      `json:"detail,omitempty"`
}

// Snapshot is the accessibility view of a page: what is there, what it is called, and what it says.
type Snapshot struct {
	SessionID SessionID `json:"session_id"`
	URL       string    `json:"url"`
	Title     string    `json:"title"`
	Elements  []Element `json:"elements"`
	// Truncated says the text budget ran out and the page continues past the last element here.
	// Said rather than implied: an agent that cannot tell a short page from a cut-off one will
	// conclude the rest does not exist, which is a worse failure than being told to scroll.
	Truncated bool `json:"truncated,omitempty"`
	// TextNext is where the prose stopped, and is what to pass as TextFrom to read on. Set only when
	// Truncated is, so its presence is the offer and its absence means there is nothing left.
	TextNext int `json:"text_next,omitempty"`
	// ControlsNext is the same offer for the actionable elements.
	//
	// It exists because "controls are never dropped" stopped being a kindness at some size. The rule
	// was written against prose crowding out a button, and it is right for that; on a directory
	// listing with two thousand links it meant a snapshot with no bound at all, reported as
	// untruncated because the prose had fit. The failure did not surface as an error — it surfaced
	// as a turn with no room left to think in.
	ControlsNext int `json:"controls_next,omitempty"`
	// Gone lists refs that were in the previous snapshot and are not on the page now. Only filled on
	// a changes-only read, where it is the half that omission cannot express: a full snapshot says
	// an element is gone by not containing it, and a partial one cannot say anything by silence.
	Gone []string `json:"gone,omitempty"`
	// Partial says this snapshot is not the whole page — the difference since the last one, or what
	// matched a Find — so an agent does not read a short list as a short page.
	Partial bool `json:"partial,omitempty"`
	// Blocked is the fence's third layer speaking. The CSP stops things inside the renderer, where
	// no request is ever made and there is nothing for the other two layers to report, so a page
	// that could not fetch its own content used to read as a page that had none.
	Blocked *Blocked `json:"blocked,omitempty"`
	// StillLoading has the same meaning here as on Session, and it is here because otherwise the
	// flag could be raised and never lowered.
	//
	// Opening said it, acting said it, and a snapshot — the only thing an agent can do about it —
	// said nothing. There is no `wait` verb, deliberately: waiting is not something an agent should
	// have to spend a turn asking for. So the reading itself carries whether the page has settled,
	// and an agent told "not finished" can take another one and be told it now is.
	StillLoading bool `json:"still_loading,omitempty"`
}

// ActionKind is the verb. The set is small and closed on purpose (spec §6.2, "consequence-free in
// v1"): every member is something a person could do with a mouse and a keyboard and that cannot, on
// its own, leave the machine.
type ActionKind string

const (
	ActionClick  ActionKind = "click"
	ActionType   ActionKind = "type"
	ActionScroll ActionKind = "scroll"
	// ActionSelect chooses an option in a dropdown. Its absence was not a missing convenience: the
	// snapshot hands out refs for `combobox` and `listbox`, so the agent was being shown a control
	// and given no verb that operates it — an invitation to click at it and to read whatever
	// happened next as success.
	ActionSelect ActionKind = "select"
	// ActionPress sends one key to whatever has focus. Typing uses Input.insertText, which is what a
	// paste does and therefore fires no keydown at all: a search box that submits on Enter could not
	// be submitted, and a field that watches keystrokes saw none. The key set is closed and carries
	// no modifiers — see the driver — because Ctrl+S is a download and Ctrl+P is a dialog, and
	// neither is consequence-free.
	ActionPress ActionKind = "press"
	// ActionBack returns to the previous page. Without it an agent that followed the wrong link
	// could only re-open the url it wanted, which it may not have, and which pays the admission
	// check again. Back can only reach a document this session already loaded, and the fence already
	// admitted every one of those.
	ActionBack ActionKind = "back"
	// ActionGoto follows a url in the session that is already open. Its absence was visible only
	// once back existed: there was a way home and no way onward, so an agent that read an address in
	// the page's own words — not a link, an address — had to open a SECOND session for it, paying a
	// fresh profile decision and losing the history it would need to come back.
	//
	// It grants nothing a link does not. The url is a navigation like any other, so the fence's
	// allowlist answers for it exactly as it answers for a link the page itself offers, and the
	// scheme is checked here as well because file: and data: are not requests the interception sees.
	ActionGoto ActionKind = "goto"
)

// Action is one attempt to touch the page.
type Action struct {
	Kind ActionKind `json:"kind"`
	// Ref names the element, and is required for click, type and select. Scroll takes one to bring
	// an element into view and takes none to move the page itself; press takes one to focus before
	// the key and takes none to send it wherever focus already is; back never takes one.
	Ref string `json:"ref"`
	// Text is the verb's argument: the characters for type, the option's label for select, the key's
	// name for press, the direction for a page scroll, and the url for goto. One field rather than
	// five, because a verb has at most one and naming them apart would only spread the same value
	// over a wider shape.
	Text string `json:"text,omitempty"`
}

// Outcome is the shape of an ActResult.
type Outcome string

const (
	OutcomeDone    Outcome = "done"
	OutcomeRefused Outcome = "refused"
)

// Consequence names WHY an act did not happen. Mostly that is the fence; two of them are not, and
// they are here rather than expressed as errors because they are the same KIND of answer — the act
// did not occur, the agent is told plainly why, and it can carry on.
//
// It is a closed vocabulary rather than a message because the núcleo has to be able to tell these
// apart without reading prose, and because the agent is shown the reason — a string assembled at the
// refusal site would drift into something a page could influence.
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
	// ConsequenceStaleRef — the ref names nothing in the current snapshot. Not a fence refusal at
	// all, and it used to be reported as off-allowlist, which told the agent a security decision had
	// been taken about a page when what had actually happened was that the page moved. The right
	// next move is a fresh snapshot, and the two answers point in opposite directions.
	ConsequenceStaleRef Consequence = "stale-ref"
	// ConsequenceNotApplicable — the verb does not apply here: a select on something that is not a
	// dropdown, a key outside the closed set, a back with nothing behind it. Also not the fence.
	ConsequenceNotApplicable Consequence = "not-applicable"
	// ConsequencePageRequest — the page tried to make a request of its OWN, without navigating:
	// fetch, XHR, EventSource, a beacon. The fence allows none of them (`connect-src 'none'`), and
	// this is the name for having been stopped by that.
	//
	// It arrives on a snapshot rather than on an act, because the failure it describes is one of
	// READING. A page that renders empty and fills itself from an API produces a shell, and a shell
	// is a correct reading of an empty page — so without this the agent concludes there is nothing
	// there, and nothing anywhere contradicts it.
	ConsequencePageRequest Consequence = "page-request"
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
	// Navigated says the act replaced the document, so every ref the agent is holding names
	// something that is gone. Said rather than left to be discovered: an act reported only as "done"
	// after a click that changed the page leaves the agent operating a page it has never read, and
	// the ref it acts on next resolves against a document that no longer exists.
	Navigated bool `json:"navigated,omitempty"`
	// URL is where the page ended up, filled only when the act moved it. Only then, because that is
	// the only moment it is NEWS — the rest of the time a snapshot already carries it, and paying a
	// round trip per act to repeat something unchanged is how a cheap verb stops being cheap.
	URL string `json:"url,omitempty"`
	// StillLoading has the same meaning as on Session, for the page this act navigated to.
	StillLoading bool `json:"still_loading,omitempty"`
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
	// Snapshot returns the accessibility view. Cheap enough to call between every action, and the
	// request says which part of it is wanted — see SnapshotRequest.
	Snapshot(ctx context.Context, id SessionID, req SnapshotRequest) (Snapshot, error)
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
