// §spec pilar-de-browser

package chrome

import (
	"context"
	"encoding/json"
	"sync"
	"time"

	"nucleosbrowser/cdp"
)

// openLoadWait is how long a freshly created tab has to report anything before it is judged lost, and
// openAttempts how many tabs will be made. Variables so tests can shorten them.
var (
	openLoadWait = 5 * time.Second
	openAttempts = 3
)

// startupWindowWait bounds how long a first tab waits for the browser's own startup window. A
// variable so a test can shorten it; a browser that never shows one is opened into anyway after this
// long.
var startupWindowWait = 15 * time.Second

// tabWatch is what both the person's window (Human) and the agent's pages (Driver) need to make a tab
// that actually loads.
//
// # The race
//
// Over the DevTools pipe the browser answers its first command long before it has built its startup
// window (a debugging port used to make Start wait for that, by accident). A tab created in the gap
// shows up as a target and never loads. And even after the window exists, a tab created in the first
// moments of its life can be lost without any error. So a tab is created only once the browser's own
// first page has reported its state, and a tab that then reports nothing is closed and made again.
type tabWatch struct {
	// up closes when the browser's own startup page first reports its state (targetInfoChanged for a
	// page). Merely being created is not enough: measured, a tab made right after that still loses the
	// race.
	up   chan struct{}
	once sync.Once

	mu sync.Mutex
	// reported holds the page targets that have reported their state at least once.
	reported map[string]bool
}

func newTabWatch() *tabWatch {
	return &tabWatch{up: make(chan struct{}), reported: map[string]bool{}}
}

// observe is an event handler: it notes every page target that reports its state.
func (w *tabWatch) observe(event cdp.Event) {
	if event.Method != "Target.targetInfoChanged" {
		return
	}
	var params struct {
		TargetInfo struct {
			TargetID string `json:"targetId"`
			Type     string `json:"type"`
		} `json:"targetInfo"`
	}
	if err := json.Unmarshal(event.Params, &params); err != nil || params.TargetInfo.Type != "page" {
		return
	}
	w.once.Do(func() { close(w.up) })
	w.mu.Lock()
	w.reported[params.TargetInfo.TargetID] = true
	w.mu.Unlock()
}

// awaitWindow waits for the browser's own startup window, up to startupWindowWait.
func (w *tabWatch) awaitWindow(ctx context.Context) error {
	select {
	case <-w.up:
	case <-time.After(startupWindowWait):
	case <-ctx.Done():
		return ctx.Err()
	}
	return nil
}

// waitReported waits for a target to report its state, up to openLoadWait.
func (w *tabWatch) waitReported(ctx context.Context, targetID string) bool {
	deadline := time.Now().Add(openLoadWait)
	for {
		w.mu.Lock()
		ok := w.reported[targetID]
		w.mu.Unlock()
		if ok {
			return true
		}
		if time.Now().After(deadline) || ctx.Err() != nil {
			return false
		}
		time.Sleep(20 * time.Millisecond)
	}
}

// makeLive waits for the browser's window, then creates a tab and makes sure it is alive.
//
// create makes one tab and returns its target id. A live tab reports its state at once, so one that
// says nothing within openLoadWait is closed (through closeTab) and made again, up to openAttempts
// times; the last one is kept whatever it does.
func (w *tabWatch) makeLive(ctx context.Context, create func(context.Context) (string, error), closeTab func(context.Context, string)) (string, error) {
	if err := w.awaitWindow(ctx); err != nil {
		return "", err
	}
	for attempt := 1; ; attempt++ {
		id, err := create(ctx)
		if err != nil {
			return id, err
		}
		if attempt >= openAttempts || w.waitReported(ctx, id) {
			return id, ctx.Err()
		}
		closeTab(ctx, id)
	}
}
