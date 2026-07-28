package notifier

import "testing"

func TestStateNewProposals(t *testing.T) {
	state := NewState()

	first := state.NewProposals([]map[string]any{{"id": float64(1)}, {"id": float64(2)}})
	if len(first) != 2 {
		t.Fatalf("len(first) = %d, want 2", len(first))
	}

	second := state.NewProposals([]map[string]any{{"id": float64(1)}, {"id": float64(2)}, {"id": float64(3)}})
	if len(second) != 1 {
		t.Fatalf("len(second) = %d, want 1", len(second))
	}
	if second[0]["id"] != float64(3) {
		t.Errorf("second[0].id = %#v, want 3", second[0]["id"])
	}
}

func TestStateNewFeedItems(t *testing.T) {
	state := NewState()

	first := state.NewFeedItems([]map[string]any{{"id": float64(10)}})
	if len(first) != 1 {
		t.Fatalf("len(first) = %d, want 1", len(first))
	}

	second := state.NewFeedItems([]map[string]any{{"id": float64(10)}, {"id": float64(11)}})
	if len(second) != 1 {
		t.Fatalf("len(second) = %d, want 1", len(second))
	}
	if second[0]["id"] != float64(11) {
		t.Errorf("second[0].id = %#v, want 11", second[0]["id"])
	}
}

// The seen sets used to grow for the life of the process: every id ever observed stayed, including
// the proposals the daemon decided months ago. Rebuilding them from what the daemon still reports
// keeps them the size of the answer — and a decided proposal never returns to be announced twice.
func TestSeenIdsShrinkBackAsItemsLeaveTheDaemonsLists(t *testing.T) {
	state := NewState()

	for id := 1; id <= 100; id++ {
		state.NewProposals([]map[string]any{{"id": float64(id)}})
		state.NewFeedItems([]map[string]any{{"id": float64(id)}})
	}

	if len(state.seenProposals) != 1 {
		t.Errorf("remembered proposals = %d, want only the one still pending", len(state.seenProposals))
	}
	if len(state.seenFeed) != 1 {
		t.Errorf("remembered feed items = %d, want only the one still in the feed", len(state.seenFeed))
	}
}

// An announcement that failed to send has still been marked as announced, so it would never be
// tried again — the proposal simply never reaches the person who has to decide on it.
func TestAnUndeliveredAnnouncementCanBeRetried(t *testing.T) {
	state := NewState()
	pending := []map[string]any{{"id": float64(4)}}

	if len(state.NewProposals(pending)) != 1 {
		t.Fatal("the first look must report the proposal as new")
	}
	if len(state.NewProposals(pending)) != 0 {
		t.Fatal("the second look must not report it again")
	}

	state.Forget(4)
	if len(state.NewProposals(pending)) != 1 {
		t.Error("a forgotten proposal must be reported again so the send can be retried")
	}
}

func TestAnUndeliveredKillAlertCanBeRetried(t *testing.T) {
	state := NewState()
	state.KillChanged(false)

	if !state.KillChanged(true) {
		t.Fatal("KillChanged(true) after false = false, want the change reported")
	}
	state.ForgetKill(true)
	if !state.KillChanged(true) {
		t.Error("KillChanged(true) after ForgetKill(true) = false, want the alert offered again")
	}
}

func TestStateKillChanged(t *testing.T) {
	state := NewState()

	if state.KillChanged(false) {
		t.Error("KillChanged(false) = true on first observation, want false")
	}
	if !state.KillChanged(true) {
		t.Error("KillChanged(true) = false after false, want true")
	}
	if state.KillChanged(true) {
		t.Error("KillChanged(true) = true without a change, want false")
	}
	if !state.KillChanged(false) {
		t.Error("KillChanged(false) = false after true, want true")
	}
}

func TestStateBudgetChanged(t *testing.T) {
	state := NewState()

	if state.BudgetChanged(map[string]any{"paused": false}) {
		t.Error("BudgetChanged(unpaused) = true on first observation, want false")
	}
	if !state.BudgetChanged(map[string]any{"paused": true, "reason": "cap"}) {
		t.Error("BudgetChanged(paused) = false after unpaused, want true")
	}
	if state.BudgetChanged(map[string]any{"paused": true, "reason": "cap"}) {
		t.Error("BudgetChanged(same paused state) = true, want false")
	}
}
