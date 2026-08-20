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

// fakeInstance is a browser.Fake that can also be shut down, which is the whole of what an Instance
// is over a Driver.
type fakeInstance struct {
	*browser.Fake
	mu        sync.Mutex
	shutdowns int
	dir       string
	policy    fence.Policy
	// headful records that this instance was built by LaunchHuman, which is the only observable
	// difference between the two launches once the process is gone.
	headful bool
	chain   []string
}

// Chain makes a headful fakeInstance the kind of driver chainOf can read, mirroring chrome.Human.
func (f *fakeInstance) Chain() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.chain...)
}

func (f *fakeInstance) Shutdown(context.Context) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.shutdowns++
}

func (f *fakeInstance) stopped() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.shutdowns
}

type fakeLauncher struct {
	mu        sync.Mutex
	instances []*fakeInstance
	err       error
	// humanErr, if set, is what LaunchHuman fails with — spec §4.4a's "the headful does not start".
	humanErr error
	// chain is what a headful instance reports as the navigation the person made.
	chain []string
	// delay makes a launch slow enough for a second caller to arrive during it, which is the case
	// the per-profile serialisation exists for.
	delay time.Duration
}

func (l *fakeLauncher) Name() string { return "fake" }

func (l *fakeLauncher) Launch(_ context.Context, dir string, policy fence.Policy) (Instance, error) {
	if l.delay > 0 {
		time.Sleep(l.delay)
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.err != nil {
		return nil, l.err
	}
	instance := &fakeInstance{
		Fake:   &browser.Fake{FenceAttached: true},
		dir:    dir,
		policy: policy,
	}
	l.instances = append(l.instances, instance)
	return instance, nil
}

func (l *fakeLauncher) LaunchHuman(_ context.Context, dir string) (Instance, error) {
	if l.delay > 0 {
		time.Sleep(l.delay)
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.humanErr != nil {
		return nil, l.humanErr
	}
	instance := &fakeInstance{
		Fake:    &browser.Fake{FenceAttached: true},
		dir:     dir,
		headful: true,
		chain:   l.chain,
	}
	l.instances = append(l.instances, instance)
	return instance, nil
}

func (l *fakeLauncher) launched() []*fakeInstance {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]*fakeInstance(nil), l.instances...)
}

func testPool(t *testing.T, launcher *fakeLauncher, maxSessions int) (*Pool, profile.Store) {
	t.Helper()
	store := profile.Store{Root: t.TempDir()}
	pool := New(launcher, store, profile.Limits{}, maxSessions)
	t.Cleanup(func() { pool.Shutdown(context.Background()) })
	return pool, store
}

func ephemeral(id string) browser.Placement {
	return browser.Placement{Profile: profile.Ref{Kind: profile.Ephemeral, ID: id}}
}

func project(id string, origins ...string) browser.Placement {
	return browser.Placement{
		Profile: profile.Ref{Kind: profile.Project, ID: id},
		Origins: origins,
	}
}

func mustOpen(t *testing.T, pool *Pool, placement browser.Placement) browser.Session {
	t.Helper()
	session, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: placement,
	})
	if err != nil {
		t.Fatalf("open %s: %v", placement.Profile, err)
	}
	return session
}

// TestOneBrowserPerProfile. Two sessions on one profile share a browser; two profiles do not. The
// second half is the one that costs money if it is wrong — a shared browser means a shared
// --user-data-dir, which means a stranger's page in the profile that holds the logins.
func TestOneBrowserPerProfile(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)

	mustOpen(t, pool, ephemeral("r1"))
	mustOpen(t, pool, ephemeral("r1"))
	if got := len(launcher.launched()); got != 1 {
		t.Fatalf("launched %d browsers for one profile, want 1", got)
	}

	mustOpen(t, pool, project("acme", "https://example.org"))
	if got := len(launcher.launched()); got != 2 {
		t.Fatalf("launched %d browsers for two profiles, want 2", got)
	}
	instances := launcher.launched()
	if instances[0].dir == instances[1].dir {
		t.Fatalf("two profiles landed in one directory: %s", instances[0].dir)
	}
}

