// §spec browser-com-painel

package pool

import (
	"context"
	"encoding/json"
	"errors"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
	"nucleosbrowser/profile"
)

// panelingInstance is a fakeInstance that carries a panel: it records what is pushed and, in
// PanelEvents, hands the sink whatever the test sends on events until its context ends.
type panelingInstance struct {
	*fakeInstance
	events  chan json.RawMessage
	entered chan struct{}
	once    sync.Once
	pmu     sync.Mutex
	pushed  []json.RawMessage
}

func (p *panelingInstance) PanelPush(_ context.Context, _ browser.SessionID, msg json.RawMessage) error {
	p.pmu.Lock()
	defer p.pmu.Unlock()
	p.pushed = append(p.pushed, msg)
	return nil
}

func (p *panelingInstance) PanelEvents(ctx context.Context, _ browser.SessionID, sink func(json.RawMessage)) error {
	p.once.Do(func() { close(p.entered) })
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case event := <-p.events:
			sink(event)
		}
	}
}

func (p *panelingInstance) pushedMessages() []string {
	p.pmu.Lock()
	defer p.pmu.Unlock()
	out := make([]string, 0, len(p.pushed))
	for _, m := range p.pushed {
		out = append(out, string(m))
	}
	return out
}

// panelLauncher launches headless browsers that carry a panel.
type panelLauncher struct {
	*fakeLauncher
	mu   sync.Mutex
	made []*panelingInstance
}

func (l *panelLauncher) Launch(ctx context.Context, dir string, policy fence.Policy) (Instance, error) {
	inner, err := l.fakeLauncher.Launch(ctx, dir, policy)
	if err != nil {
		return nil, err
	}
	instance := &panelingInstance{
		fakeInstance: inner.(*fakeInstance),
		events:       make(chan json.RawMessage, 8),
		entered:      make(chan struct{}),
	}
	l.mu.Lock()
	l.made = append(l.made, instance)
	l.mu.Unlock()
	return instance, nil
}

func (l *panelLauncher) first(t *testing.T) *panelingInstance {
	t.Helper()
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.made) == 0 {
		t.Fatal("no panel-carrying browser was launched")
	}
	return l.made[0]
}

func panelPool(t *testing.T) (*Pool, *panelLauncher) {
	t.Helper()
	launcher := &panelLauncher{fakeLauncher: &fakeLauncher{}}
	pool := New(launcher, profile.Store{Root: t.TempDir()}, profile.Limits{}, 4)
	t.Cleanup(func() { pool.Shutdown(context.Background()) })
	return pool, launcher
}

func panelClosedReason(t *testing.T, err error) browser.PanelEnd {
	t.Helper()
	var value browser.PanelClosed
	if errors.As(err, &value) {
		return value.Reason
	}
	var pointer *browser.PanelClosed
	if errors.As(err, &pointer) {
		return pointer.Reason
	}
	t.Fatalf("the panel channel ended with %v, want a browser.PanelClosed", err)
	return ""
}

// TestPoolPanelEventsEndWhenTheSessionCloses. A listener that is attached sees the events the panel
// sends, and when the session is closed from this side its channel ends with PanelClosed{"closed"}.
func TestPoolPanelEventsEndWhenTheSessionCloses(t *testing.T) {
	pool, launcher := panelPool(t)
	session := mustOpen(t, pool, ephemeral("r1"))
	instance := launcher.first(t)

	got := make(chan string, 4)
	done := make(chan error, 1)
	go func() {
		done <- pool.PanelEvents(context.Background(), session.ID, func(m json.RawMessage) { got <- string(m) })
	}()
	select {
	case <-instance.entered:
	case err := <-done:
		t.Fatalf("PanelEvents returned before it started: %v", err)
	case <-time.After(5 * time.Second):
		t.Fatal("the browser never began to listen to the panel")
	}

	instance.events <- json.RawMessage(`{"say":"hi"}`)
	select {
	case m := <-got:
		if m != `{"say":"hi"}` {
			t.Fatalf("event = %q", m)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the panel's event never reached the sink")
	}

	if err := pool.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	select {
	case err := <-done:
		if reason := panelClosedReason(t, err); reason != browser.PanelSessionClosed {
			t.Fatalf("reason = %q, want closed", reason)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the panel listener was not ended by Close")
	}
}

// Shutdown ends the listeners too, with the same reason.
func TestPoolPanelEventsEndOnShutdown(t *testing.T) {
	pool, launcher := panelPool(t)
	session := mustOpen(t, pool, ephemeral("r1"))
	instance := launcher.first(t)

	done := make(chan error, 1)
	go func() {
		done <- pool.PanelEvents(context.Background(), session.ID, func(json.RawMessage) {})
	}()
	select {
	case <-instance.entered:
	case <-time.After(5 * time.Second):
		t.Fatal("the browser never began to listen to the panel")
	}

	pool.Shutdown(context.Background())
	select {
	case err := <-done:
		if reason := panelClosedReason(t, err); reason != browser.PanelSessionClosed {
			t.Fatalf("reason = %q, want closed", reason)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("the panel listener was not ended by Shutdown")
	}
}

// A push is routed to the session's own browser.
func TestPoolPanelPushReachesTheSessionsBrowser(t *testing.T) {
	pool, launcher := panelPool(t)
	session := mustOpen(t, pool, ephemeral("r1"))

	if err := pool.PanelPush(context.Background(), session.ID, json.RawMessage(`{"say":"hello"}`)); err != nil {
		t.Fatalf("push: %v", err)
	}
	if got := launcher.first(t).pushedMessages(); len(got) != 1 || got[0] != `{"say":"hello"}` {
		t.Fatalf("pushed = %v", got)
	}
}

// A browser with no panel says so: ErrUnsupported, and an unknown session is ErrNoSuchSession.
func TestPoolPanelOnAPlainDriverIsUnsupported(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)
	session := mustOpen(t, pool, ephemeral("r1"))

	if err := pool.PanelPush(context.Background(), session.ID, json.RawMessage(`{}`)); !errors.Is(err, browser.ErrUnsupported) {
		t.Fatalf("push on a plain driver: %v, want ErrUnsupported", err)
	}
	if err := pool.PanelEvents(context.Background(), session.ID, func(json.RawMessage) {}); !errors.Is(err, browser.ErrUnsupported) {
		t.Fatalf("events on a plain driver: %v, want ErrUnsupported", err)
	}
	if err := pool.PanelPush(context.Background(), "nope", json.RawMessage(`{}`)); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Fatalf("push on an unknown session: %v, want ErrNoSuchSession", err)
	}
}
