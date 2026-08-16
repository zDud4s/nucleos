package browser

import (
	"context"
	"errors"
	"reflect"
	"strings"
	"testing"
)

// TestOpenRequestCannotChooseAProfile is the guard on the boundary this whole pillar holds.
//
// The agent chooses WHAT to look at; the núcleo chooses WHERE it happens (spec §5.3, §6.1). If a
// profile, user-data-dir or identity field ever appears on OpenRequest, the caller can pick which
// logged-in identity it browses under — and every allowlist above becomes decoration. That would
// arrive as a one-line struct change in a hurry, which is exactly the sort of thing a test has to
// be standing in front of.
func TestOpenRequestCannotChooseAProfile(t *testing.T) {
	forbidden := []string{"profile", "userdata", "userdatadir", "identity", "session", "cookie", "dir"}
	typ := reflect.TypeOf(OpenRequest{})
	for i := range typ.NumField() {
		name := strings.ToLower(typ.Field(i).Name)
		for _, bad := range forbidden {
			if strings.Contains(name, bad) {
				t.Fatalf(
					"OpenRequest has field %q: the caller must never choose where it browses (spec §5.3, §6.1)",
					typ.Field(i).Name,
				)
			}
		}
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
	if _, err := driver.Snapshot(ctx, "s1"); err == nil {
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
	if _, err := driver.Snapshot(ctx, "nope"); !errors.Is(err, ErrNoSuchSession) {
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
	if _, err := driver.Snapshot(ctx, session.ID); !errors.Is(err, ErrNoSuchSession) {
		t.Fatalf("a closed session must be gone, got %v", err)
	}
}

// TestFakeSatisfiesDriver keeps the Fake honest: if a verb is added to the interface, this stops
// compiling rather than letting the fake drift away from the contract it stands in for.
func TestFakeSatisfiesDriver(t *testing.T) {
	var _ Driver = &Fake{}
	var _ Driver = Unavailable{}
}