// TestConcurrentOpensOnOneProfileLaunchOnce. Starting a second Chromium over a directory the first
// one holds does not fail — it hands off to the survivor and exits, leaving the caller driving a
// browser it never configured (launch.ErrInheritedInstance). So the race must not be possible at all.
func TestConcurrentOpensOnOneProfileLaunchOnce(t *testing.T) {
	launcher := &fakeLauncher{delay: 30 * time.Millisecond}
	pool, _ := testPool(t, launcher, 8)

	var wait sync.WaitGroup
	for range 6 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			_, _ = pool.Open(context.Background(), browser.OpenRequest{
				URL:       "https://example.org/",
				Placement: ephemeral("r1"),
			})
		}()
	}
	wait.Wait()

	if got := len(launcher.launched()); got != 1 {
		t.Fatalf("launched %d browsers over one profile directory, want 1", got)
	}
}

// TestSessionsFromDifferentBrowsersDoNotCollide. Every driver numbers its own sessions from one, so
// the first session of every browser answers to the same name. Without an id minted by the pool, the
// second profile's session overwrites the first one's entry and acts land in the wrong browser —
// which, for a profile that holds logins, is the whole boundary crossed by a map key.
func TestSessionsFromDifferentBrowsersDoNotCollide(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)

	first := mustOpen(t, pool, ephemeral("r1"))
	second := mustOpen(t, pool, project("acme", "https://example.org"))
	if first.ID == second.ID {
		t.Fatalf("two sessions share the id %q", first.ID)
	}

	if _, err := pool.Act(context.Background(), first.ID, browser.Action{Kind: browser.ActionClick, Ref: "e1"}); err != nil {
		t.Fatalf("act on the first session: %v", err)
	}
	instances := launcher.launched()
	if len(instances[0].Actions) != 1 {
		t.Errorf("the first browser saw %d actions, want 1", len(instances[0].Actions))
	}
	if len(instances[1].Actions) != 0 {
		t.Errorf("the action landed in the wrong browser: %+v", instances[1].Actions)
	}

	// And the id the caller was given is the id it keeps hearing, in both directions.
	snapshot, err := pool.Snapshot(context.Background(), second.ID, browser.SnapshotRequest{})
	if err != nil {
		t.Fatalf("snapshot: %v", err)
	}
	if snapshot.SessionID != second.ID {
		t.Errorf("snapshot came back as %q, want %q", snapshot.SessionID, second.ID)
	}
}

// TestTheLastCloseStopsTheBrowserAndTakesTheEphemeralProfile — spec §5.1's promise under normal
// operation, and §4.2's graceful close that flushes the profile rather than losing its last write.
func TestTheLastCloseStopsTheBrowserAndTakesTheEphemeralProfile(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, store := testPool(t, launcher, 4)

	first := mustOpen(t, pool, ephemeral("r1"))
	second := mustOpen(t, pool, ephemeral("r1"))
	dir, err := store.Dir(profile.Ref{Kind: profile.Ephemeral, ID: "r1"})
	if err != nil {
		t.Fatalf("dir: %v", err)
	}
	if _, err := os.Stat(dir); err != nil {
		t.Fatalf("the profile was never created: %v", err)
	}

	if err := pool.Close(context.Background(), first.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	instance := launcher.launched()[0]
	if instance.stopped() != 0 {
		t.Fatal("the browser was stopped while a session was still open on it")
	}
	if _, err := os.Stat(dir); err != nil {
		t.Fatalf("the profile was deleted while a session was still open: %v", err)
	}

	if err := pool.Close(context.Background(), second.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if instance.stopped() != 1 {
		t.Errorf("the browser was stopped %d times after the last close, want 1", instance.stopped())
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Errorf("the ephemeral profile survived its last session: %v", err)
	}
}

// TestTheProjectProfileSurvivesItsLastSession is the control for the test above. A pool that deleted
// on close regardless of kind would pass it and take every login with it.
func TestTheProjectProfileSurvivesItsLastSession(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, store := testPool(t, launcher, 4)

	session := mustOpen(t, pool, project("acme", "https://example.org"))
	dir, err := store.Dir(profile.Ref{Kind: profile.Project, ID: "acme"})
	if err != nil {
		t.Fatalf("dir: %v", err)
	}
	if err := pool.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if launcher.launched()[0].stopped() != 1 {
		t.Error("the browser was left running after its last session")
	}
	if _, err := os.Stat(dir); err != nil {
		t.Fatalf("the project profile was deleted: %v", err)
	}
}

// TestTheSessionCeilingRefuses — spec §9.6. A browser is hundreds of megabytes competing with the
// local model for the same card, so this refuses rather than degrades.
func TestTheSessionCeilingRefuses(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 1)

	session := mustOpen(t, pool, ephemeral("r1"))
	_, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: ephemeral("r2"),
	})
	if !errors.Is(err, ErrTooManySessions) {
		t.Fatalf("second open = %v, want ErrTooManySessions", err)
	}

	// And the slot comes back when the session does. A ceiling that leaked would turn into a sidecar
	// that refuses everything after the first few pages, which reads exactly like a crash.
	if err := pool.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	mustOpen(t, pool, ephemeral("r2"))
}

