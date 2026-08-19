//go:build browsergate

package gate_test

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"nucleosbrowser/browser"
	"nucleosbrowser/fence"
	"nucleosbrowser/pool"
	"nucleosbrowser/profile"
)

// The profile group. Everything here is about spec §4.2's premise — "the profile is the identity,
// the process is disposable" — and about §5.1's promise that an ephemeral profile dies with its run.
//
// Both are claims about what a REAL browser leaves on a REAL disk, so neither can be tested anywhere
// but here. The unit tests in package pool prove the bookkeeping: which browser a session is routed
// to, what gets deleted, what gets refused. They cannot prove that Chrome wrote the cookie down
// before it exited, and that is the half that decides whether a login the person made survives.

// localLauncher is the production launcher with one amendment: it admits the test site on loopback.
//
// pool.Policy deliberately leaves the loopback list empty, because in production nothing on this
// machine is a browsing destination — the núcleo's API and the browser's own token-less debugging
// port are what live there. The site these tests drive is on 127.0.0.1, so the gate has to open that
// door itself rather than production growing a setting that would open it for everyone. Amending the
// policy at the launcher seam is the smallest way to say that, and it leaves pool.Policy's refusal
// exactly as strict as it ships.
type localLauncher struct {
	inner pool.ChromeLauncher
	site  *site
}

func (l localLauncher) Name() string { return l.inner.Name() }

func (l localLauncher) Launch(ctx context.Context, dir string, policy fence.Policy) (pool.Instance, error) {
	policy.Loopback = []string{l.site.origin()}
	return l.inner.Launch(ctx, dir, policy)
}

// LaunchHuman needs no amendment: a headful browser has no fence at all (spec §6.4), so there is no
// loopback list to open. Straight through, which is also the honest thing for a gate to do — this is
// the production launch, unmodified.
func (l localLauncher) LaunchHuman(ctx context.Context, dir string) (pool.Instance, error) {
	return l.inner.LaunchHuman(ctx, dir)
}

func pooled(t *testing.T, s *site) (*pool.Pool, profile.Store) {
	t.Helper()
	store := profile.Store{Root: filepath.Join(t.TempDir(), "profiles")}
	browsers := pool.New(
		localLauncher{inner: pool.ChromeLauncher{ExecutablePath: chromium(t)}, site: s},
		store,
		profile.Limits{},
		4,
	)
	t.Cleanup(func() { browsers.Shutdown(context.Background()) })
	return browsers, store
}

// projectAt is a placement for a project profile. The origin list names a host the tests never
// visit: what actually admits the local site is the loopback amendment above, and a project profile
// with an empty list is refused by the fence (correctly — it would admit no document at all).
func projectAt(id string) browser.Placement {
	return browser.Placement{
		Profile: profile.Ref{Kind: profile.Project, ID: id},
		Origins: []string{"https://nucleos.invalid"},
	}
}

func openAt(t *testing.T, browsers *pool.Pool, placement browser.Placement, url string) browser.Session {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	session, err := browsers.Open(ctx, browser.OpenRequest{URL: url, Placement: placement})
	if err != nil {
		t.Fatalf("open %s in %s: %v", url, placement.Profile, err)
	}
	return session
}

func closeSession(t *testing.T, browsers *pool.Pool, id browser.SessionID) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	if err := browsers.Close(ctx, id); err != nil {
		t.Fatalf("close: %v", err)
	}
}

// TestAProjectProfileKeepsTheLoginAcrossTheProcess is spec §4.2's premise, measured end to end and
// through the pool's own close: a cookie set in one session is presented by the next, with the
// browser closed and relaunched in between.
//
// It is the reason Shutdown closes rather than kills. The spike measured that a hard kill loses a
// write from seconds earlier because Chrome only flushes the profile on a graceful close — which,
// for this pillar, means losing the login the person had just made.
func TestAProjectProfileKeepsTheLoginAcrossTheProcess(t *testing.T) {
	s := newSite(t)
	browsers, store := pooled(t, s)
	placement := projectAt("acme")

	first := openAt(t, browsers, placement, s.origin()+"/set-cookie")
	if first.Refusal != nil {
		t.Fatalf("the fence refused the site the gate admits: %+v", first.Refusal)
	}
	closeSession(t, browsers, first.ID)

	dir, err := store.Dir(placement.Profile)
	if err != nil {
		t.Fatalf("dir: %v", err)
	}
	if _, err := os.Stat(dir); err != nil {
		t.Fatalf("the project profile was deleted with its session: %v", err)
	}

	second := openAt(t, browsers, placement, s.origin()+"/whoami")
	if second.Refusal != nil {
		t.Fatalf("the second session was refused: %+v", second.Refusal)
	}
	if !s.reached("WHOAMI 1", 15*time.Second) {
		t.Fatal("the cookie did not survive the browser it was set in")
	}
}

