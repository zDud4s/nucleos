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
	// Writable are the origins this profile may SUBMIT A FORM to, and it is a second list rather
	// than a flag on the first because reading a site and acting as the person on it are different
	// permissions. A person grants the second at the login, next to the first and separately from
	// it — there are sites one wants read and on which one wants nothing submitted.
	//
	// Empty for an ephemeral profile, and it must be, for a reason narrower than Origins': a
	// throwaway has no login in it, so there is nobody for a form to be submitted AS.
	Writable []string `json:"writable,omitempty"`
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
	// Status is the HTTP status the PAGE came back with, and 0 when nothing said.
	//
	// Its absence was the quietest hole in this contract. A 404 IS a page: it has a heading, prose,
	// usually a search box, and it reads as a perfectly ordinary document with words on it. A 500
	// with an empty body reads as an empty page. A 429 reads as whatever the site serves. So an agent
	// sent to find something got a correct reading of a page that was not the one it asked for, and
	// concluded the thing is not there rather than that the request failed — with no `blocked`, no
	// `truncated`, no `still_loading`, and nothing anywhere to contradict it.
	//
	// Zero means NOTHING SAID, and never "fine". A document restored from the back-forward cache, or
	// an about: url, produces no response for the fence to see, and inventing a 200 for those would
	// rebuild the same trap with the sign flipped.
	Status int `json:"status,omitempty"`
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
	// `required`, `focused`. Both halves of the booleans are spelled out rather than left implied,
	// because an absent `checked` is indistinguishable from an element that has no checked state at
	// all — and that is exactly the distinction a checkbox turns on.
	//
	// `focused` is here because `press` with no ref sends its key to whatever has focus, and without
	// this the reading never named the one element the verb was about to act on.
	State []string `json:"state,omitempty"`
	// URL is where a link goes.
	//
	// Its absence was a dead end rather than an inconvenience, because it compounded with the
	// refusal of new windows: a link that opens in a tab is refused, the way onward is `goto`, and
	// `goto` needs an address no reading had ever given. Two links called "Details" were also, to an
	// agent, the same link.
	//
	// Shortened to a path when it points at the page's own origin, which is most of them: it is
	// shorter, it is more legible, and `goto` resolves a relative url against the page anyway. A
	// link to anywhere else carries its whole address, because that is the part worth knowing.
	URL string `json:"url,omitempty"`
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
	// Unread is what is ON the page that the accessibility tree cannot express.
	//
	// The last shape of the failure this whole pillar was built to end. A page drawn into a canvas —
	// a chart, a map, a PDF viewer, a design tool — has nothing in the tree, so the reading comes
	// back correct, short, and with nothing to doubt: no `blocked`, no `truncated`, no
	// `still_loading`. An agent reads "there is nothing here" and it is the one page where that is
	// most confidently wrong.
	//
	// This does not make the content readable. It makes the ABSENCE legible, which is the difference
	// between an agent concluding the answer is not there and an agent knowing to ask a person.
	Unread []Unread `json:"unread,omitempty"`
	// StillLoading has the same meaning here as on Session, and it is here because otherwise the
	// flag could be raised and never lowered.
	//
	// Opening said it, acting said it, and a snapshot — the only thing an agent can do about it —
	// said nothing. There is no `wait` verb, deliberately: waiting is not something an agent should
	// have to spend a turn asking for. So the reading itself carries whether the page has settled,
	// and an agent told "not finished" can take another one and be told it now is.
	StillLoading bool `json:"still_loading,omitempty"`
	// Dialogs are the questions this page put to a PERSON, and the answers it was given instead.
	//
	// alert, confirm, prompt and beforeunload freeze the renderer until something answers them, and
	// with the Page domain enabled that something is this driver rather than Chromium. It answers
	// no. So a page CAN have asked "Delete everything?", been told no, and carried on — and without
	// this the agent reads a page where its click did nothing and concludes the button is broken.
	//
	// On the reading rather than on the act, for the same reason Blocked is: the act did happen.
	Dialogs []Dialog `json:"dialogs,omitempty"`
	// Status has the same meaning as on Session, and is here because a click or a goto replaces the
	// document without producing a new Session — so a reading is the only place the status of the
	// page actually in front of the agent can arrive.
	Status int `json:"status,omitempty"`
}

// Unread is one kind of thing on the page that the accessibility tree does not carry.
//
// A kind and a count, not a list: there is nothing to act on, and what the agent needs to know is
// that the page shows something this reading does not.
type Unread struct {
	Kind  string `json:"kind"`
	Count int    `json:"count"`
}

