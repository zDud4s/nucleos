package browser

import (
	"context"
	"errors"
	"reflect"
	"testing"
)

// TestOpenRequestNamesOnlyTheUrlAndThePlacement is the guard on the boundary this whole pillar
// holds.
//
// The agent chooses WHAT to look at; the núcleo chooses WHERE it happens (spec §5.3, §6.1), and the
// two live in different fields so that the second can be attached downstream of the first. What must
// never appear is a THIRD way to say where — a UserDataDir, an Identity, a Cookies — because a
// request carrying both a placement and a directory has two answers to "as whom", and the loser of
// that tie is decided by whichever line of the driver runs last.
//
// An earlier version of this test forbade a profile field outright. That was the right rule when
// there was nowhere for the núcleo's decision to go, and it stopped being the rule when the decision
// had to travel; the property that survived the change is this one.
func TestOpenRequestNamesOnlyTheUrlAndThePlacement(t *testing.T) {
	want := map[string]bool{"URL": true, "Placement": true}
	typ := reflect.TypeOf(OpenRequest{})
	for i := range typ.NumField() {
		if !want[typ.Field(i).Name] {
			t.Fatalf(
				"OpenRequest has field %q: only the url and the núcleo's placement belong here (spec §5.3, §6.1)",
				typ.Field(i).Name,
			)
		}
	}
	if typ.NumField() != len(want) {
		t.Fatalf("OpenRequest has %d fields, want %d", typ.NumField(), len(want))
	}
}

// TestAPlacementNobodyFilledInIsNotUsable. The fail-closed half of the boundary above: with a field
// to carry the decision there is also a zero value for it, and a zero value that resolved to
// something would be the default nobody chose — which for this field means an identity nobody chose.
func TestAPlacementNobodyFilledInIsNotUsable(t *testing.T) {
	var placement Placement
	if err := placement.Profile.Validate(); err == nil {
		t.Fatal("the zero Placement names a usable profile")
	}
	if len(placement.Origins) != 0 {
		t.Fatal("the zero Placement carries a site list")
	}
}

// TestOpenFailsClosedWithoutFence pins spec §6.2a: "se o interceptor não estiver atado, o separador
// não navega". The zero-value Fake has no fence, and must refuse rather than browse.
func TestOpenFailsClosedWithoutFence(t *testing.T) {
	var driver Fake // deliberately unconfigured
	_, err := driver.Open(context.Background(), OpenRequest{URL: "https://example.org/"})
	if !errors.Is(err, ErrFenceNotAttached) {
		t.Fatalf("open without a fence: got %v, want ErrFenceNotAttached", err)
	}
	if len(driver.Opened) != 1 {
		t.Fatalf("the attempt should still be recorded, got %d", len(driver.Opened))
	}
}

// TestUnavailableRefusesEverything: the stand-in driver answers closed on all six verbs. A single
// verb that answered "fine" would be a hole open exactly when nothing is set up.
func TestUnavailableRefusesEverything(t *testing.T) {
	var driver Driver = Unavailable{}
	ctx := context.Background()

	if _, err := driver.Open(ctx, OpenRequest{URL: "https://example.org/"}); err == nil {
		t.Error("Open answered nil")
	}
	if _, err := driver.Snapshot(ctx, "s1", SnapshotRequest{}); err == nil {
		t.Error("Snapshot answered nil")
	}
	if _, err := driver.Act(ctx, "s1", Action{Kind: ActionClick, Ref: "e1"}); err == nil {
		t.Error("Act answered nil")
	}
	if _, err := driver.Screenshot(ctx, "s1"); err == nil {
		t.Error("Screenshot answered nil")
	}
	if _, err := driver.Handoff(ctx, "s1", "because"); err == nil {
		t.Error("Handoff answered nil")
	}
	if err := driver.Close(ctx, "s1"); err == nil {
		t.Error("Close answered nil")
	}
}

