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

// awaitReady waits for the page to finish, and reports whether it gave up waiting.
//
// It must only be called when something IS loading — after a navigate, or after an act that moved
// the page. Called on a page that settled long ago it would hear nothing, wait out the deadline, and
// then report a finished page as unfinished.
func (d *Driver) awaitReady(ctx context.Context, w *watcher) (stillLoading bool) {
	overall := time.NewTimer(d.readyWithin)
	defer overall.Stop()

	var grace *time.Timer
	var graceC <-chan time.Time
	defer func() {
		if grace != nil {
			grace.Stop()
		}
	}()

	for {
		w.mu.Lock()
		loaded, idle := w.loaded, w.idle
		w.mu.Unlock()

		if idle {
			return false
		}
		if loaded && grace == nil {
			grace = time.NewTimer(d.idleGrace)
			graceC = grace.C
		}

		select {
		case <-w.wake:
		case <-graceC:
			// Loaded, and still talking to the network. Not "still loading": the document is there
			// and the agent can read it. A page that never goes quiet is ordinary.
			return false
		case <-overall.C:
			return true
		case <-ctx.Done():
			return true
		}
	}
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
}
