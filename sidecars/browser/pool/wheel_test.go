package pool

import (
	"context"
	"errors"
	"os"
	"testing"

	"nucleosbrowser/browser"
	"nucleosbrowser/profile"
)

// readDir is how these tests ask whether a profile directory is still on disk.
func readDir(path string) ([]os.DirEntry, error) { return os.ReadDir(path) }

func takeWheel(t *testing.T, pool *Pool, session browser.SessionID, placement browser.Placement) browser.Wheel {
	t.Helper()
	wheel, err := pool.TakeWheel(context.Background(), browser.WheelRequest{
		Session:   session,
		URL:       "https://jira.example.org/login",
		Placement: placement,
	})
	if err != nil {
		t.Fatalf("take the wheel: %v", err)
	}
	return wheel
}

// Spec §4.2: the handover is a PROCESS swap over the same profile directory. This is the shape of
// that sentence — the headless browser is stopped, a headful one is started, and the directory both
// were given is the same one, because the directory is the identity.
func TestTheWheelSwapsTheProcessAndKeepsTheProfile(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 2)

	placement := project("acme", "https://jira.example.org")
	agent := mustOpen(t, pool, placement)

	wheel := takeWheel(t, pool, agent.ID, placement)
	if wheel.Mode != browser.ModeHuman {
		t.Fatalf("mode = %q, want human", wheel.Mode)
	}

	instances := launcher.launched()
	if len(instances) != 2 {
		t.Fatalf("launched %d browsers, want the headless one and then the headful one", len(instances))
	}
	headless, headful := instances[0], instances[1]
	if headless.headful {
		t.Fatal("the first launch must be the agent's headless browser")
	}
	if !headful.headful {
		t.Fatal("the second launch must be headful")
	}
	if headless.dir != headful.dir {
		t.Fatalf("the person got a different profile: %q then %q", headless.dir, headful.dir)
	}
	if headless.stopped() != 1 {
		t.Fatalf("the headless browser was stopped %d times, want exactly one graceful close", headless.stopped())
	}
}

// The agent's session was in a THROWAWAY — which is the ordinary case, since a login wall is what
// sends the agent to ask — and the window opens in the project's profile instead (spec §4.5). The
// throwaway is not where the person logs in, because it is deleted with the run.
func TestAHandoverFromAThrowawayOpensTheProjectProfile(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, store := testPool(t, launcher, 2)

	agent := mustOpen(t, pool, ephemeral("r7"))
	wheel := takeWheel(t, pool, agent.ID, project("acme"))

	if wheel.Session == agent.ID {
		t.Fatal("the person's session is a new one, not the agent's renamed")
	}
	instances := launcher.launched()
	if len(instances) != 2 {
		t.Fatalf("launched %d browsers, want two", len(instances))
	}
	throwaway, err := store.Dir(profile.Ref{Kind: profile.Ephemeral, ID: "r7"})
	if err != nil {
		t.Fatal(err)
	}
	if instances[1].dir == throwaway {
		t.Fatal("the person was sent to the profile that dies with the run")
	}
	if instances[0].stopped() != 1 {
		t.Fatal("the run's browser was left running behind the person's window")
	}
	if _, err := readDir(throwaway); err == nil {
		t.Fatal("the throwaway profile survived the handover")
	}
}

// Spec §4.1: one browser per profile, and no gap. While the person drives, an agent open on that
// profile is refused — not queued, because §4.4 rule 2 puts no bound on how long a person takes, and
// not relaunched, because that would take the window away mid-login.
func TestWhileAPersonDrivesTheAgentIsRefusedThatProfile(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 3)

	placement := project("acme", "https://jira.example.org")
	agent := mustOpen(t, pool, placement)
	takeWheel(t, pool, agent.ID, placement)

	_, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://jira.example.org/",
		Placement: placement,
	})
	if !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Fatalf("open during a handover: %v, want ErrPersonIsDriving", err)
	}

	// A DIFFERENT profile is untouched. The refusal is about the browser holding this directory, not
	// about the pillar being busy.
	if _, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.net/",
		Placement: ephemeral("r9"),
	}); err != nil {
		t.Fatalf("another profile was refused too: %v", err)
	}
}

// Spec §4.1 again, from the other side: taking the wheel on a profile that already had agent sessions
// closes them, and SAYS SO. The núcleo has rows for those sessions, and a row left saying "open"
// about a browser that has been stopped is a row the UI offers to hand over a second time.
func TestTakingTheWheelReportsTheSessionsItClosed(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)

	placement := project("acme", "https://jira.example.org")
	first := mustOpen(t, pool, placement)
	second := mustOpen(t, pool, placement)

	// The wheel is asked for from `first`; `second` is collateral, and is the one that must be named.
	wheel := takeWheel(t, pool, first.ID, placement)
	if len(wheel.Displaced) != 1 || wheel.Displaced[0] != second.ID {
		t.Fatalf("displaced = %v, want exactly [%s]", wheel.Displaced, second.ID)
	}

	// And neither of them is addressable any more.
	for _, id := range []browser.SessionID{first.ID, second.ID} {
		if _, err := pool.Snapshot(context.Background(), id, browser.SnapshotRequest{}); !errors.Is(err, browser.ErrNoSuchSession) {
			t.Fatalf("session %s survived the handover: %v", id, err)
		}
	}
}