// TestRefusalIsAnAnswerNotAnError is the second contract property from spec §14.2 step A. A fence
// refusal must reach the agent as a value it can read and move on from. If it arrived as an error
// it would be indistinguishable from a crashed browser, and the agent would retry the one thing it
// must not.
func TestRefusalIsAnAnswerNotAnError(t *testing.T) {
	driver := &Fake{
		FenceAttached: true,
		Refuse:        &Refusal{Consequence: ConsequenceMethod, Detail: "POST /orders"},
	}
	ctx := context.Background()
	session, err := driver.Open(ctx, OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}

	result, err := driver.Act(ctx, session.ID, Action{Kind: ActionClick, Ref: "e1"})
	if err != nil {
		t.Fatalf("a refusal must not be an error, got %v", err)
	}
	if result.Outcome != OutcomeRefused {
		t.Fatalf("outcome: got %q, want %q", result.Outcome, OutcomeRefused)
	}
	if result.Refusal == nil || result.Refusal.Consequence != ConsequenceMethod {
		t.Fatalf("refusal did not carry its consequence: %+v", result.Refusal)
	}
	if !result.Valid() {
		t.Error("a refusal carrying a consequence must be Valid")
	}
}

func TestActResultValidity(t *testing.T) {
	cases := []struct {
		name  string
		value ActResult
		want  bool
	}{
		{"done, no refusal", Done(), true},
		{"refused with consequence", Refused(ConsequenceForm, "login form"), true},
		{"done carrying a refusal", ActResult{
			Outcome: OutcomeDone,
			Refusal: &Refusal{Consequence: ConsequenceForm},
		}, false},
		{"refused with no refusal", ActResult{Outcome: OutcomeRefused}, false},
		{"refused with an empty consequence", ActResult{
			Outcome: OutcomeRefused,
			Refusal: &Refusal{},
		}, false},
		{"no outcome at all", ActResult{}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.value.Valid(); got != tc.want {
				t.Fatalf("Valid() = %v, want %v", got, tc.want)
			}
		})
	}
}

// TestSessionReportsBothUrls: the trust decision is a conjunction over requested and final (spec
// §5.3). A driver that reported only one of them would make that decision impossible to take.
func TestSessionReportsBothUrls(t *testing.T) {
	driver := &Fake{FenceAttached: true, FinalURL: "https://elsewhere.example/landed"}
	session, err := driver.Open(context.Background(), OpenRequest{URL: "https://example.org/start"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if session.RequestedURL != "https://example.org/start" {
		t.Errorf("requested url: got %q", session.RequestedURL)
	}
	if session.FinalURL != "https://elsewhere.example/landed" {
		t.Errorf("final url: got %q", session.FinalURL)
	}
}

func TestUnknownSessionIsNamed(t *testing.T) {
	driver := &Fake{FenceAttached: true}
	ctx := context.Background()
	if _, err := driver.Snapshot(ctx, "nope", SnapshotRequest{}); !errors.Is(err, ErrNoSuchSession) {
		t.Errorf("snapshot: got %v", err)
	}
	if err := driver.Close(ctx, "nope"); !errors.Is(err, ErrNoSuchSession) {
		t.Errorf("close: got %v", err)
	}
}

func TestCloseReleasesTheSession(t *testing.T) {
	driver := &Fake{FenceAttached: true}
	ctx := context.Background()
	session, err := driver.Open(ctx, OpenRequest{URL: "https://example.org/"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	if err := driver.Close(ctx, session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if _, err := driver.Snapshot(ctx, session.ID, SnapshotRequest{}); !errors.Is(err, ErrNoSuchSession) {
		t.Fatalf("a closed session must be gone, got %v", err)
	}
}

// TestFakeSatisfiesDriver keeps the Fake honest: if a verb is added to the interface, this stops
// compiling rather than letting the fake drift away from the contract it stands in for.
func TestFakeSatisfiesDriver(t *testing.T) {
	var _ Driver = &Fake{}
	var _ Driver = Unavailable{}
}
