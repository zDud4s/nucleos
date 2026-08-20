package profile

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// TestDiscardRefusesAProjectProfile. The one irreversible action in this package, and the only guard
// against it is the Kind check: profiles are deliberately outside backup.rs (spec §5.6), so a project
// profile deleted here is every login the owner made, gone with nowhere to restore it from.
func TestDiscardRefusesAProjectProfile(t *testing.T) {
	store := Store{Root: t.TempDir()}
	ref := Ref{Kind: Project, ID: "42"}
	dir, err := store.Prepare(ref)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	cookie := filepath.Join(dir, "Cookies")
	if err := os.WriteFile(cookie, []byte("a session"), 0o600); err != nil {
		t.Fatalf("writing: %v", err)
	}

	if err := store.Discard(ref); !errors.Is(err, ErrPersistent) {
		t.Fatalf("Discard = %v, want ErrPersistent", err)
	}
	if _, err := os.Stat(cookie); err != nil {
		t.Fatalf("the project profile was deleted anyway: %v", err)
	}
}

// TestDiscardRemovesAnEphemeralProfile is the control for the test above, and spec §5.1's promise
// under normal operation: a refusal that covered both kinds would pass that test and leave every
// throwaway profile on disk.
func TestDiscardRemovesAnEphemeralProfile(t *testing.T) {
	store := Store{Root: t.TempDir()}
	ref := Ref{Kind: Ephemeral, ID: "run7"}
	dir, err := store.Prepare(ref)
	if err != nil {
		t.Fatalf("prepare: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "Cookies"), []byte("a session"), 0o600); err != nil {
		t.Fatalf("writing: %v", err)
	}

	if err := store.Discard(ref); err != nil {
		t.Fatalf("discard: %v", err)
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatalf("the ephemeral profile survived its run: %v", err)
	}
}

// TestTheSweepTakesEphemeralProfilesAndLeavesEverythingElse — spec §9.3, the sweeper that makes the
// promise true after a crash and not only during a clean shutdown.
func TestTheSweepTakesEphemeralProfilesAndLeavesEverythingElse(t *testing.T) {
	root := t.TempDir()
	store := Store{Root: root}
	for _, ref := range []Ref{
		{Kind: Ephemeral, ID: "1"},
		{Kind: Ephemeral, ID: "2"},
		{Kind: Project, ID: "acme"},
	} {
		if _, err := store.Prepare(ref); err != nil {
			t.Fatalf("prepare %s: %v", ref, err)
		}
	}
	// A file, not a directory, whose name starts the same way. A sweeper matching on the name alone
	// would take it.
	stray := filepath.Join(root, "run-notes.txt")
	if err := os.WriteFile(stray, []byte("x"), 0o600); err != nil {
		t.Fatalf("writing: %v", err)
	}

	swept, err := store.SweepEphemeral()
	if err != nil {
		t.Fatalf("sweep: %v", err)
	}
	if swept != 2 {
		t.Errorf("swept %d, want 2", swept)
	}
	if _, err := os.Stat(filepath.Join(root, "project-acme")); err != nil {
		t.Errorf("the sweep took a project profile: %v", err)
	}
	if _, err := os.Stat(stray); err != nil {
		t.Errorf("the sweep took a file that merely shared the prefix: %v", err)
	}
	for _, gone := range []string{"run-1", "run-2"} {
		if _, err := os.Stat(filepath.Join(root, gone)); !os.IsNotExist(err) {
			t.Errorf("%s survived the sweep: %v", gone, err)
		}
	}
}

// TestTheSweepIsQuietWhenNothingHasEverRun. First start on a new machine: the profiles directory does
// not exist yet, and reporting that as a failure would make a clean install look broken (spec §9.4's
// reason for three health states rather than one).
func TestTheSweepIsQuietWhenNothingHasEverRun(t *testing.T) {
	store := Store{Root: filepath.Join(t.TempDir(), "never-created")}
	swept, err := store.SweepEphemeral()
	if err != nil {
		t.Fatalf("sweep: %v", err)
	}
	if swept != 0 {
		t.Errorf("swept %d from a directory that does not exist", swept)
	}
}

// TestTheProjectCeilingRefusesAndSaysWhichToForget — spec §8's max_profiles, including the half that
// is easy to drop: a refusal that does not name a candidate gets resolved by raising the limit.
func TestTheProjectCeilingRefusesAndSaysWhichToForget(t *testing.T) {
	root := t.TempDir()
	store := Store{Root: root}
	for _, id := range []string{"one", "two"} {
		if _, err := store.Prepare(Ref{Kind: Project, ID: id}); err != nil {
			t.Fatalf("prepare: %v", err)
		}
	}
	// Make "one" the least recently used, by a margin no filesystem's timestamp resolution will lose.
	old := time.Now().Add(-72 * time.Hour)
	if err := os.Chtimes(filepath.Join(root, "project-one"), old, old); err != nil {
		t.Fatalf("chtimes: %v", err)
	}

	err := store.Admit(Ref{Kind: Project, ID: "three"}, Limits{MaxProjects: 2})
	if !errors.Is(err, ErrTooManyProfiles) {
		t.Fatalf("Admit = %v, want ErrTooManyProfiles", err)
	}
	if !strings.Contains(err.Error(), "project-one") {
		t.Errorf("the refusal does not name the profile to forget: %v", err)
	}

	// The controls. An ephemeral profile is not a project and must not be refused by this ceiling,
	// and a project that ALREADY exists is not a new one — otherwise reaching the limit would lock
	// the owner out of the profiles they already have.
	if err := store.Admit(Ref{Kind: Ephemeral, ID: "r1"}, Limits{MaxProjects: 2}); err != nil {
		t.Errorf("an ephemeral profile was refused by the project ceiling: %v", err)
	}
	if err := store.Admit(Ref{Kind: Project, ID: "one"}, Limits{MaxProjects: 2}); err != nil {
		t.Errorf("an existing project was refused by the ceiling: %v", err)
	}
}

// TestTheDiskBudgetSweepsBeforeItRefuses — spec §8, in that order. Refusing while a crashed run's
// leftovers are still on disk refuses for a reason that has already gone away.
func TestTheDiskBudgetSweepsBeforeItRefuses(t *testing.T) {
	root := t.TempDir()
	store := Store{Root: root}
	fill := func(ref Ref, bytes int) {
		dir, err := store.Prepare(ref)
		if err != nil {
			t.Fatalf("prepare: %v", err)
		}
		if err := os.WriteFile(filepath.Join(dir, "Cache"), make([]byte, bytes), 0o600); err != nil {
			t.Fatalf("writing: %v", err)
		}
	}
	fill(Ref{Kind: Ephemeral, ID: "dead"}, 8000)
	fill(Ref{Kind: Project, ID: "acme"}, 1000)

	// Over budget only because of the leftover. The sweep frees it and the admission goes through.
	if err := store.Admit(Ref{Kind: Project, ID: "new"}, Limits{DiskBudget: 5000}); err != nil {
		t.Fatalf("Admit = %v, want the sweep to have made room", err)
	}
	if _, err := os.Stat(filepath.Join(root, "run-dead")); !os.IsNotExist(err) {
		t.Errorf("the leftover was not swept: %v", err)
	}

	// And when sweeping cannot help, it refuses rather than filling the disk in silence.
	err := store.Admit(Ref{Kind: Project, ID: "another"}, Limits{DiskBudget: 500})
	if !errors.Is(err, ErrDiskBudget) {
		t.Fatalf("Admit = %v, want ErrDiskBudget", err)
	}
	if _, err := os.Stat(filepath.Join(root, "project-acme")); err != nil {
		t.Errorf("the budget check took a project profile: %v", err)
	}
}

// TestNoLimitsMeansNoCeiling. The zero Limits is what a Store built before configuration arrives
// gets, and it must not refuse everything: a ceiling of zero read as "zero allowed" would make the
// pillar unable to open anything at all, in a way that looks exactly like the ceiling working.
func TestNoLimitsMeansNoCeiling(t *testing.T) {
	store := Store{Root: t.TempDir()}
	for _, id := range []string{"a", "b", "c"} {
		if err := store.Admit(Ref{Kind: Project, ID: id}, Limits{}); err != nil {
			t.Fatalf("Admit(%s) = %v", id, err)
		}
		if _, err := store.Prepare(Ref{Kind: Project, ID: id}); err != nil {
			t.Fatalf("prepare: %v", err)
		}
	}
}

// TestAStoreWithNoRootRefusesToDoAnything. The zero value must not resolve to the working directory:
// Discard on a relative path would delete whatever "run-1" happens to be next to the process.
func TestAStoreWithNoRootRefusesToDoAnything(t *testing.T) {
	var store Store
	if _, err := store.Dir(Ref{Kind: Ephemeral, ID: "1"}); err == nil {
		t.Error("Dir returned a path with no root configured")
	}
	if _, err := store.Prepare(Ref{Kind: Ephemeral, ID: "1"}); err == nil {
		t.Error("Prepare created a directory with no root configured")
	}
	if _, err := store.SweepEphemeral(); err == nil {
		t.Error("SweepEphemeral ran with no root configured")
	}
}
