// §spec pilar-de-browser

package pool

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
	"nucleosbrowser/profile"
)

// gatedInstance lets the first Open through and holds every later one until released, which is the
// window a slow navigation gives another session to close on the same browser.
type gatedInstance struct {
	*fakeInstance
	mu      sync.Mutex
	calls   int
	entered chan struct{}
	proceed chan struct{}
}

func (g *gatedInstance) Open(ctx context.Context, req browser.OpenRequest) (browser.Session, error) {
	g.mu.Lock()
	g.calls++
	hold := g.calls > 1
	g.mu.Unlock()
	if hold {
		close(g.entered)
		<-g.proceed
	}
	return g.fakeInstance.Open(ctx, req)
}

type gatedLauncher struct {
	*fakeLauncher
	instance *gatedInstance
}

func (l *gatedLauncher) Launch(ctx context.Context, dir string, policy fence.Policy) (Instance, error) {
	inner, err := l.fakeLauncher.Launch(ctx, dir, policy)
	if err != nil {
		return nil, err
	}
	l.instance = &gatedInstance{
		fakeInstance: inner.(*fakeInstance),
		entered:      make(chan struct{}),
		proceed:      make(chan struct{}),
	}
	return l.instance, nil
}

// TestAdmitSweepSparesTheProfileOfARunningBrowser. A second profile over the disk budget sweeps
// ephemeral directories; the one the first browser is running in is not a leftover.
func TestAdmitSweepSparesTheProfileOfARunningBrowser(t *testing.T) {
	launcher := &fakeLauncher{}
	store := profile.Store{Root: t.TempDir()}
	pool := New(launcher, store, profile.Limits{DiskBudget: 1000}, 4)
	t.Cleanup(func() { pool.Shutdown(context.Background()) })

	mustOpen(t, pool, ephemeral("r1"))
	dir, err := store.Dir(profile.Ref{Kind: profile.Ephemeral, ID: "r1"})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "Cache"), make([]byte, 5000), 0o600); err != nil {
		t.Fatal(err)
	}

	_, err = pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: ephemeral("r2"),
	})
	if !errors.Is(err, profile.ErrDiskBudget) {
		t.Fatalf("open over budget = %v, want ErrDiskBudget", err)
	}
	if _, statErr := os.Stat(dir); statErr != nil {
		t.Fatalf("the sweep deleted the profile of a running browser: %v", statErr)
	}
}

// TestReleaseDoesNotRetireABrowserAnotherOpenIsUsing. The browser a second Open acquired has no
// session registered until its navigation finishes, so the last other session closing saw an idle
// browser and shut it down under the navigation.
func TestReleaseDoesNotRetireABrowserAnotherOpenIsUsing(t *testing.T) {
	launcher := &gatedLauncher{fakeLauncher: &fakeLauncher{}}
	pool, _ := testPool(t, launcher.fakeLauncher, 4)
	pool.launcher = launcher

	first := mustOpen(t, pool, ephemeral("r1"))
	gated := launcher.instance

	opened := make(chan error, 1)
	go func() {
		_, err := pool.Open(context.Background(), browser.OpenRequest{
			URL:       "https://example.org/",
			Placement: ephemeral("r1"),
		})
		opened <- err
	}()
	select {
	case <-gated.entered:
	case <-time.After(5 * time.Second):
		t.Fatal("the second open never reached the driver")
	}

	if err := pool.Close(context.Background(), first.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if got := gated.stopped(); got != 0 {
		t.Fatalf("browser shut down %d time(s) while a second open was mid-navigation", got)
	}

	close(gated.proceed)
	if err := <-opened; err != nil {
		t.Fatalf("second open: %v", err)
	}
	if got := gated.stopped(); got != 0 {
		t.Fatalf("browser shut down %d time(s) although the second session is open", got)
	}
}

// blockedInstance holds every Open until released and counts the sessions it was told to close.
type blockedInstance struct {
	*fakeInstance
	mu      sync.Mutex
	closes  int
	entered chan struct{}
	proceed chan struct{}
}

func (b *blockedInstance) Open(ctx context.Context, req browser.OpenRequest) (browser.Session, error) {
	close(b.entered)
	<-b.proceed
	return b.fakeInstance.Open(ctx, req)
}

func (b *blockedInstance) Close(ctx context.Context, id browser.SessionID) error {
	b.mu.Lock()
	b.closes++
	b.mu.Unlock()
	return b.fakeInstance.Close(ctx, id)
}

type blockedLauncher struct {
	*fakeLauncher
	instance *blockedInstance
}

func (l *blockedLauncher) Launch(ctx context.Context, dir string, policy fence.Policy) (Instance, error) {
	inner, err := l.fakeLauncher.Launch(ctx, dir, policy)
	if err != nil {
		return nil, err
	}
	l.instance = &blockedInstance{
		fakeInstance: inner.(*fakeInstance),
		entered:      make(chan struct{}),
		proceed:      make(chan struct{}),
	}
	return l.instance, nil
}

// TestForgetWhileOpeningWins. A Forget that lands while an Open on the profile is still navigating
// must win: the open, once released, closes what it started, reports ErrForgotten and registers
// nothing.
func TestForgetWhileOpeningWins(t *testing.T) {
	launcher := &blockedLauncher{fakeLauncher: &fakeLauncher{}}
	pool, _ := testPool(t, launcher.fakeLauncher, 4)
	pool.launcher = launcher

	placement := project("acme", "https://jira.example.org")
	opened := make(chan error, 1)
	go func() {
		_, err := pool.Open(context.Background(), browser.OpenRequest{
			URL:       "https://jira.example.org/",
			Placement: placement,
		})
		opened <- err
	}()
	blocked := func() *blockedInstance {
		deadline := time.After(5 * time.Second)
		for {
			pool.mu.Lock()
			entry := pool.running[placement.Profile]
			pool.mu.Unlock()
			if entry != nil {
				<-entry.ready
				if inst, ok := entry.driver.(*blockedInstance); ok {
					return inst
				}
			}
			select {
			case <-deadline:
				t.Fatal("the open never produced a browser")
			case <-time.After(5 * time.Millisecond):
			}
		}
	}()
	select {
	case <-blocked.entered:
	case <-time.After(5 * time.Second):
		t.Fatal("the open never reached the driver")
	}

	if _, err := pool.Forget(context.Background(), placement.Profile); err != nil {
		t.Fatalf("forget: %v", err)
	}
	close(blocked.proceed)

	if err := <-opened; !errors.Is(err, ErrForgotten) {
		t.Fatalf("open after a forget = %v, want ErrForgotten", err)
	}
	blocked.mu.Lock()
	closes := blocked.closes
	blocked.mu.Unlock()
	if closes != 1 {
		t.Fatalf("the forgotten open closed %d session(s), want 1", closes)
	}
	pool.mu.Lock()
	defer pool.mu.Unlock()
	if len(pool.running) != 0 || len(pool.sessions) != 0 || pool.pending != 0 {
		t.Fatalf("state left behind: running=%d sessions=%d pending=%d", len(pool.running), len(pool.sessions), pool.pending)
	}
}
