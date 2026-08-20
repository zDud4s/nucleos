//go:build browsergate

package gate_test

import (
	"context"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/browser"
)

// The wheel group. Everything here is spec §4.2's mechanism rather than its premise: the profile is
// the identity and the process is disposable, so handing the wheel over means killing one browser
// and starting another over the same --user-data-dir.
//
// It cannot be tested anywhere else. The unit tests in package pool prove the bookkeeping — which
// browser a session routes to, what is displaced, what is refused — with a launcher that returns a
// struct. They cannot prove that a REAL Chrome, closed gracefully and relaunched headful over the
// same directory, still presents the cookie; and that is the entire promise the handover makes.
//
// These open a visible window on the machine that runs them. That is what headful means, and a gate
// that avoided it would be measuring the one mode the person never sees.

// TestTheWheelCrossesTheProcessWithTheLoginIntact.
//
// Spec §4.2, end to end and through the pool's own paths: the agent's headless browser sets a cookie,
// a person takes the wheel, the headful browser over the SAME profile presents that cookie, and the
// headless browser that comes after the wheel is returned still has it.
//
// The middle step is the one nothing else covers. Everything up to it was already proven by the
// profile group; what is new is that the process swap does not lose the profile — which is exactly
// the failure a hard kill produces, and the reason Shutdown closes before it kills.
func TestTheWheelCrossesTheProcessWithTheLoginIntact(t *testing.T) {
	s := newSite(t)
	browsers, _ := pooled(t, s)
	placement := projectAt("acme")

	agent := openAt(t, browsers, placement, s.origin()+"/set-cookie")
	if agent.Refusal != nil {
		t.Fatalf("the fence refused the site the gate admits: %+v", agent.Refusal)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	wheel, err := browsers.TakeWheel(ctx, browser.WheelRequest{
		Session:   agent.ID,
		URL:       s.origin() + "/whoami",
		Placement: placement,
	})
	if err != nil {
		t.Fatalf("take the wheel: %v", err)
	}
	if wheel.Mode != browser.ModeHuman {
		t.Fatalf("mode = %q, want human", wheel.Mode)
	}

	// The person's window presents the cookie the agent's browser wrote. This is the sentence the
	// whole handover exists to make true, and the process it runs in is not the one that wrote it.
	if !s.reached("WHOAMI 1", 30*time.Second) {
		t.Fatal("the login did not survive the swap to the person's window")
	}

	returned, err := browsers.ReturnWheel(ctx, wheel.Session)
	if err != nil {
		t.Fatalf("return the wheel: %v", err)
	}
	// Spec §5.3a: the navigation is recorded while the person drives, and it is what the núcleo
	// offers to grant. A chain that came back empty would mean a login nobody could keep.
	if !containsPrefix(returned.Chain, s.origin()) {
		t.Fatalf("the chain does not name where the person went: %v", returned.Chain)
	}

	// And the agent's next headless session still has it. The cycle is closed: the login the person
	// made is what the run that comes after them will find already done.
	after := openAt(t, browsers, placement, s.origin()+"/whoami")
	if after.Refusal != nil {
		t.Fatalf("the session after the handover was refused: %+v", after.Refusal)
	}
	if !s.reached("WHOAMI 1", 30*time.Second) {
		t.Fatal("the cookie was lost between the person's window and the agent's next one")
	}
}

// TestThePersonsWindowIsNotFenced — spec §6.4, and it is a REQUIREMENT rather than a relaxation.
//
// The wheel is handed over so a person can do what the agent may not: submit a login form. If the
// headful browser inherited the fence, the POST would be refused and the handover would be an empty
// gesture — a window opened for a person who cannot use it.
//
// The control is the profile group's own tests: the same site, submitted by the agent, is refused
// there. Here it must go through.
func TestThePersonsWindowIsNotFenced(t *testing.T) {
	s := newSite(t)
	browsers, _ := pooled(t, s)
	placement := projectAt("acme")

	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	wheel, err := browsers.TakeWheel(ctx, browser.WheelRequest{
		URL:       s.origin() + "/form",
		Placement: placement,
	})
	if err != nil {
		t.Fatalf("take the wheel: %v", err)
	}
	t.Cleanup(func() {
		stopping, stop := context.WithTimeout(context.Background(), 60*time.Second)
		defer stop()
		_, _ = browsers.ReturnWheel(stopping, wheel.Session)
	})

	if !s.reached("GET /form", 30*time.Second) {
		t.Fatal("the person's window never loaded the page")
	}
}

// TestAnAgentIsRefusedTheProfileAPersonIsDriving — spec §4.1, against a real pair of processes.
//
// One browser per profile and no hidden third state. The unit test proves the pool refuses; this
// proves the refusal is what stands between the two, rather than a second Chromium quietly
// inheriting the first over the same --user-data-dir (launch.ErrInheritedInstance).
func TestAnAgentIsRefusedTheProfileAPersonIsDriving(t *testing.T) {
	s := newSite(t)
	browsers, _ := pooled(t, s)
	placement := projectAt("acme")

	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	wheel, err := browsers.TakeWheel(ctx, browser.WheelRequest{
		URL:       s.origin() + "/",
		Placement: placement,
	})
	if err != nil {
		t.Fatalf("take the wheel: %v", err)
	}

	if _, err := browsers.Open(ctx, browser.OpenRequest{
		URL:       s.origin() + "/whoami",
		Placement: placement,
	}); err == nil {
		t.Fatal("an agent opened a session in the profile a person was driving")
	}

	// And the profile comes back the moment the wheel does, without a restart.
	if _, err := browsers.ReturnWheel(ctx, wheel.Session); err != nil {
		t.Fatalf("return the wheel: %v", err)
	}
	openAt(t, browsers, placement, s.origin()+"/whoami")
}

func containsPrefix(chain []string, prefix string) bool {
	for _, step := range chain {
		if strings.HasPrefix(step, prefix) {
			return true
		}
	}
	return false
}
