package chrome

import (
	"context"
	"encoding/json"
	"sync"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/cdp"
)

// This file is about the race that had no name.
//
// `Page.navigate` returns when a navigation is COMMITTED, not when the page is there. Open used to
// return at that moment and Act waited only for the fence's refusal window, which is a window about
// the fence and not about the page. So the agent's first snapshot raced the load: on anything
// rendered by script it read an empty document, concluded the page had nothing on it, and acted on
// that conclusion. Nothing reported an error, because an empty reading of a half-loaded page is a
// correct reading of that instant — the same shape as refs minted by position, where both snapshots
// were right and only the inference between them was wrong.
//
// The answer is not a sleep. It is to watch the page say it is done, with a bound on how long it is
// given, and to SAY SO when the bound is reached rather than let silence pass for readiness.

// readyDeadline caps the whole wait. A page that has not loaded in this long is reported as still
// loading rather than waited on further: the agent can act on "not finished" and cannot act on a
// call that never returns.
const readyDeadline = 15 * time.Second

// idleGrace is how much longer than its load event a page gets to go quiet.
//
// Waiting for network silence alone would hang on anything holding a connection open — a live feed,
// a long poll — which is common enough that it would be the normal case on the sites this pillar
// exists to reach. Waiting for `load` alone returns before a script-rendered page has any content in
// it. So: load, and then a short while for whatever the scripts started to finish.
const idleGrace = 2 * time.Second

// watcher observes one page target for the two things an act needs to know: whether the document
// changed under it, and whether what replaced it has finished arriving.
type watcher struct {
	mu        sync.Mutex
	navigated bool
	loaded    bool
	idle      bool
	wake      chan struct{}
	cancel    func()
}

// watchPage subscribes before the thing being watched happens.
//
// Before, and not after: a fast page can fire `load` between a navigate call returning and a
// subscription being made, and a waiter that missed it would wait out the whole deadline and then
// report a loaded page as still loading.
func (d *Driver) watchPage(on cdp.SessionID, mainFrame string) *watcher {
	w := &watcher{wake: make(chan struct{}, 1)}
	w.cancel = d.conn.OnEvent(func(event cdp.Event) {
		if event.Session != on {
			return
		}
		switch event.Method {
		case "Page.frameNavigated":
			var params struct {
				Frame struct {
					ID       string `json:"id"`
					ParentID string `json:"parentId"`
				} `json:"frame"`
			}
			if err := json.Unmarshal(event.Params, &params); err != nil {
				return
			}
			// The main frame only. A page that swaps an ad in a subframe has not moved, and an act
			// reported as a navigation would tell the agent to throw away refs that are still good.
			if params.Frame.ParentID != "" || (mainFrame != "" && params.Frame.ID != mainFrame) {
				return
			}
			w.mark(func() {
				w.navigated = true
				w.loaded = false
				w.idle = false
			})
		case "Page.loadEventFired":
			w.mark(func() { w.loaded = true })
		case "Page.lifecycleEvent":
			var params struct {
				FrameID string `json:"frameId"`
				Name    string `json:"name"`
			}
			if err := json.Unmarshal(event.Params, &params); err != nil {
				return
			}
			if mainFrame != "" && params.FrameID != mainFrame {
				return
			}
			switch params.Name {
			case "load":
				w.mark(func() { w.loaded = true })
			case "networkAlmostIdle":
				w.mark(func() { w.idle = true })
			}
		}
	})
	return w
}

func (w *watcher) mark(change func()) {
	w.mu.Lock()
	change()
	w.mu.Unlock()
	select {
	case w.wake <- struct{}{}:
	default:
	}
}

func (w *watcher) stop() {
	if w.cancel != nil {
		w.cancel()
	}
}

// sawNavigation reports whether the document was replaced while this watcher was listening.
func (w *watcher) sawNavigation() bool {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.navigated
}

// renderGrace is how long a page gets to put a ferried answer on the screen.
//
// Short, because a resolved promise and a DOM write are microtasks. Not zero, because returning
// inside that window hands back the shell the ferry exists to prevent — the same race this file was
// written to close, reopened one layer up.
const renderGrace = 250 * time.Millisecond