// TestAnEphemeralProfileForgetsEverything is the control, and spec §5.1's promise in the same
// motion. Same code path, same site, same two navigations — the only difference is the kind of
// profile, and the outcome must be the opposite one.
//
// Without it, a pool that never deleted anything would pass the test above and quietly turn every
// throwaway session into a persistent identity.
func TestAnEphemeralProfileForgetsEverything(t *testing.T) {
	s := newSite(t)
	browsers, store := pooled(t, s)
	placement := browser.Placement{Profile: profile.Ref{Kind: profile.Ephemeral, ID: "r1"}}

	first := openAt(t, browsers, placement, s.origin()+"/set-cookie")
	dir, err := store.Dir(placement.Profile)
	if err != nil {
		t.Fatalf("dir: %v", err)
	}
	if _, err := os.Stat(dir); err != nil {
		t.Fatalf("the profile directory was never created: %v", err)
	}
	closeSession(t, browsers, first.ID)

	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatalf("the ephemeral profile outlived its last session: %v", err)
	}

	openAt(t, browsers, placement, s.origin()+"/whoami")
	if !s.reached("WHOAMI none", 15*time.Second) {
		t.Fatal("a cookie crossed into a fresh ephemeral profile")
	}
}

// TestTwoProfilesRunAtOnceWithoutInheritingEachOther.
//
// Starting a Chromium over a directory another instance holds does not fail — it hands off to the
// survivor and exits, so the caller drives a browser it never configured (launch.ErrInheritedInstance).
// The pool's rule is one browser per profile; this measures the other half, that two DIFFERENT
// profiles really are two browsers and not one wearing two names.
func TestTwoProfilesRunAtOnceWithoutInheritingEachOther(t *testing.T) {
	s := newSite(t)
	browsers, _ := pooled(t, s)

	logged := openAt(t, browsers, projectAt("acme"), s.origin()+"/set-cookie")
	stranger := openAt(t, browsers, browser.Placement{
		Profile: profile.Ref{Kind: profile.Ephemeral, ID: "r1"},
	}, s.origin()+"/whoami")
	if logged.ID == stranger.ID {
		t.Fatalf("both sessions answer to %q", logged.ID)
	}
	if !s.reached("WHOAMI none", 15*time.Second) {
		t.Fatal("the ephemeral session presented the project profile's cookie")
	}

	// Both are still usable afterwards. An inherited instance shows up here: one of the two drivers
	// is talking to a browser that closed, and its next call fails.
	for _, session := range []browser.Session{logged, stranger} {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		if _, err := browsers.Snapshot(ctx, session.ID, browser.SnapshotRequest{}); err != nil {
			t.Errorf("snapshot on %s: %v", session.ID, err)
		}
		cancel()
	}
}

// TestTheSweepTakesWhatACrashLeft — spec §9.3, against a directory a real browser really wrote to.
//
// The unit test proves the rule about names; this proves the profile is actually deletable after the
// pool has finished with the browser, which on Windows is a different question: a file handle held by
// a process that has not finished dying makes RemoveAll fail, and the sweep would then be reporting
// success while leaving the disk full.
func TestTheSweepTakesWhatACrashLeft(t *testing.T) {
	s := newSite(t)
	browsers, store := pooled(t, s)

	openAt(t, browsers, browser.Placement{
		Profile: profile.Ref{Kind: profile.Ephemeral, ID: "crashed"},
	}, s.origin()+"/")
	// Shutdown is what the signal handler runs, and it is also what a crash never gets to run — so
	// this leaves the directory in the state the sweeper is meant to find, minus the corpse.
	browsers.Shutdown(context.Background())

	swept, err := store.SweepEphemeral()
	if err != nil {
		t.Fatalf("sweep: %v", err)
	}
	entries, err := os.ReadDir(store.Root)
	if err != nil {
		t.Fatalf("reading the profiles directory: %v", err)
	}
	for _, entry := range entries {
		if entry.IsDir() && entry.Name() == "run-crashed" {
			t.Fatalf("the ephemeral profile survived shutdown and a sweep (swept %d)", swept)
		}
	}
}