// Spec §5.3a: what comes back is the navigation the person made, and it comes back BEFORE the
// browser is stopped — the recorder lives in the driver that is about to be shut down.
func TestTheWheelComesBackWithTheChainThePersonWalked(t *testing.T) {
	chain := []string{
		"https://jira.example.org/login",
		"https://accounts.google.com/o/oauth2/auth",
		"https://jira.example.org/browse/X-1",
	}
	launcher := &fakeLauncher{chain: chain}
	pool, _ := testPool(t, launcher, 2)

	placement := project("acme")
	wheel := takeWheel(t, pool, "", placement)

	returned, err := pool.ReturnWheel(context.Background(), wheel.Session)
	if err != nil {
		t.Fatalf("return the wheel: %v", err)
	}
	if len(returned.Chain) != len(chain) {
		t.Fatalf("chain = %v, want %v", returned.Chain, chain)
	}
	for i := range chain {
		if returned.Chain[i] != chain[i] {
			t.Fatalf("chain[%d] = %q, want %q", i, returned.Chain[i], chain[i])
		}
	}

	// The browser is down and the PROJECT profile is not: Store.Discard refuses a project profile,
	// which is why the handover targets one. Deleting it here would delete the login just made.
	instances := launcher.launched()
	if instances[len(instances)-1].stopped() != 1 {
		t.Fatal("the person's browser was not closed gracefully")
	}
	if _, err := readDir(instances[len(instances)-1].dir); err != nil {
		t.Fatalf("the project profile was deleted with the window: %v", err)
	}
}

// A return for a session no person was ever handed is refused, and refused distinguishably. Without
// this the núcleo could grant a chain read off an agent session — which is empty, so nothing would be
// granted, and the bug would present as "the login did not stick" instead of as an error.
func TestReturningAWheelNobodyWasGivenIsRefused(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 2)

	agent := mustOpen(t, pool, project("acme", "https://jira.example.org"))
	if _, err := pool.ReturnWheel(context.Background(), agent.ID); !errors.Is(err, browser.ErrNoWheelToReturn) {
		t.Fatalf("returning an agent session: %v, want ErrNoWheelToReturn", err)
	}
	if _, err := pool.ReturnWheel(context.Background(), "nope"); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Fatalf("returning an unknown session: %v, want ErrNoSuchSession", err)
	}
}

// Spec §4.5, at the boundary the núcleo is trusted to respect and this refuses to assume: a handover
// into a throwaway is refused outright rather than opened.
func TestTheWheelIsNeverHandedIntoAThrowaway(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 2)

	_, err := pool.TakeWheel(context.Background(), browser.WheelRequest{
		URL:       "https://jira.example.org/",
		Placement: ephemeral("r7"),
	})
	if !errors.Is(err, browser.ErrNotAProjectProfile) {
		t.Fatalf("handover into a throwaway: %v, want ErrNotAProjectProfile", err)
	}
	if len(launcher.launched()) != 0 {
		t.Fatal("a browser was started for a handover that had to be refused")
	}
}

// Spec §4.4a: the headful browser does not start. The failure is reported, the agent does NOT get
// its browser back, and the profile is left alone — the núcleo turns this into a failed delivery.
func TestAHeadfulThatWillNotStartLeavesNobodyDriving(t *testing.T) {
	boom := errors.New("no display")
	launcher := &fakeLauncher{humanErr: boom}
	pool, _ := testPool(t, launcher, 2)

	placement := project("acme", "https://jira.example.org")
	agent := mustOpen(t, pool, placement)

	_, err := pool.TakeWheel(context.Background(), browser.WheelRequest{
		Session:   agent.ID,
		URL:       "https://jira.example.org/",
		Placement: placement,
	})
	if !errors.Is(err, boom) {
		t.Fatalf("take the wheel: %v, want the launch failure", err)
	}

	// The profile is free again rather than stuck holding a browser that never existed: the person
	// can retry, which is what §4.4a says they may do.
	if _, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: placement,
	}); err != nil {
		t.Fatalf("the profile stayed locked after a failed handover: %v", err)
	}
}

// Spec §10's "Esquecer", and the counterweight to a list that only grows: the browser goes down and
// the directory goes with it, project profile or not.
//
// The order matters on Windows and is asserted by the fact that the delete succeeds at all — a
// running Chrome holds files under its --user-data-dir open, and a RemoveAll over them fails halfway.
func TestForgettingAProfileStopsItsBrowserAndDeletesTheDirectory(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, store := testPool(t, launcher, 4)

	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)
	dir, err := store.Dir(placement.Profile)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := readDir(dir); err != nil {
		t.Fatalf("the profile was never created: %v", err)
	}

	stopped, err := pool.Forget(context.Background(), placement.Profile)
	if err != nil {
		t.Fatalf("forget: %v", err)
	}
	if len(stopped) != 1 || stopped[0] != session.ID {
		t.Fatalf("stopped = %v, want [%s]", stopped, session.ID)
	}
	if launcher.launched()[0].stopped() != 1 {
		t.Fatal("the browser was left running over a directory that no longer exists")
	}
	if _, err := readDir(dir); err == nil {
		t.Fatal("the profile directory survived being forgotten")
	}

	// And the profile is free to be used again — forgetting is not a tombstone.
	if _, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://jira.example.org/",
		Placement: placement,
	}); err != nil {
		t.Fatalf("the profile stayed unusable after being forgotten: %v", err)
	}
}