// awaitReady waits for the page to finish, and reports whether it gave up waiting.
//
// It must only be called when something IS loading — after a navigate, or after an act that moved
// the page. Called on a page that settled long ago it would hear nothing, wait out the deadline, and
// then report a finished page as unfinished.
//
// "Finished" includes what the ferry is carrying. A ferried request does not go through the
// browser's network stack, so `networkAlmostIdle` fires while one is still on its way — and a page
// that asked us for its content would come back empty with the browser insisting it was done.
func (d *Driver) awaitReady(ctx context.Context, w *watcher, entry *session) (stillLoading bool) {
	overall := time.NewTimer(d.readyWithin)
	defer overall.Stop()
	// Polled as well as woken, because the ferry finishing is not one of the events the watcher
	// hears; it happens on this side.
	tick := time.NewTicker(25 * time.Millisecond)
	defer tick.Stop()

	var grace *time.Timer
	var graceC <-chan time.Time
	stopGrace := func() {
		if grace != nil {
			grace.Stop()
			grace, graceC = nil, nil
		}
	}
	defer stopGrace()

	for {
		w.mu.Lock()
		loaded, idle := w.loaded, w.idle
		w.mu.Unlock()
		asked, carrying := d.ferryState(entry)

		switch {
		case carrying > 0 || !(loaded || idle):
			// Something is still on its way. Any grace already running was started on a page that
			// has since asked for more, and letting it fire would answer about the earlier state.
			stopGrace()
		case idle && asked == 0:
			// The ordinary page: quiet, and it never asked us for anything. Nothing to wait for.
			return false
		case grace == nil:
			// Either it went quiet after a ferried answer — a moment to render it — or it loaded and
			// is still talking to the network, which gets the longer window it always had.
			within := d.idleGrace
			if idle {
				within = renderGrace
			}
			grace = time.NewTimer(within)
			graceC = grace.C
		}

		select {
		case <-w.wake:
		case <-tick.C:
		case <-graceC:
			// Loaded, nothing of ours in flight. Not "still loading": the document is there and the
			// agent can read it. A page that never goes quiet is ordinary.
			return false
		case <-overall.C:
			return true
		case <-ctx.Done():
			return true
		}
	}
}

