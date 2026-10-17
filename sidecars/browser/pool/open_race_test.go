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
