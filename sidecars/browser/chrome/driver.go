// §spec pilar-de-browser

// Package chrome implements browser.Driver over CDP.
//
// # The fence is a constructor, not a flag
//
// Spec §6.2a: "se o interceptor não estiver atado, o separador não navega." The strongest way to say
// that in Go is to make an unfenced Driver impossible to hold. [Connect] attaches the interception
// and returns an error if it cannot; there is no exported way to build a Driver otherwise. So there
// is no `if !d.fenced` to forget, and no window between construction and arming.
//
// The interception goes on the BROWSER session (see cdp.SessionID). The spike measured that a
// page-session fence never sees a service worker's script fetch at all.
//
// The policy the fence enforces lives in the fence package, pure and table-tested. This package is
// the wiring: it decides nothing, and every rule it applies comes from there.
package chrome

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
	"nucleosbrowser/fence"
)

// Driver drives one browser. Holding one means the fence is attached.
type Driver struct {
	conn   *cdp.Conn
	policy fence.Policy

	mu       sync.Mutex
	sessions map[browser.SessionID]*session
	counter  int
	// targets maps a CDP target to the session that owns it, so a popup can be traced back to the
	// session whose page opened it.
	targets map[string]browser.SessionID
	// cdpToSession maps an attached CDP session to ours, so a refusal raised by the fence can be
	// reported against the act that caused it. Not every CDP session has an entry — see
	// recordedRefusal.
	cdpToSession map[cdp.SessionID]browser.SessionID
	// contexts is where each of a page's worlds is, by the origin Chromium reports for it. The
	// ferry's same-origin rule reads it from here and never from the page — a restriction the
	// restricted thing gets to describe is not one.
	contexts map[contextKey]executionContext
	// casts are the live screencasts, by the page session they run on, guarded by mu. castCtl
	// serialises starting and stopping them, so a stop that follows the last viewer leaving cannot
	// land on the stream a new viewer has just started.
	casts   map[cdp.SessionID]*screencast
	castCtl sync.Mutex

	refusals     []recordedRefusal
	refusalTotal int

	// gate is what makes the person's turn a swap and not a race. Every exported agent verb holds it
	// for READING for as long as it runs; BeginPerson and EndPerson hold it for WRITING, so a verb in
	// flight finishes first and none starts in the middle of the swap. Taken at exported entry points
	// only, and no exported method calls another, so it never recurses under a waiting writer.
	gate sync.RWMutex
	// person is the person's turn, or nil. Read without a lock by the fence, which answers requests
	// from goroutines that must never wait on the gate: a paused request nobody answers wedges the
	// renderer the swap itself is waiting on.
	person atomic.Pointer[personState]
	// personBegun is set by BeginPerson inside the same d.mu critical section as its sole-session
	// check, and Open refuses to insert a session while it is set. Guarded by mu. Unlike person it is
	// true from the moment the fence is about to lift, so an Open that is mid-flight cannot slip in.
	personBegun bool

	// visible marks a driver whose browser has a window a person looks at (the panel). personSwitch is
	// the proxy's person switch, moved by BeginPerson/EndPerson; personTargets are the popups the
	// person opened, by target id, closed by EndPerson. All three are set by MakeVisible before any
	// Open, personTargets guarded by mu.
	visible       bool
	personSwitch  func(bool)
	personTargets map[string]cdp.SessionID

	// swept closes when the profile has been cleared of service workers, and sweepErr says whether
	// that succeeded. Open waits on it — see waitForSweep. sweepErr is written before the close and
	// read only after it, which is what makes it safe without a lock.
	swept    chan struct{}
	sweepErr error

	// How long a page is given to arrive, and how long after its load event it is given to go
	// quiet. Fields rather than constants so a test can shorten them: the interesting case is the
	// page that never finishes, and asserting it against the real bound would cost the suite
	// fifteen seconds to learn nothing it could not learn in fifty milliseconds.
	readyWithin time.Duration
	idleGrace   time.Duration
	// settleWithin is how long an act that did not navigate waits to see whether it started
	// anything. Paid once per such act, so a test that asserts the negative case asserts it against
	// a short one rather than adding a third of a second to every click in the suite.
	settleWithin time.Duration
	// movingWithin caps how long a page may hold that wait by redrawing. See movingBound.
	movingWithin time.Duration
	// promptTimeout is how long a prompt put to a person waits before it is cancelled.
	promptTimeout time.Duration
}