// Dialog is one question the page asked a person, and the answer the driver gave on their behalf.
//
// Answer is "dismissed" for everything but beforeunload, which is "accepted" — see chrome/dialog.go
// for why those two and not one rule. "unanswered" means the answer itself failed to land, which is
// the one case where the page may still be frozen.
type Dialog struct {
	Kind    string `json:"kind"`
	Message string `json:"message,omitempty"`
	Answer  string `json:"answer"`
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
	// ActionUpload attaches a file to an `<input type=file>` — and the file is one the AGENT WROTE,
	// never one it named on this disk. Text is the contents and Filename is what the site is told it
	// is called; nothing here takes a path, and there is deliberately no way to say one.
	//
	// # Why it carries contents rather than a path, which is the whole design
	//
	// The obvious version takes a filename and reads it from some folder. That folder then has to be
	// bounded, and every bound anybody could name here is either unreachable or wrong. The one the
	// design wanted — the errand's own folder — does not exist on a browsing turn: no browser tool is
	// in `ERRAND_TOOLS`, so the turns that can browse and the turns that have a folder are disjoint
	// sets. The one that is reachable — the files folder, where the owner's uploads and filed mail
	// live — is measurably worse than it looks: nothing on this surface can READ a file from it
	// (`list_files` returns names), so an upload from there would let an agent send out the contents
	// of files it cannot itself see, chosen by a name a sender may have picked.
	//
	// Carrying the contents dissolves the question instead of answering it. There is no folder to
	// escape from, no path to canonicalise, no symlink to chase — and, the part that matters, no new
	// channel: anything an agent can put in Text it could already have typed into a form field with
	// ActionType. Upload is genuinely the type of files.
	//
	// What it does not do is attach a file the owner already has. That is a real limitation and not
	// a step on the way here: it needs a folder a browsing turn can reach, which is a decision about
	// what an errand may do, not about this verb.
	ActionUpload ActionKind = "upload"
)

// Action is one attempt to touch the page.
type Action struct {
	Kind ActionKind `json:"kind"`
	// Ref names the element, and is required for click, type, select and upload. Scroll takes one to
	// bring an element into view and takes none to move the page itself; press takes one to focus
	// before the key and takes none to send it wherever focus already is; back never takes one.
	Ref string `json:"ref"`
	// Text is the verb's argument: the characters for type, the option's label for select, the key's
	// name for press, the direction for a page scroll, the url for goto, and the file's CONTENTS for
	// upload. One field rather than six, because a verb has at most one and naming them apart would
	// only spread the same value over a wider shape.
	Text string `json:"text,omitempty"`
	// Filename is upload's second argument, and it is second because upload is the one verb that
	// genuinely has two: what the file says, and what it is called. They cannot share a field —
	// a convention for splitting one string is exactly the kind of thing a page's words could learn
	// to exploit — so the shape widens rather than the meaning.
	//
	// A NAME and never a path. It is validated as one before anything touches a disk, because it
	// becomes a real filename: see chrome/upload.go.
	Filename string `json:"filename,omitempty"`
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
	// ConsequenceForm — a form submission with a method that has a consequence. It is the same rule
	// as ConsequenceMethod above and a better name for it: a non-GET that produces a DOCUMENT is a
	// form in all but name, and telling the agent "that button submits a form" is worth more than
	// telling it "something was blocked".
	//
	// "Whatever its method" is what this used to say, and it is no longer true. A GET form is a
	// document GET — the same request a link to action?fields would make — and it goes through, as
	// the fence's other rules judge any navigation: refused for its origin if the profile does not
	// admit the host, allowed otherwise. See fence/csp.go for why the CSP stopped saying otherwise.
	//
	// Nor is a POST always this. One leaves — the only kind that does — when five things hold at
	// once: it produces a document, it goes back to the origin the page is on, that origin has a
	// WRITE grant a person gave at the login, and an act on something the reading showed is what
	// caused it. Anything short of all five is this consequence, and the detail says WHICH of the
	// five failed, because they are five different next moves: ask a person for the wheel, act on
	// the form instead of watching the page submit it, or stop trying. See fence/policy.go's
	// decideWrite and chrome/write.go.
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
	// Writes are the form submissions this act actually sent — the ones the fence LET THROUGH.
	//
	// Only what left. A submission the fence refused is a Refusal and not a Write, and conflating
	// the two would make the record of what an agent did as the person include things it did not do.
	//
	// Reported on the act rather than gathered by the núcleo from somewhere else, because the act is
	// the only place that knows both halves: the sidecar saw the request leave, and the núcleo has
	// the session row to file it against. It is a slice and not a single value for the same reason
	// the refusal record has a cursor — one that lands after this act's window is carried to the
	// next one — though here the race is close to impossible in practice: a submission produces a
	// document, and the act waits for that very document to arrive.
	Writes []Write `json:"writes,omitempty"`
}

