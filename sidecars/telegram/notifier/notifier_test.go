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