type session struct {
	id        browser.SessionID
	target    string
	cdp       cdp.SessionID
	mode      browser.Mode
	requested string
	final     string
	title     string
	// frameID is this target's top frame, kept so a wait for "the page is ready" can ignore the
	// lifecycle of every subframe. A page whose advertisement finished loading has not finished.
	frameID string
	// changedAt is when the page last told us it redrew itself, which is the only signal there is
	// for an act that changes a page without asking us for anything — a menu opening, a route
	// rendering from data already in memory, a list filtering itself.
	changedAt time.Time
	// reportedUpTo is how far into the refusal record this session has already been told. It is what
	// makes a refusal that lands after an act's settle window arrive on the NEXT act instead of being
	// lost — no fixed window can catch every one, and silence is the wrong failure.
	reportedUpTo int
	// refs maps a snapshot ref ("e5") to the node it named. The agent may only act on something a
	// snapshot actually showed it — see Act.
	refs map[string]nodeKey
	// refByNode is what makes a ref MEAN the same thing twice, and it is the reason the two fields
	// are not one. Refs used to be minted by position — first interesting node is e1 — so a snapshot
	// taken after a click renumbered the page, and an agent holding "e5" from the previous one was
	// holding a name for something else. Keying on the backend node id, which Chromium keeps stable
	// for the life of the node, makes a ref a handle on an ELEMENT rather than on a position, which
	// is what the agent already assumed it was.
	refByNode map[nodeKey]string
	// frames are the documents Chromium decided to run in their own processes, and the session each
	// one is framed by. A cross-site iframe is a separate target with a separate accessibility tree,
	// so without this the snapshot stops at the process boundary — see Snapshot.
	frames map[cdp.SessionID]frameRef
	// mintedRefs counts how many have ever been handed out for this session, so a ref is never
	// reused for a different node after the first one leaves the page.
	mintedRefs int
	// lastReported is the previous snapshot by ref, kept so the next one can say what changed. Only
	// what was actually sent: comparing against something the agent never saw would report changes
	// it cannot reconcile.
	lastReported map[string]browser.Element
	// blocked counts what the injected CSP stopped in THIS document, and blockedLast is the most
	// recent one. Per document, and reset when it changes: the count answers "is what I am reading
	// the whole page", and an answer carried over from the previous page does not.
	blocked     int
	blockedLast browser.Refusal
	// ferried counts the requests this document has asked us to carry, so a page that polls cannot
	// have us carrying its traffic forever. carrying is how many are in flight right now, which is
	// what a wait for "the page is ready" has to include: a ferried request does not go through the
	// browser's network stack, so `networkAlmostIdle` fires while it is still on its way.
	ferried  int
	carrying int
	// ferryEpoch names the document carrying counts against. forgetRefs zeroes carrying when the
	// document changes while requests the old one asked for are still on their way; each of those
	// counts itself down only if the epoch it counted up in is still current, or the count would go
	// negative and a later page would be reported ready while its own requests were in flight.
	ferryEpoch int
	// dialogs are the questions THIS document put to a person and the answers it was given in their
	// place. Per document and reset with the rest: "the page asked me to confirm something" is a fact
	// about the page being read, not about the one before it.
	dialogs []browser.Dialog
	// status is the HTTP status of the last main-frame document response, or 0 for a page that never
	// produced one. See recordStatus for why it is not reset with the rest of this struct.
	status int
	// mayWrite is the write window one act opens: the permission for a single form submission, for
	// as long as that act lasts. Nil the rest of the time, which is the ordinary state — see
	// chrome/write.go for why the lifetime is the act's and not a number of milliseconds.
	mayWrite *writeWindow
	// attachDir is where this session's uploads were written, or empty until one is. Per session
	// and not per act, because Chromium reads a file input's file when the FORM IS SUBMITTED — a
	// later act than the one that attached it — so a directory cleaned up when the upload returned
	// would be a form that submits nothing and reports success. Removed by Close.
	attachDir string
	// writes are the form submissions this session has actually sent and not yet reported. NOT reset
	// with the rest of the per-document state: a submission navigates, so resetting on navigation
	// would throw away the record of the very thing that caused it.
	writes []browser.Write
}

// contextKey names one execution context. The id is unique within a target and not across them, so
// the session is part of the key — the same mistake, one layer up, as the backend node ids that
// made a ref mean two things.
type contextKey struct {
	session cdp.SessionID
	id      int64
}