// Write is one form submission that left this machine, as much of it as is safe to keep.
//
// # What is here, and what is deliberately not
//
// The names of the fields and how many there were. Never the VALUES. A form carries a password, a
// token, a private message; a record of what was submitted would be the most useful audit trail
// there is and would also turn the núcleo's database into a place where credentials come to rest,
// permanently, for every form an agent ever fills. The price is accepted with open eyes and is worth
// stating: knowing that something was submitted to a reply form does not say what the reply said.
//
// It exists because supervision has to be possible AFTERWARDS, given that it deliberately is not
// beforehand — the whole point of the grant is that the agent works alone inside it. Autonomy with
// no record is autonomy with no supervision available at all, which is a different arrangement and
// not the one this pillar wants.
type Write struct {
	// Origin is where it went, in the shape the grant is written in.
	Origin string `json:"origin"`
	// Action is the form's action as the page resolved it, and Method what it was submitted with.
	// Together they are what ties this row to something a person can see on the page.
	Action string `json:"action"`
	Method string `json:"method"`
	// Fields are the NAMES of what was submitted, in document order. Only named fields: a control
	// with no name sends nothing, so a name that is absent here is a field that was absent from the
	// request.
	Fields []string `json:"fields,omitempty"`
	// FieldCount is how many there were in total, which is not len(Fields) when a form is long
	// enough to be truncated. A count that quietly became "the first thirty-two" would be the same
	// shape of quiet wrongness the reading half of this pillar spent itself removing.
	FieldCount int `json:"field_count"`
	// Ref and Verb are the act that caused it — the fifth condition of the write rule, written down
	// rather than asserted, so a row can be read back against the snapshot that produced it.
	Ref  string `json:"ref,omitempty"`
	Verb string `json:"verb,omitempty"`
	// Files are the NAMES of the attachments this submission carried, and never their
	// contents.
	//
	// The same rule as Fields above and for a sharper reason: a form field's value is a line
	// of text somebody typed, and a file is the most concentrated form there is of content
	// that must not come to rest in this database. What the owner needs to see is that a file
	// left and what it was called; what it said is between them and the site they sent it to.
	Files []string `json:"files,omitempty"`
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

// ErrTooManySessions is spec §9.6's ceiling. A browser is hundreds of megabytes and a GPU consumer
// competing with the local model on the same card, so this refuses rather than degrades.
//
// Here and not in `pool`, where it lived, for the reason `ErrPersonIsDriving` is here: `serve`
// translates what comes out of a `Driver`, and a sentinel it cannot name falls into the default arm
// and loses its text. That is not hypothetical. A live session asked for a third tab and the caller
// was told `502: open failed` while the log, one process away, said
// `pool: too many sessions open: 2 of 2`. The reason was written and then dropped at the boundary.
//
// Wrapped with the counts by whoever returns it, because "you are at the ceiling" and "the ceiling
// is two" are different sentences and only the second tells a person what to change.
var ErrTooManySessions = errors.New("browser: too many sessions open")

// LookResult is one annotated picture of the page: what a person would see, with the agent's own
// refs drawn on top of it.
//
// The labels ARE the refs — not a second numbering that has to be translated back. That is the whole
// design, and everything else here follows from it: an agent that can see is not thereby an agent
// that can click at a coordinate, because there is no verb that takes one. What the picture buys is
// a way to find the ref worth acting on when the accessibility tree does not say enough — a canvas,
// a chart, an icon whose label is a sprite.
type LookResult struct {
	// Image is the picture, base64 for the JSON hop, and MIME says what it is.
	Image string `json:"image"`
	MIME  string `json:"mime"`
	// Labels are the refs actually DRAWN, which is a smaller set than the refs the session knows:
	// an element scrolled out of the viewport, or collapsed to nothing, gets no label.
	//
	// Reported rather than left to be read off the picture, for the same reason a snapshot reports
	// what it dropped: an agent comparing what it sees against what it holds needs the difference to
	// be stated, not inferred from an image it may be reading imperfectly.
	Labels []string `json:"labels,omitempty"`
	// Width and Height are the picture's own, after any reduction. Said because a reduction changes
	// what is legible, and an agent that cannot read a label is better off knowing the picture was
	// shrunk than concluding the page had nothing written on it.
	Width  int `json:"width"`
	Height int `json:"height"`
}

// Driver is the seam. Seven verbs, matching spec §6.1 plus the annotated picture.
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
	// Look returns the annotated picture, for the AGENT to look at. Distinct from Screenshot in
	// audience and therefore in everything else: this one is labelled, viewport-only, lossy and
	// bounded, because it is paid for in an agent's context rather than in a person's window.
	Look(ctx context.Context, id SessionID) (LookResult, error)
	// Handoff prepares the session to be driven by a person.
	Handoff(ctx context.Context, id SessionID, reason string) (HandoffTicket, error)
	// Close ends the session and releases its profile. It is the only verb here that reads nothing
	// from the page, which is why it is the only ReadsOwn tool of the set (spec §6.1a).
	Close(ctx context.Context, id SessionID) error
	// Name identifies the driver in logs and in the health readout.
	Name() string
}
