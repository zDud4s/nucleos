// §spec browser-ao-vivo

package pool

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
	"nucleosbrowser/profile"
)

// watchingInstance is a fakeInstance that can also be watched. Watch pushes whatever the test sends on
// frames to the sink until its context ends, which is all a viewer needs from a browser.
type watchingInstance struct {
	*fakeInstance
	frames  chan browser.Frame
	entered chan struct{}
	once    sync.Once
}

func (w *watchingInstance) Watch(ctx context.Context, _ browser.SessionID, sink func(browser.Frame)) error {
	w.once.Do(func() { close(w.entered) })
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case frame := <-w.frames:
			sink(frame)
			if err := ctx.Err(); err != nil {
				return err
			}
		}
	}
}

// watchLauncher launches headless browsers that can be watched. LaunchHuman is the embedded
// fakeLauncher's, so a person's browser is NOT a Watcher — which is what spec §3.1 says it must not be.
type watchLauncher struct {
	*fakeLauncher
	mu   sync.Mutex
	made []*watchingInstance
}

func (l *watchLauncher) Launch(ctx context.Context, dir string, policy fence.Policy) (Instance, error) {
	inner, err := l.fakeLauncher.Launch(ctx, dir, policy)
	if err != nil {
		return nil, err
	}
	instance := &watchingInstance{
		fakeInstance: inner.(*fakeInstance),
		frames:       make(chan browser.Frame, 8),
		entered:      make(chan struct{}),
	}
	l.mu.Lock()
	l.made = append(l.made, instance)
	l.mu.Unlock()
	return instance, nil
}

func (l *watchLauncher) first(t *testing.T) *watchingInstance {
	t.Helper()
	l.mu.Lock()
	defer l.mu.Unlock()
	if len(l.made) == 0 {
		t.Fatal("no watchable browser was launched")
	}
	return l.made[0]
}

func watchPool(t *testing.T) (*Pool, *watchLauncher) {
	t.Helper()
	launcher := &watchLauncher{fakeLauncher: &fakeLauncher{}}
	pool := New(launcher, profile.Store{Root: t.TempDir()}, profile.Limits{}, 4)
	t.Cleanup(func() { pool.Shutdown(context.Background()) })
	return pool, launcher
}

// startWatch begins a watch and waits until the browser is inside it, which, since the pool registers
// the viewer before it calls the browser, means the viewer is registered too.
func startWatch(t *testing.T, pool *Pool, instance *watchingInstance, id browser.SessionID, sink func(browser.Frame)) <-chan error {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- pool.Watch(context.Background(), id, sink) }()
	select {
	case <-instance.entered:
	case err := <-done:
		t.Fatalf("watch returned before it started: %v", err)
	case <-time.After(5 * time.Second):
		t.Fatal("the browser never began to watch")
	}
	return done
}

func discard(browser.Frame) {}

// endedWith reads the reason a watch ended with. It accepts the error as a value or as a pointer,
// because both satisfy `error` and the contract is the reason, not the spelling.
func endedWith(t *testing.T, err error) browser.EndReason {
	t.Helper()
	var value browser.WatchEnded
	if errors.As(err, &value) {
		return value.Reason
	}
	var pointer *browser.WatchEnded
	if errors.As(err, &pointer) {
		return pointer.Reason
	}
	t.Fatalf("the watch ended with %v, want a browser.WatchEnded", err)
	return ""
}

func waitEnd(t *testing.T, done <-chan error) error {
	t.Helper()
	select {
	case err := <-done:
		return err
	case <-time.After(2 * time.Second):
		t.Fatal("the viewer was not ended")
		return nil
	}
}

// Spec §3.1: handing a session to a person ends the viewers with "wheel" — the page they were looking
// at is about to be a person's.
func TestWatchHandoffEndsWithWheel(t *testing.T) {
	pool, launcher := watchPool(t)
	session := mustOpen(t, pool, ephemeral("r1"))
	done := startWatch(t, pool, launcher.first(t), session.ID, discard)

	if _, err := pool.Handoff(context.Background(), session.ID, "login"); err != nil {
		t.Fatalf("handoff: %v", err)
	}
	if got := endedWith(t, waitEnd(t, done)); got != browser.EndWheel {
		t.Fatalf("reason = %q, want wheel", got)
	}
}

func TestWatchTakeWheelEndsWithWheel(t *testing.T) {
	pool, launcher := watchPool(t)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)
	done := startWatch(t, pool, launcher.first(t), session.ID, discard)

	takeWheel(t, pool, session.ID, placement)
	if got := endedWith(t, waitEnd(t, done)); got != browser.EndWheel {
		t.Fatalf("reason = %q, want wheel", got)
	}
}

