// §spec browser-volante

package pool

import (
	"context"
	"errors"
	"slices"
	"testing"

	"nucleosbrowser/browser"
)

// TestPoolBeginPersonRefusesASecondSessionInTheBrowser. The fence is browser-wide, so a person can
// only take a browser that holds exactly the one session being handed over.
func TestPoolBeginPersonRefusesASecondSessionInTheBrowser(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)
	placement := project("acme", "https://jira.example.org")
	first := mustOpen(t, pool, placement)
	mustOpen(t, pool, placement)

	err := pool.BeginPerson(context.Background(), first.ID)
	if !errors.Is(err, browser.ErrNotSoleSession) {
		t.Fatalf("BeginPerson with a second session = %v, want ErrNotSoleSession", err)
	}

	// The refusal must not leave the profile marked: a third open still finds the browser.
	if _, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: placement,
	}); err != nil {
		t.Fatalf("open after a refused BeginPerson: %v", err)
	}
}

// TestPoolBeginPersonRefusesAnAgentOpenOnThatProfile. While a person drives, there is nowhere for a
// second agent session on that profile to go.
func TestPoolBeginPersonRefusesAnAgentOpenOnThatProfile(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)

	if err := pool.BeginPerson(context.Background(), session.ID); err != nil {
		t.Fatalf("BeginPerson: %v", err)
	}
	_, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: placement,
	})
	if !errors.Is(err, browser.ErrPersonIsDriving) {
		t.Fatalf("open on a profile a person drives = %v, want ErrPersonIsDriving", err)
	}
}

// TestPoolEndPersonReturnsTheChainAndLetsTheAgentOpenAgain.
func TestPoolEndPersonReturnsTheChainAndLetsTheAgentOpenAgain(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)
	launcher.launched()[0].Fake.Chain = []string{"https://jira.example.org/", "https://accounts.google.com/"}

	// Ending a person nobody began is refused, and names why.
	if _, err := pool.EndPerson(context.Background(), session.ID); !errors.Is(err, browser.ErrNotPerson) {
		t.Fatalf("EndPerson before BeginPerson = %v, want ErrNotPerson", err)
	}

	if err := pool.BeginPerson(context.Background(), session.ID); err != nil {
		t.Fatalf("BeginPerson: %v", err)
	}
	returned, err := pool.EndPerson(context.Background(), session.ID)
	if err != nil {
		t.Fatalf("EndPerson: %v", err)
	}
	want := []string{"https://jira.example.org/", "https://accounts.google.com/"}
	if !slices.Equal(returned.Chain, want) {
		t.Errorf("chain = %v, want %v", returned.Chain, want)
	}

	// The mark is gone: the agent may open on the profile again.
	mustOpen(t, pool, placement)
}

// TestPoolClosingThePersonSessionFreesTheProfile. Core closes the session when the person gives up,
// and a profile that stayed marked would refuse every later open.
func TestPoolClosingThePersonSessionFreesTheProfile(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)

	if err := pool.BeginPerson(context.Background(), session.ID); err != nil {
		t.Fatalf("BeginPerson: %v", err)
	}
	if err := pool.Close(context.Background(), session.ID); err != nil {
		t.Fatalf("close: %v", err)
	}
	if _, err := pool.Open(context.Background(), browser.OpenRequest{
		URL:       "https://example.org/",
		Placement: placement,
	}); err != nil {
		t.Fatalf("open after closing the person's session = %v, want success", err)
	}
}

// TestPoolInputReachesTheSessionsBrowser. The pool does not interpret an input batch: it finds the
// browser the session lives in and hands it over, and says so when the session is unknown.
func TestPoolInputReachesTheSessionsBrowser(t *testing.T) {
	launcher := &fakeLauncher{}
	pool, _ := testPool(t, launcher, 4)
	placement := project("acme", "https://jira.example.org")
	session := mustOpen(t, pool, placement)
	events := []browser.InputEvent{
		{Kind: "mouse", Type: "mouseMoved", X: 3, Y: 4},
		{Kind: "text", Value: "hi"},
	}

	if err := pool.Input(context.Background(), session.ID, events); !errors.Is(err, browser.ErrNotPerson) {
		t.Fatalf("Input before BeginPerson = %v, want ErrNotPerson", err)
	}
	if err := pool.BeginPerson(context.Background(), session.ID); err != nil {
		t.Fatalf("BeginPerson: %v", err)
	}
	if err := pool.Input(context.Background(), session.ID, events); err != nil {
		t.Fatalf("Input: %v", err)
	}
	seen := launcher.launched()[0].Fake.Inputs
	if len(seen) != 1 || !slices.Equal(seen[0], events) {
		t.Errorf("the browser saw %v, want the one batch %v", seen, events)
	}
	if err := pool.Input(context.Background(), "nope", events); !errors.Is(err, browser.ErrNoSuchSession) {
		t.Errorf("Input for an unknown session = %v, want ErrNoSuchSession", err)
	}
}