// ferryState is how much this session has asked the ferry for, and how much is still on its way.
func (d *Driver) ferryState(entry *session) (asked, carrying int) {
	if entry == nil {
		return 0, 0
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	return entry.ferried, entry.carrying
}

// settleGrace is how long an act that ran the page's own code is given to start something.
//
// It is a REACTION window and not a wait: it is spent only when the act set nothing in motion, and
// the moment the page asks the ferry for anything, or redraws itself, the wait switches to that
// instead. So a click on a link that does nothing costs this once, and a click that fetches costs
// what the fetch costs.
const settleGrace = 300 * time.Millisecond

// changeDebounce is how long the page's own observer coalesces before it tells us anything.
//
// A MutationObserver on a document fires per node, and this report crosses a socket: undebounced, a
// page rendering a list would send thousands of messages to say one thing.
//
// It is SPENT OUT OF settleGrace, which is what keeps it short. The window is a bound on how late a
// redraw still counts as caused by the act, and every millisecond the observer sits on the news is a
// millisecond of that bound gone before we hear it. At 100ms a page that drew at 250 was reported at
// 350 and missed; the debounce is the part of that we control.
const changeDebounce = 40 * time.Millisecond

// The reaction window is a bound on how late a redraw still counts as CAUSED by the act, and it is a
// judgement rather than a measurement: an interface that has not begun to respond within it is not
// one a person would call responsive either. A page that renders half a second after a click is read
// as it stood before — which is why the next reading says `still_loading` when it is, and why taking
// another one is cheap.

// quietAfterChange is how long a page that redrew has to redraw again before it counts as finished.
//
// Separate from renderGrace, which is shorter and answers a different question. There we KNOW the
// request completed and are only allowing for a promise and a DOM write; here we know nothing —
// the second half of a skeleton-then-data render arrives from a timer we cannot see, and the only
// evidence available is silence.
//
// So this is a stated bound and not a measurement: a page whose two halves are further apart than
// this is read as it stood after the first. Five hundred milliseconds covers the ordinary
// placeholder-then-content gap; a second longer would cover more and would be paid by every act
// that redraws once and stops.
const quietAfterChange = 500 * time.Millisecond

// movingBound caps how long a page may hold an act by changing.
//
// A page that never stops changing is ORDINARY — a clock, a carousel, a spinner — and waiting it out
// would make every act on it cost the full deadline. Reaching this bound is not "still loading"
// either: the document is there and the agent can read it. What it costs is that an act on a page
// with something ticking on it pays this much, which is the price of not returning before a click
// has drawn anything.
const movingBound = 1500 * time.Millisecond

// awaitSettled waits for what an act set in motion WITHOUT replacing the document.
//
// This is the load race again, one layer in, and it was reopened by the thing that closed it. The
// ferry exists for pages that render themselves from an API; on such a page the ordinary
// interaction — a click — does not navigate, so [Driver.afterAct] returned immediately, and the
// agent's next snapshot read the page before the answer it had just asked for arrived. Open was
// covered, goto and back were covered, and the single most common case on the single class of page
// the ferry was built for was not.
//
// A page that goes on asking is not waited on forever: the overall bound is the same one Open uses,
// and reaching it is reported as still loading rather than passed off as finished.
func (d *Driver) awaitSettled(ctx context.Context, entry *session, since time.Time) (stillLoading bool) {
	overall := time.NewTimer(d.readyWithin)
	defer overall.Stop()
	moving := time.NewTimer(d.movingWithin)
	defer moving.Stop()
	// Polled, because neither signal is one the page watcher hears: a ferried request starts and
	// finishes on this side of the connection, and a redraw arrives as a binding call.
	tick := time.NewTicker(10 * time.Millisecond)
	defer tick.Stop()

	reactionEnds := time.Now().Add(d.settleWithin)
	started := false

	for {
		_, carrying := d.ferryState(entry)
		changed := d.changedAt(entry)
		// A redraw from before this act is somebody else's news. Only what happened after it began
		// is evidence that it began anything.
		busy := carrying > 0
		drawing := !changed.Before(since) && time.Since(changed) < quietAfterChange
		if busy || drawing {
			started = true
		}

		switch {
		case busy || drawing:
			// Still going. Nothing to decide yet.
		case started:
			// It started something and that something has gone quiet.
			return false
		case time.Now().After(reactionEnds):
			// The act touched the page and the page did nothing. There is nothing to wait for, and
			// waiting anyway is how a cheap verb stops being cheap.
			return false
		}

		// While a request of ours is in flight the moving bound must not fire: it would answer
		// "done" about a page that is mid-request, which is the one thing this wait exists to stop.
		movingC := moving.C
		if busy {
			movingC = nil
		}

		select {
		case <-tick.C:
		case <-movingC:
			// A page that never settles is ordinary, and it is not unfinished — see movingBound.
			return false
		case <-overall.C:
			return true
		case <-ctx.Done():
			return true
		}
	}
}

// noteChange records that a page said it redrew itself.
//
// A timestamp and not a count: what the wait needs to know is when the changes STOPPED, and a
// counter would answer a different question at the same cost.
func (d *Driver) noteChange(on cdp.SessionID) {
	d.mu.Lock()
	defer d.mu.Unlock()
	owner, known := d.cdpToSession[on]
	if !known {
		return
	}
	if entry, live := d.sessions[owner]; live {
		entry.changedAt = time.Now()
	}
}

// changedAt is when this session's page last said it redrew.
func (d *Driver) changedAt(entry *session) time.Time {
	if entry == nil {
		return time.Time{}
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	return entry.changedAt
}

// mainFrameOf asks which frame is the top one, so the watcher can ignore everything else.
//
// Best effort. An empty answer means the watcher accepts any frame's events, which is the behaviour
// this had before there was a filter at all — noisier, never wrong in the direction that matters.
func (d *Driver) mainFrameOf(ctx context.Context, on cdp.SessionID) string {
	result, err := d.conn.Call(ctx, on, "Page.getFrameTree", nil)
	if err != nil {
		return ""
	}
	var payload struct {
		FrameTree struct {
			Frame struct {
				ID string `json:"id"`
			} `json:"frame"`
		} `json:"frameTree"`
	}
	if err := json.Unmarshal(result, &payload); err != nil {
		return ""
	}
	return payload.FrameTree.Frame.ID
}

// forgetRefs drops every handle a session was holding, because the document they named is gone.
//
// Without this an act after a navigation resolves a ref against a document that no longer exists.
// The best case is a CDP error the agent reads as a broken browser; the worse case is a renderer
// that reused the number for something else, which is the ref-by-position bug wearing a different
// hat. Clearing turns both into the one answer the agent knows what to do with: "that is not in the
// current snapshot; take a new one."
//
// lastReported is deliberately KEPT. It is what lets the next changes-only read say that everything
// on the old page is gone, which is true and is exactly what the agent needs to hear.
func (d *Driver) forgetRefs(entry *session) {
	d.mu.Lock()
	defer d.mu.Unlock()
	entry.refs = map[string]nodeKey{}
	entry.refByNode = map[nodeKey]string{}
	entry.frames = map[cdp.SessionID]frameRef{}
	// What the CSP stopped belonged to the document that is gone. Carrying the count forward would
	// answer a question about this page with evidence from the last one.
	entry.blocked = 0
	entry.blockedLast = browser.Refusal{}
	entry.ferried = 0
	entry.carrying = 0
	// A question the document that is gone asked is not one this one is asking.
	entry.dialogs = nil
	// A redraw of the document that is gone is not news about the one that replaced it.
	entry.changedAt = time.Time{}
}