// nodeKey identifies a node across every document a session can see.
//
// The backend node id alone is not enough, and the reason is the same process boundary that hid
// cross-site frames from the snapshot: the id is minted by a RENDERER, and a cross-site frame is a
// different renderer, so two documents in one session can hand out the same number for two
// unrelated elements. Keying on the session as well is what keeps a ref a handle on one element
// rather than on whichever document answered last.
type nodeKey struct {
	session cdp.SessionID
	backend int64
}

// frameRef is a document in its own process: its target id, and the session that frames it.
//
// Parent is recorded rather than assumed to be the page, because frames nest. It is read off the
// session the attach arrived ON, which is the only place the relationship is stated.
type frameRef struct {
	target string
	parent cdp.SessionID
}

// Connect attaches the fence and returns a Driver.
//
// The order below is the security argument, and it is asserted by a test rather than trusted: the
// policy is checked before a single call goes out, interception is armed before auto-attach, and no
// target is created by this function at all. A Driver that returned successfully without all of that
// landing would be an unfenced browser wearing the type that promises otherwise.
func Connect(ctx context.Context, conn *cdp.Conn, policy fence.Policy) (*Driver, error) {
	// First, and before touching the browser. A driver that armed the interception and then found
	// out it had no policy to enforce would be holding an open fence for as long as it took to
	// notice — and the tempting fix, a default policy, is the permissive one.
	if err := policy.Validate(); err != nil {
		return nil, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	if _, err := conn.Call(ctx, cdp.BrowserSession, "Fetch.enable", map[string]any{
		"patterns":           []map[string]any{{"urlPattern": "*"}},
		"handleAuthRequests": true,
	}); err != nil {
		return nil, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	// waitForDebuggerOnStart is what closes the TOCTOU window of spec §5.4: a new target is born
	// paused, with an empty url, and the interception is already on before it may navigate.
	if _, err := conn.Call(ctx, cdp.BrowserSession, "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": true,
		"flatten":                true,
	}); err != nil {
		return nil, fmt.Errorf("%w: %v", browser.ErrFenceNotAttached, err)
	}

	// Downloads (spec §6.2, test 8). Fatal rather than best-effort, and the trade-off is deliberate:
	// the response-stage Content-Disposition check would still catch most of them, so refusing to
	// start here costs availability on a Chrome that renamed this method. §6.2a says a fence that is
	// not fully there does not get to look like one, and this is that rule applied to the one
	// mechanism that covers a download the interception never sees.
	if _, err := conn.Call(ctx, cdp.BrowserSession, "Browser.setDownloadBehavior", map[string]any{
		"behavior": "deny",
	}); err != nil {
		return nil, fmt.Errorf("%w: downloads not denied: %v", browser.ErrFenceNotAttached, err)
	}

	driver := &Driver{
		conn:         conn,
		policy:       policy,
		sessions:     map[browser.SessionID]*session{},
		targets:      map[string]browser.SessionID{},
		cdpToSession: map[cdp.SessionID]browser.SessionID{},
		contexts:     map[contextKey]executionContext{},
		casts:        map[cdp.SessionID]*screencast{},
		swept:        make(chan struct{}),
		readyWithin:  readyDeadline,
		idleGrace:    idleGrace,
		settleWithin: settleGrace,
		movingWithin: movingBound,

		promptTimeout: promptTimeoutDefault,
	}
	conn.OnEvent(driver.onEvent)
	conn.OnEvent(driver.onFetchPaused)
	conn.OnEvent(driver.onAuthRequired)
	conn.OnEvent(driver.onLogEntry)
	conn.OnEvent(driver.onRuntimeEvent)
	// Subscribed here, with the others, and not when a page opens: a dialog that arrives with nobody
	// listening leaves the renderer frozen for the life of the session, and there is no later moment
	// from which that can be recovered.
	conn.OnEvent(driver.onDialog)
	conn.OnEvent(driver.onScreencastFrame)
	conn.OnEvent(driver.onFileChooser)
	driver.startSweep()
	return driver, nil
}

func (d *Driver) Name() string { return "chrome" }

// Policy returns the fence this driver enforces, for a health readout to show.
func (d *Driver) Policy() fence.Policy { return d.policy }

// onEvent services targets that are born paused.
//
// Auto-attach is HIERARCHICAL: a browser-level subscription is offered pages, but a page's own
// out-of-process iframes are only offered to whoever auto-attached on THAT page's session. The
// spike measured this — a cross-site iframe never appeared as a target until each session re-armed
// on its own children — so every attached target re-arms before it is released.
func (d *Driver) onEvent(event cdp.Event) {
	if event.Method == "Target.detachedFromTarget" {
		d.onDetached(event)
		return
	}
	if event.Method != "Target.attachedToTarget" {
		return
	}
	var params struct {
		SessionID  cdp.SessionID `json:"sessionId"`
		TargetInfo struct {
			TargetID string `json:"targetId"`
			Type     string `json:"type"`
			OpenerID string `json:"openerId"`
			URL      string `json:"url"`
		} `json:"targetInfo"`
		WaitingForDebugger bool `json:"waitingForDebugger"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	// Re-arm on this target's own children before letting it run.
	_, _ = d.conn.Call(ctx, params.SessionID, "Target.setAutoAttach", map[string]any{
		"autoAttach":             true,
		"waitForDebuggerOnStart": true,
		"flatten":                true,
		"filter":                 []map[string]any{{}},
	})

	// A popup belongs to the session whose page opened it. Identified by openerId and never by
	// arrival order: the spike watched Chrome raise three page attaches for one window.open, and a
	// "first new target" heuristic bound the fence to the wrong one — silently, which is the worst
	// way for a fence to be wrong.
	owner := browser.SessionID("")
	if params.TargetInfo.OpenerID != "" {
		d.mu.Lock()
		if known, ok := d.targets[params.TargetInfo.OpenerID]; ok {
			owner = known
			d.targets[params.TargetInfo.TargetID] = known
			d.cdpToSession[params.SessionID] = known
		}
		d.mu.Unlock()
	}

	// A frame Chromium chose to run somewhere else. It is tied to its parent by the session the
	// attach ARRIVED on — auto-attach is re-armed per target and flattened, so this event is
	// delivered on the session that frames it and nowhere else. Nothing about the child's url says
	// who framed it, and guessing from arrival order is the mistake the popup comment above records.
	if params.TargetInfo.Type == "iframe" {
		d.mu.Lock()
		if known, ok := d.cdpToSession[event.Session]; ok {
			d.cdpToSession[params.SessionID] = known
			if entry, live := d.sessions[known]; live {
				entry.frames[params.SessionID] = frameRef{
					target: params.TargetInfo.TargetID,
					parent: event.Session,
				}
			}
		}
		d.mu.Unlock()
		// A framed login form that cannot fetch is the case this pillar exists for, so the frame's
		// log is listened to as well as the page's, and it gets the ferry too.
		d.watchCSP(ctx, params.SessionID)
		d.armFerry(ctx, params.SessionID)
	}

	// Spec §5.4: in agent mode a new target is BLOCKED, not opened and then watched. A headless
	// popup is invisible by construction (§4.1), so there is no mode in which showing it would be
	// honest. --block-new-web-contents already makes window.open return null; this is the second
	// mechanism, and it is the one a test can assert without trusting a command-line flag.
	//
	// In a visible window the person's own popup is the exception: it is let through and resumed, and
	// EndPerson closes it. The agent's popup stays closed while PAUSED (spike G4d: a resumed page has
	// already run its first script), so that close must stay before any resume below.
	if params.TargetInfo.Type == "page" && params.TargetInfo.OpenerID != "" {
		if d.visible && owner != "" && d.personHolds(owner) {
			d.mu.Lock()
			d.personTargets[params.TargetInfo.TargetID] = params.SessionID
			d.mu.Unlock()
			if params.WaitingForDebugger {
				_, _ = d.conn.Call(ctx, params.SessionID, "Runtime.runIfWaitingForDebugger", nil)
			}
			return
		}
		d.recordSessionRefusal(owner, browser.ConsequenceNewTarget,
			"a page tried to open a new window; agent mode does not have one to show")
		_, _ = d.conn.Call(ctx, cdp.BrowserSession, "Target.closeTarget", map[string]any{
			"targetId": params.TargetInfo.TargetID,
		})
		return
	}

	if params.WaitingForDebugger {
		_, _ = d.conn.Call(ctx, params.SessionID, "Runtime.runIfWaitingForDebugger", nil)
	}
}

// onDetached forgets a frame that went away.
//
// Without it a navigation would leave the previous document's frames on the session forever, and
// the next snapshot would spend a round trip on each before being told the target is gone. Read as
// bookkeeping, not as cleanup: an entry left behind does not produce a WRONG reading, it produces a
// slower one and a growing map.
func (d *Driver) onDetached(event cdp.Event) {
	var params struct {
		SessionID cdp.SessionID `json:"sessionId"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil {
		return
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if owner, ok := d.cdpToSession[params.SessionID]; ok {
		if entry, live := d.sessions[owner]; live {
			delete(entry.frames, params.SessionID)
		}
	}
	delete(d.cdpToSession, params.SessionID)
}

// Open creates a target, arms it while it is still paused, and only then navigates.
func (d *Driver) Open(ctx context.Context, req browser.OpenRequest) (browser.Session, error) {
	// Before a target exists, let alone navigates. Spec §5.8 says the profile is swept "ao abrir",
	// and the reason it is here rather than in Connect is that this is the line the requirement is
	// actually about: nothing may render while a worker somebody else registered is still live.
	if err := d.waitForSweep(ctx); err != nil {
		return browser.Session{}, err
	}

	targetID, cdpSession, err := d.createPage(ctx)
	if err != nil {
		return browser.Session{}, err
	}
	target := struct{ TargetID string }{TargetID: targetID}

	if _, err := d.conn.Call(ctx, cdpSession, "Page.enable", nil); err != nil {
		return browser.Session{}, fmt.Errorf("enabling page domain: %w", err)
	}
	// Best effort, both of them. Without lifecycle events a wait falls back to the load event, which
	// is earlier than it should be but is not wrong; refusing to open over it would trade a real
	// capability for a better wait.
	_, _ = d.conn.Call(ctx, cdpSession, "Page.setLifecycleEventsEnabled", map[string]any{"enabled": true})
	d.watchCSP(ctx, cdpSession)
	// Before the navigation, because the shim has to be in place before the document that will use
	// it exists. Arming after would leave the first page — the one the agent asked for — unserved.
	d.armFerry(ctx, cdpSession)
	mainFrame := d.mainFrameOf(ctx, cdpSession)

	d.mu.Lock()
	d.counter++
	id := browser.SessionID(fmt.Sprintf("s%d", d.counter))
	entry := &session{
		id:           id,
		target:       target.TargetID,
		cdp:          cdpSession,
		mode:         browser.ModeAgent,
		requested:    req.URL,
		frameID:      mainFrame,
		refs:         map[string]nodeKey{},
		refByNode:    map[nodeKey]string{},
		frames:       map[cdp.SessionID]frameRef{},
		lastReported: map[string]browser.Element{},
		// Anything refused before this session existed belongs to the sweep or to another session.
		reportedUpTo: d.refusalTotal,
	}
	if d.personBegun {
		// A person was handed the browser while this target was being made. The sole-session check
		// did not see it, so it is refused here, before anything navigates with the fence lifted.
		d.mu.Unlock()
		closing, cancel := context.WithTimeout(context.Background(), fenceCallTimeout)
		defer cancel()
		_, _ = d.conn.Call(closing, cdp.BrowserSession, "Target.closeTarget", map[string]any{
			"targetId": target.TargetID,
		})
		return browser.Session{}, browser.ErrPersonIsDriving
	}
	d.sessions[id] = entry
	d.targets[target.TargetID] = id
	d.cdpToSession[cdpSession] = id
	d.mu.Unlock()

	// Counted BEFORE the navigation, so a refusal the fence raises while the navigation is in
	// flight is attributable to it and not lost.
	before := d.refusalCount()

	// Subscribed before the navigate call for the same reason: a page that finishes quickly would
	// otherwise fire everything worth hearing before anything was listening.
	ready := d.watchPage(cdpSession, mainFrame)
	defer ready.stop()

	navigated, err := d.conn.Call(ctx, cdpSession, "Page.navigate", map[string]any{"url": req.URL})
	if err != nil {
		return browser.Session{}, fmt.Errorf("navigating: %w", err)
	}
	var outcome struct {
		ErrorText string `json:"errorText"`
	}
	if err := json.Unmarshal(navigated, &outcome); err != nil {
		return browser.Session{}, err
	}

	if outcome.ErrorText != "" {
		if refused := d.refusalFor(ctx, id, before); refused != nil {
			// The session exists and is empty. Reported as a value rather than an error because it
			// is an ANSWER: the agent asked for a url this profile does not admit, and the useful
			// next move is to ask for it in an ephemeral one — not to retry.
			return browser.Session{
				ID:           id,
				Mode:         entry.mode,
				RequestedURL: entry.requested,
				Refusal:      &browser.Refusal{Consequence: refused.Consequence, Detail: refused.Detail},
			}, nil
		}
		return browser.Session{}, fmt.Errorf("navigating to %s: %s", req.URL, outcome.ErrorText)
	}

	stillLoading := d.awaitReady(ctx, ready, entry)
	d.readTargetInfo(ctx, entry)
	final, title := d.placeOf(entry)
	return browser.Session{
		ID:           id,
		Mode:         entry.mode,
		RequestedURL: entry.requested,
		FinalURL:     final,
		Title:        title,
		StillLoading: stillLoading,
		Status:       d.statusOf(entry),
	}, nil
}

// placeOf is where the page is and what it is called, under the lock: Snapshot and readTargetInfo
// write them from whichever goroutine is serving that call.
func (d *Driver) placeOf(entry *session) (final, title string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	return entry.final, entry.title
}

// notePlace records a non-empty url and title under the lock and returns what is now held.
func (d *Driver) notePlace(entry *session, final, title string) (string, string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	if final != "" {
		entry.final = final
	}
	if title != "" {
		entry.title = title
	}
	return entry.final, entry.title
}

// statusOf is the page's HTTP status under the lock, or 0 when nothing said it.
func (d *Driver) statusOf(entry *session) int {
	d.mu.Lock()
	defer d.mu.Unlock()
	return entry.status
}

// readTargetInfo asks the browser where the target actually ended up.
//
// Not cosmetic: spec §5.3 makes the trust decision a conjunction over the requested url AND the
// final one, and a redirect is the case that rule exists for. Reporting back the url we asked for
// would make that decision impossible to take, and would do it while looking correct.
func (d *Driver) readTargetInfo(ctx context.Context, entry *session) {
	result, err := d.conn.Call(ctx, cdp.BrowserSession, "Target.getTargetInfo", map[string]any{
		"targetId": entry.target,
	})
	if err != nil {
		return
	}
	var info struct {
		TargetInfo struct {
			URL   string `json:"url"`
			Title string `json:"title"`
		} `json:"targetInfo"`
	}
	if err := json.Unmarshal(result, &info); err != nil {
		return
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if info.TargetInfo.URL != "" {
		entry.final = info.TargetInfo.URL
	}
	entry.title = info.TargetInfo.Title
}

type attachment struct {
	session cdp.SessionID
	target  string
}

// createPage opens a blank target and returns it once it is attached.
//
// Blank, and navigated afterwards. Creating the target with the destination url would race the
// interception: the target would begin loading before there is a session to answer for it. Spec §5.4
// calls that window TOCTOU, and this is the shape of code that closes it.
func (d *Driver) createPage(ctx context.Context) (string, cdp.SessionID, error) {
	attached := make(chan attachment, 4)
	cancel := d.conn.OnEvent(func(event cdp.Event) {
		if event.Method != "Target.attachedToTarget" {
			return
		}
		var params struct {
			SessionID  cdp.SessionID `json:"sessionId"`
			TargetInfo struct {
				TargetID string `json:"targetId"`
				Type     string `json:"type"`
			} `json:"targetInfo"`
		}
		if err := json.Unmarshal(event.Params, &params); err != nil {
			return
		}
		if params.TargetInfo.Type != "page" {
			return
		}
		select {
		case attached <- attachment{session: params.SessionID, target: params.TargetInfo.TargetID}:
		default:
		}
	})
	defer cancel()

	created, err := d.conn.Call(ctx, cdp.BrowserSession, "Target.createTarget", map[string]any{
		"url": "about:blank",
	})
	if err != nil {
		return "", "", fmt.Errorf("creating target: %w", err)
	}
	var target struct {
		TargetID string `json:"targetId"`
	}
	if err := json.Unmarshal(created, &target); err != nil {
		return "", "", err
	}

	session, err := d.sessionFor(ctx, target.TargetID, attached)
	if err != nil {
		return "", "", err
	}
	return target.TargetID, session, nil
}

// sessionFor waits for the auto-attach of a target we just created. It does NOT call
// Target.attachToTarget: auto-attach is already on from Connect, and attaching a second time would
// produce a second session for the same target, which is how a fence ends up armed on one of them.
func (d *Driver) sessionFor(ctx context.Context, targetID string, attached <-chan attachment) (cdp.SessionID, error) {
	deadline := time.After(20 * time.Second)
	for {
		select {
		case got := <-attached:
			if got.target == targetID {
				return got.session, nil
			}
		case <-deadline:
			return "", errors.New("chrome: the target never attached")
		case <-ctx.Done():
			return "", ctx.Err()
		}
	}
}

func (d *Driver) lookup(id browser.SessionID) (*session, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	entry, ok := d.sessions[id]
	if !ok {
		return nil, browser.ErrNoSuchSession
	}
	return entry, nil
}

// Screenshot returns PNG bytes.
func (d *Driver) Screenshot(ctx context.Context, id browser.SessionID) ([]byte, error) {
	d.gate.RLock()
	defer d.gate.RUnlock()
	entry, err := d.lookup(id)
	if err != nil {
		return nil, err
	}
	// The page in front of a person is theirs, and for a login it is a password half-typed.
	if d.personHolds(id) {
		return nil, browser.ErrPersonIsDriving
	}
	result, err := d.conn.Call(ctx, entry.cdp, "Page.captureScreenshot", map[string]any{"format": "png"})
	if err != nil {
		return nil, err
	}
	var payload struct {
		Data string `json:"data"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return nil, err
	}
	return base64.StdEncoding.DecodeString(payload.Data)
}

// Handoff marks the session as the person's.
//
// The relaunch itself belongs to the núcleo, not here: spec §4.2 says the process is discarded and
// a headful one is started over the SAME profile, and this driver does not know where profiles live
// — deliberately, since that is the boundary the whole pillar holds. What this does is close the
// agent's session gracefully, because the spike measured that a hard kill loses the last writes to
// the profile, and the profile is the identity the person is about to use.
func (d *Driver) Handoff(ctx context.Context, id browser.SessionID, reason string) (browser.HandoffTicket, error) {
	entry, err := d.lookup(id)
	if err != nil {
		return browser.HandoffTicket{}, err
	}
	// Under the lock, because Act reads it. Spec §4.4 rule 1 makes this write the thing that stops
	// the agent, and a stop that races the act it is stopping is not one.
	d.mu.Lock()
	entry.mode = browser.ModeHuman
	final := entry.final
	d.mu.Unlock()
	return browser.HandoffTicket{
		SessionID: id,
		Mode:      browser.ModeHuman,
		URL:       final,
		Reason:    reason,
	}, nil
}

// Close ends one session's target.
func (d *Driver) Close(ctx context.Context, id browser.SessionID) error {
	entry, err := d.lookup(id)
	if err != nil {
		return err
	}
	_, callErr := d.conn.Call(ctx, cdp.BrowserSession, "Target.closeTarget", map[string]any{
		"targetId": entry.target,
	})
	// Before the session is forgotten, because forgetAttachments reads it. A session that closes
	// without this leaves the agent's own words on the disk of a machine it was never asked to
	// write to.
	d.forgetAttachments(entry)
	// A person whose session closes has nothing left to drive: the fence comes back with it.
	if state := d.person.Load(); state != nil && state.session == id {
		if d.person.CompareAndSwap(state, nil) && state.unsubscribe != nil {
			state.unsubscribe()
		}
	}
	d.mu.Lock()
	delete(d.sessions, id)
	// Viewers of this session's screencast return ErrNoSuchSession.
	if cast, ok := d.casts[entry.cdp]; ok {
		cast.end()
		delete(d.casts, entry.cdp)
	}
	// Everything that points at this session, not only the page's own entries: a frame Chromium ran
	// out of process and a popup traced to its opener each left one, and so did every execution
	// context the ferry placed. Left behind they are a map that only grows for the life of the
	// browser.
	for target, owner := range d.targets {
		if owner == id {
			delete(d.targets, target)
		}
	}
	owned := map[cdp.SessionID]bool{entry.cdp: true}
	for on, owner := range d.cdpToSession {
		if owner == id {
			owned[on] = true
			delete(d.cdpToSession, on)
		}
	}
	for on := range entry.frames {
		owned[on] = true
	}
	for key := range d.contexts {
		if owned[key.session] {
			delete(d.contexts, key)
		}
	}
	d.mu.Unlock()
	return callErr
}