// A viewer on a session that was NOT the one handed over still loses its browser when a person takes
// the profile — beginHuman evicts every session on it.
func TestWatchDisplacedByAHumanEndsWithWheel(t *testing.T) {
	pool, launcher := watchPool(t)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)
	done := startWatch(t, pool, launcher.first(t), session.ID, discard)

	takeWheel(t, pool, "", placement)
	if got := endedWith(t, waitEnd(t, done)); got != browser.EndWheel {
		t.Fatalf("reason = %q, want wheel", got)
	}
}

// Closed is not gone: the agent finished with the page, which the viewer is told differently from a
// browser that vanished.
func TestWatchCloseEndsWithClosedNotGone(t *testing.T) {
	pool, launcher := watchPool(t)
	session := mustOpen(t, pool, ephemeral("r1"))
	done := startWatch(t, pool, launcher.first(t), session.ID, discard)

	if err := pool.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if got := endedWith(t, waitEnd(t, done)); got != browser.EndClosed {
		t.Fatalf("reason = %q, want closed", got)
	}
}

func TestWatchForgetEndsWithGone(t *testing.T) {
	pool, launcher := watchPool(t)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)
	done := startWatch(t, pool, launcher.first(t), session.ID, discard)

	if _, err := pool.Forget(context.Background(), placement.Profile); err != nil {
		t.Fatalf("forget: %v", err)
	}
	if got := endedWith(t, waitEnd(t, done)); got != browser.EndGone {
		t.Fatalf("reason = %q, want gone", got)
	}
}

func TestWatchShutdownEndsWithGone(t *testing.T) {
	pool, launcher := watchPool(t)
	session := mustOpen(t, pool, ephemeral("r1"))
	done := startWatch(t, pool, launcher.first(t), session.ID, discard)

	pool.Shutdown(context.Background())
	if got := endedWith(t, waitEnd(t, done)); got != browser.EndGone {
		t.Fatalf("reason = %q, want gone", got)
	}
}

// Spec §3.1: a person's browser is not watchable. The refusal is ErrPersonIsDriving (a 409 on the
// wire), not "no such session": the session exists, the pillar just will not show it.
func TestWatchOnAHumanBrowserIsRefused(t *testing.T) {
	pool, _ := watchPool(t)
	placement := project("acme", "https://jira.example.org")
	wheel := takeWheel(t, pool, "", placement)

	err := pool.Watch(context.Background(), wheel.Session, discard)
	if !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Fatalf("watch on a human browser: %v, want ErrPersonIsDriving", err)
	}
}

func TestWatchOnAnUnknownSessionIsNoSuchSession(t *testing.T) {
	pool, _ := watchPool(t)
	err := pool.Watch(context.Background(), "nope", discard)
	if !errors.Is(err, browser.ErrNoSuchSession) {
		t.Fatalf("watch on an unknown session: %v, want ErrNoSuchSession", err)
	}
}

// A viewer that never drains its sink must not hold the wheel hostage: ending watchers cancels them
// and returns, it never waits for them (spec §3.1).
func TestWatchTakeWheelDoesNotWaitForAStuckViewer(t *testing.T) {
	pool, launcher := watchPool(t)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)
	instance := launcher.first(t)

	inSink := make(chan struct{})
	release := make(chan struct{})
	var once sync.Once
	sink := func(browser.Frame) {
		once.Do(func() { close(inSink) })
		<-release
	}
	done := startWatch(t, pool, instance, session.ID, sink)
	t.Cleanup(func() {
		select {
		case <-release:
		default:
			close(release)
		}
	})
	instance.frames <- browser.Frame{JPEG: []byte("x")}
	select {
	case <-inSink:
	case <-time.After(5 * time.Second):
		t.Fatal("the viewer's sink was never reached")
	}

	took := make(chan struct{})
	go func() {
		_, _ = pool.TakeWheel(context.Background(), browser.WheelRequest{
			Session:   session.ID,
			URL:       "https://jira.example.org/login",
			Placement: placement,
		})
		close(took)
	}()
	select {
	case <-took:
	case <-time.After(2 * time.Second):
		t.Fatal("taking the wheel waited on a viewer whose sink was stuck")
	}

	close(release)
	if got := endedWith(t, waitEnd(t, done)); got != browser.EndWheel {
		t.Fatalf("reason = %q, want wheel", got)
	}
}