// TestAChangedSiteListRelaunchesWhenIdleAndRefusesWhenNot. The list grows by a human login (spec
// §5.2), so it changes between sessions rather than during one — and a fence whose policy is swapped
// under a loading document decides differently halfway through a decision that has to precede
// execution (§5.4).
func TestAChangedSiteListRelaunchesWhenIdleAndRefusesWhenNot(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)

	first := mustOpen(t, pool, project("acme", "https://jira.example.org"))

	// While it is open: refused, and the running browser is left alone.
	_, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: project("acme", "https://jira.example.org", "https://accounts.google.com"),
	})
	if !errors.Is(err, ErrPolicyChanged) {
		t.Fatalf("open with a changed list = %v, want ErrPolicyChanged", err)
	}
	if launcher.launched()[0].stopped() != 0 {
		t.Error("the live browser was stopped by a request it refused")
	}

	// Once idle: relaunched on the new list, with no error for anyone to work around.
	if err := pool.Close(context.Background(), first.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	mustOpen(t, pool, project("acme", "https://jira.example.org", "https://accounts.google.com"))
	instances := launcher.launched()
	if len(instances) != 2 {
		t.Fatalf("launched %d browsers, want 2", len(instances))
	}
	if len(instances[1].policy.Origins) != 2 {
		t.Errorf("the new browser carries %v, want both origins", instances[1].policy.Origins)
	}
}

// TestTheSameListInAnotherOrderIsTheSamePolicy. Nothing promises the núcleo sends a list in a stable
// order, and tearing down a working browser because a slice was shuffled would be a restart nobody
// asked for — visible to the person as a page that closed itself.
func TestTheSameListInAnotherOrderIsTheSamePolicy(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)

	mustOpen(t, pool, project("acme", "https://jira.example.org", "https://accounts.google.com"))
	mustOpen(t, pool, project("acme", "accounts.google.com", "JIRA.example.org"))
	if got := len(launcher.launched()); got != 1 {
		t.Fatalf("launched %d browsers for one list in two orders, want 1", got)
	}
}

// TestALaunchThatFailsLeavesNothingBehind, and can be retried. A pool that kept the failed entry
// would answer every later open for that profile with a failure that already happened.
func TestALaunchThatFailsLeavesNothingBehind(t *testing.T) {
	launcher := &fakeLauncher{err: errors.New("no chromium")}
	pool, store := testPool(t, launcher, 4)

	_, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: ephemeral("r1"),
	})
	if err == nil {
		t.Fatal("open succeeded with a launcher that cannot launch")
	}
	dir, _ := store.Dir(profile.Ref{Kind: profile.Ephemeral, ID: "r1"})
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Errorf("a failed launch left a profile directory behind: %v", err)
	}

	launcher.mu.Lock()
	launcher.err = nil
	launcher.mu.Unlock()
	mustOpen(t, pool, ephemeral("r1"))
}

// TestAFailedNavigationDoesNotLeaveABrowserRunning. Open is two steps — launch, then navigate — and
// the second failing must not leave the first behind, or a run that hits a dead site accumulates
// browsers nobody will ever close.
func TestAFailedNavigationDoesNotLeaveABrowserRunning(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, store := testPool(t, launcher, 4)

	// The Fake is built by the launcher, so reach in the way the pool's own caller cannot: launch
	// one browser, then make its next Open fail.
	session := mustOpen(t, pool, ephemeral("r1"))
	if err := pool.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	launcher.mu.Lock()
	launcher.instances = nil
	launcher.mu.Unlock()

	failing := &failingLauncher{}
	pool = New(failing, store, profile.Limits{}, 4)
	_, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: ephemeral("r2"),
	})
	if err == nil {
		t.Fatal("open succeeded despite the navigation failing")
	}
	if failing.instance.stopped() != 1 {
		t.Errorf("the browser was stopped %d times, want 1", failing.instance.stopped())
	}
	dir, _ := store.Dir(profile.Ref{Kind: profile.Ephemeral, ID: "r2"})
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Errorf("the ephemeral profile outlived the session that never happened: %v", err)
	}
}

