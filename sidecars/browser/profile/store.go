package profile

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// Store is the profiles directory of spec §5.6, and the only thing in this process allowed to create
// or remove one.
//
// Root is the "profiles" directory itself, not the install root — one level below launch.Install, so
// that a bug in this package cannot reach the pinned Chromium next to it.
type Store struct {
	Root string
}

// Dir is where a profile lives.
//
// It validates first and checks the result afterwards. That is not belt-and-braces theatre: Validate
// is a rule about IDs, and containment is a fact about paths. Keeping both means a future relaxation
// of the rule — an ID format that allows a dot, say — cannot silently turn into an escape.
func (s Store) Dir(ref Ref) (string, error) {
	if s.Root == "" {
		return "", errors.New("profile: no profiles directory configured")
	}
	if err := ref.Validate(); err != nil {
		return "", err
	}
	dir := filepath.Join(s.Root, ref.Name())
	relative, err := filepath.Rel(s.Root, dir)
	if err != nil || relative == ".." || relative == "." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("%w: %q would land outside the profiles directory", ErrBadID, ref.ID)
	}
	return dir, nil
}

// Prepare creates the directory and returns it.
//
// 0o700 because the directory holds session cookies. Windows ignores the mode and gets its
// protection from the user's profile ACL instead; on a machine where the mode is honoured it is the
// difference between "another account can read the owner's logged-in sessions" and not.
func (s Store) Prepare(ref Ref) (string, error) {
	dir, err := s.Dir(ref)
	if err != nil {
		return "", err
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("creating profile %s: %w", ref, err)
	}
	return dir, nil
}

// Discard deletes an ephemeral profile, and refuses to delete anything else.
//
// The refusal is the point of the function. Spec §5.1 promises an ephemeral profile dies with its
// run, so something has to delete a directory tree — and the same call with the wrong Kind would
// delete every login the owner has made, with no undo and no backup behind it (§5.6 keeps profiles
// out of backup.rs deliberately). A Kind check at the top is what stands between those two outcomes.
func (s Store) Discard(ref Ref) error {
	if ref.Persistent() {
		return fmt.Errorf("%w: %s holds logins a person made", ErrPersistent, ref)
	}
	dir, err := s.Dir(ref)
	if err != nil {
		return err
	}
	if err := os.RemoveAll(dir); err != nil {
		return fmt.Errorf("discarding profile %s: %w", ref, err)
	}
	return nil
}

// Limits are spec §8's ceilings on the profiles directory. A zero field means no ceiling, which is
// what a Store built without configuration gets: unbounded is the wrong default for a disk, but a
// hard-coded number here would be a ceiling nobody chose and nobody could raise.
type Limits struct {
	// MaxProjects caps persistent profiles. Ephemeral ones are not counted: they are bounded by the
	// sweeper instead, and counting them would make a burst of runs look like too many projects.
	MaxProjects int
	// DiskBudget caps the whole directory in bytes, ephemeral included.
	DiskBudget int64
}

// ErrTooManyProfiles refuses a new project profile past the ceiling. Its message names one to forget,
// because a refusal that leaves the owner guessing which of twenty directories to delete is a refusal
// they will resolve by raising the limit.
var ErrTooManyProfiles = errors.New("profile: too many project profiles")

// ErrDiskBudget refuses a new profile because the directory is full.
var ErrDiskBudget = errors.New("profile: the profiles directory is over its disk budget")

