package profile

import (
	"errors"
	"path/filepath"
	"strings"
	"testing"
)

// TestAnIDThatCannotBeADirectoryNameIsRefused. Every row is a way a directory name goes wrong, and
// the two that matter most are the traversal and the case fold: the first turns Discard into a
// delete of whatever is next door, the second silently merges two identities into one set of cookies.
func TestAnIDThatCannotBeADirectoryNameIsRefused(t *testing.T) {
	cases := []struct {
		name string
		id   string
	}{
		{"empty", ""},
		{"parent", ".."},
		{"traversal", "../chromium-1234"},
		{"forward slash", "a/b"},
		{"backslash", `a\b`},
		{"dot", "a.b"},
		{"leading dot", ".hidden"},
		{"trailing dot, which Windows strips", "a."},
		{"uppercase, which Windows folds", "Acme"},
		{"space", "two words"},
		{"colon, an NTFS stream", "a:b"},
		{"null", "a\x00b"},
		{"too long", strings.Repeat("a", MaxIDLength+1)},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			ref := Ref{Kind: Project, ID: testCase.id}
			if err := ref.Validate(); !errors.Is(err, ErrBadID) {
				t.Fatalf("Validate(%q) = %v, want ErrBadID", testCase.id, err)
			}
			// And the store must not produce a path for it either — the rule and the path have to
			// agree, because only one of them is checked at the call site.
			if _, err := (Store{Root: t.TempDir()}).Dir(ref); err == nil {
				t.Fatalf("Dir(%q) returned a path", testCase.id)
			}
		})
	}
}

// TestAnOrdinaryIDIsAccepted is the control. Without it a Validate that refused everything would
// pass the table above and make the pillar unable to open a single session.
func TestAnOrdinaryIDIsAccepted(t *testing.T) {
	for _, id := range []string{"42", "run-1", "a", "b_2", strings.Repeat("a", MaxIDLength),
		"7f3a1c2e-9b4d-4a6f-8e1c-2d3b5a7c9e0f"} {
		if err := (Ref{Kind: Project, ID: id}).Validate(); err != nil {
			t.Errorf("Validate(%q) = %v, want nil", id, err)
		}
	}
}

// TestAProfileWithNoKindIsRefused. Both available defaults are wrong in a way nobody would see: one
// loses the logins, the other hands them to a stranger's page.
func TestAProfileWithNoKindIsRefused(t *testing.T) {
	for _, kind := range []Kind{"", "Project", "temporary", "PROJECT"} {
		err := (Ref{Kind: kind, ID: "42"}).Validate()
		if !errors.Is(err, ErrNoKind) {
			t.Errorf("Validate(kind=%q) = %v, want ErrNoKind", kind, err)
		}
	}
}

// TestTheTwoKindsLandInDifferentDirectories, both under the profiles directory and neither above it.
func TestTheTwoKindsLandInDifferentDirectories(t *testing.T) {
	root := t.TempDir()
	store := Store{Root: root}

	project, err := store.Dir(Ref{Kind: Project, ID: "42"})
	if err != nil {
		t.Fatalf("project: %v", err)
	}
	ephemeral, err := store.Dir(Ref{Kind: Ephemeral, ID: "42"})
	if err != nil {
		t.Fatalf("ephemeral: %v", err)
	}

	if project == ephemeral {
		// The same ID in both kinds is not a contrived case: a run and a project can easily be
		// numbered 42, and sharing a directory would put a stranger's page in the logged-in profile.
		t.Fatalf("both kinds of 42 landed in %s", project)
	}
	if filepath.Base(project) != "project-42" {
		t.Errorf("project directory is %s", filepath.Base(project))
	}
	if filepath.Base(ephemeral) != "run-42" {
		t.Errorf("ephemeral directory is %s", filepath.Base(ephemeral))
	}
	for _, dir := range []string{project, ephemeral} {
		if filepath.Dir(dir) != root {
			t.Errorf("%s is not directly under %s", dir, root)
		}
	}
}

// TestTheSweeperAndTheNamingAgree pins the one coupling that would leave orphans forever: the
// sweeper matches a prefix, and the naming produces one. Two spellings of the same idea would leak
// every ephemeral profile ever created, silently and only after a crash.
func TestTheSweeperAndTheNamingAgree(t *testing.T) {
	name := Ref{Kind: Ephemeral, ID: "1"}.Name()
	if !strings.HasPrefix(name, EphemeralPrefix) {
		t.Fatalf("%q does not carry the prefix the sweeper looks for (%q)", name, EphemeralPrefix)
	}
	if strings.HasPrefix(Ref{Kind: Project, ID: "1"}.Name(), EphemeralPrefix) {
		t.Fatal("a project profile carries the sweeper's prefix, and would be deleted at startup")
	}
}