type failingLauncher struct {
	instance *fakeInstance
}

func (l *failingLauncher) Name() string { return "failing" }

func (l *failingLauncher) Launch(context.Context, string, fence.Policy) (Instance, error) {
	l.instance = &fakeInstance{Fake: &browser.Fake{FenceAttached: true, OpenErr: errors.New("dead site")}}
	return l.instance, nil
}

func (l *failingLauncher) LaunchHuman(context.Context, string) (Instance, error) {
	l.instance = &fakeInstance{
		Fake:    &browser.Fake{FenceAttached: true, OpenErr: errors.New("dead site")},
		headful: true,
	}
	return l.instance, nil
}

// TestShutdownStopsEveryBrowserAndSweepsTheProfiles — spec §9.3. The process the launcher spawned is
// not the browser: without this the sidecar exits, Chrome keeps running with our argv, the profile
// stays locked, and the NEXT launch inherits it.
func TestShutdownStopsEveryBrowserAndSweepsTheProfiles(t *testing.T) {
	launcher := &fakeLauncher{}
	store := profile.Store{Root: t.TempDir()}
	pool := New(launcher, store, profile.Limits{}, 4)

	mustOpen(t, pool, ephemeral("r1"))
	mustOpen(t, pool, project("acme", "https://example.org"))
	pool.Shutdown(context.Background())

	for i, instance := range launcher.launched() {
		if instance.stopped() != 1 {
			t.Errorf("browser %d was stopped %d times, want 1", i, instance.stopped())
		}
	}
	if _, err := os.Stat(filepath.Join(store.Root, "run-r1")); !os.IsNotExist(err) {
		t.Error("shutdown left an ephemeral profile behind")
	}
	if _, err := os.Stat(filepath.Join(store.Root, "project-acme")); err != nil {
		t.Errorf("shutdown took a project profile: %v", err)
	}
}

// TestAProfileIsChasedUntilTheHandlesGo.
//
// The failure this guards is not a slow disk. Stop() issues `taskkill /T /F` and then waits on the
// launcher's pid, which spec §9.3 says is not the browser — so the renderers it killed can still be
// exiting, still holding handles into the profile, when RemoveAll walks it. On Windows an open handle
// is enough to make a file undeletable. The removal used to be one call with the error thrown away,
// so spec §5.1's promise that an ephemeral profile dies with its run stopped holding and nothing
// anywhere said so.
//
// Two failures then a success is the shape measured against the gate: the window is short, because
// what closes it is a handful of processes finishing.
func TestAProfileIsChasedUntilTheHandlesGo(t *testing.T) {
	tries := 0
	err := chase(func() error {
		tries++
		if tries < 3 {
			return errors.New("the directory is not empty")
		}
		return nil
	}, time.Second, time.Millisecond)

	if err != nil {
		t.Fatalf("the profile survived a window that was long enough: %v", err)
	}
	if tries != 3 {
		t.Errorf("it asked %d times; asking once is the bug this replaced", tries)
	}
}

// TestChasingAProfileGivesUpRatherThanHanging.
//
// The other half, and the one that matters more. A directory something holds open FOREVER is a
// different fault — a browser that did not die, a handle nobody owns — and a chase with no bound
// would wear this one's clothes while hanging the session that closed.
func TestChasingAProfileGivesUpRatherThanHanging(t *testing.T) {
	held := errors.New("something still has it open")
	tries := 0
	began := time.Now()
	err := chase(func() error { tries++; return held }, 50*time.Millisecond, time.Millisecond)

	if !errors.Is(err, held) {
		t.Fatalf("giving up reported %v rather than what actually went wrong", err)
	}
	if tries < 2 {
		t.Errorf("it gave up after %d attempt(s), so the window bought nothing", tries)
	}
	if elapsed := time.Since(began); elapsed > time.Second {
		t.Errorf("it held the caller for %s; the bound is what stops this being a hang", elapsed)
	}
}

// TestAnAttemptIsMadeEvenWithNoWindowToChaseIn. A caller that passed no window meant "try", not
// "do nothing" — and a zero here would otherwise silently stop deleting profiles altogether.
func TestAnAttemptIsMadeEvenWithNoWindowToChaseIn(t *testing.T) {
	tries := 0
	if err := chase(func() error { tries++; return nil }, 0, time.Millisecond); err != nil {
		t.Fatalf("the one attempt failed: %v", err)
	}
	if tries != 1 {
		t.Errorf("a zero window produced %d attempts", tries)
	}
}