// Admit decides whether a new profile may be created, and sweeps before it refuses.
//
// It runs only when the directory does not exist yet, and that is the honest scope: this checks the
// COUNT of profiles against the space they take together. A single profile growing without end is a
// different problem with a different answer — --disk-cache-size, applied per profile at launch — and
// pretending one mechanism covers both would leave the expensive check running on every open while
// still not catching the case it was supposed to.
//
// Spec §8's order is kept: sweep first, refuse second. Refusing while a crashed run's leftovers are
// still on disk would refuse for a reason that had already gone away.
func (s Store) Admit(ref Ref, limits Limits) error {
	dir, err := s.Dir(ref)
	if err != nil {
		return err
	}
	if _, err := os.Stat(dir); err == nil {
		// It exists. Nothing new is being created, so no ceiling applies — and the walk below is
		// skipped, which is what keeps opening a session cheap in the ordinary case.
		return nil
	}

	if limits.MaxProjects > 0 && ref.Persistent() {
		projects, err := s.projects()
		if err != nil {
			return err
		}
		if len(projects) >= limits.MaxProjects {
			return fmt.Errorf("%w: %d of %d in use; the least recently used is %s",
				ErrTooManyProfiles, len(projects), limits.MaxProjects, oldest(projects))
		}
	}

	if limits.DiskBudget > 0 {
		used, err := s.usage()
		if err != nil {
			return err
		}
		if used >= limits.DiskBudget {
			// A sweep that partly failed is not fatal here. What decides is the measurement after
			// it, and a sweep that took three of four leftovers may already have freed enough.
			_, _ = s.SweepEphemeral()
			if used, err = s.usage(); err != nil {
				return err
			}
		}
		if used >= limits.DiskBudget {
			return fmt.Errorf("%w: %d MB used of %d MB, after sweeping",
				ErrDiskBudget, used>>20, limits.DiskBudget>>20)
		}
	}
	return nil
}

type projectProfile struct {
	name     string
	modified int64
}

func (s Store) projects() ([]projectProfile, error) {
	entries, err := os.ReadDir(s.Root)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading %s: %w", s.Root, err)
	}
	var found []projectProfile
	for _, entry := range entries {
		if !entry.IsDir() || !strings.HasPrefix(entry.Name(), "project-") {
			continue
		}
		modified := int64(0)
		if info, err := entry.Info(); err == nil {
			modified = info.ModTime().Unix()
		}
		found = append(found, projectProfile{name: entry.Name(), modified: modified})
	}
	return found, nil
}

func oldest(profiles []projectProfile) string {
	if len(profiles) == 0 {
		return "none"
	}
	choice := profiles[0]
	for _, candidate := range profiles[1:] {
		if candidate.modified < choice.modified {
			choice = candidate
		}
	}
	return choice.name
}

// usage is the size of everything under Root, in bytes.
//
// Walking is not free, which is why Admit only calls it when a profile is about to be created. A
// file that cannot be stat'd is skipped rather than fatal: a browser being reaped holds files that
// come and go under the walk, and failing the open because one of them vanished mid-count would turn
// a race into an outage.
func (s Store) usage() (int64, error) {
	var total int64
	err := filepath.WalkDir(s.Root, func(_ string, entry os.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if entry.IsDir() {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return nil
		}
		total += info.Size()
		return nil
	})
	if err != nil && !os.IsNotExist(err) {
		return 0, fmt.Errorf("measuring %s: %w", s.Root, err)
	}
	return total, nil
}

// SweepEphemeral removes every ephemeral profile under Root, and reports how many it took.
//
// # Why it takes no list of live runs
//
// The obvious signature is SweepEphemeral(live map[string]bool), and it is wrong here. This process
// is the only thing that creates ephemeral profiles and it holds no state across restarts (spec §4,
// §9.1: the adapter is the parent, so its Chromes die with it). At the moment it starts, every
// "run-*" directory on disk is therefore a leftover from a crash by construction — there is no live
// run to protect, and a caller passing a list of them would be describing runs whose browsers no
// longer exist.
//
// That is the §9.3 sweeper: it is what makes §5.1's promise true after a crash rather than only
// during normal operation.
//
// Failures are collected and the sweep continues. On Windows a profile whose browser is still being
// reaped will refuse to delete, and stopping at the first one would leave the rest behind for a
// transient reason — the next startup gets another attempt at whatever failed.
func (s Store) SweepEphemeral() (int, error) {
	if s.Root == "" {
		return 0, errors.New("profile: no profiles directory configured")
	}
	entries, err := os.ReadDir(s.Root)
	if err != nil {
		if os.IsNotExist(err) {
			// Nothing has ever run here. Not a failure.
			return 0, nil
		}
		return 0, fmt.Errorf("reading %s: %w", s.Root, err)
	}

	swept := 0
	var failures []error
	for _, entry := range entries {
		if !entry.IsDir() || !strings.HasPrefix(entry.Name(), EphemeralPrefix) {
			continue
		}
		if err := os.RemoveAll(filepath.Join(s.Root, entry.Name())); err != nil {
			failures = append(failures, fmt.Errorf("sweeping %s: %w", entry.Name(), err))
			continue
		}
		swept++
	}
	return swept, errors.Join(failures...)
}
