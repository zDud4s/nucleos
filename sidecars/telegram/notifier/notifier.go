package notifier

import (
	"encoding/json"
	"fmt"
)

type State struct {
	seenProposals map[int64]bool
	seenFeed      map[int64]bool
	lastKill      *bool
	lastBudgetKey *string
}

func NewState() *State {
	return &State{
		seenProposals: map[int64]bool{},
		seenFeed:      map[int64]bool{},
	}
}

func (s *State) NewProposals(current []map[string]any) []map[string]any {
	var items []map[string]any
	items, s.seenProposals = newItems(current, s.seenProposals)
	return items
}

func (s *State) NewFeedItems(current []map[string]any) []map[string]any {
	var items []map[string]any
	items, s.seenFeed = newItems(current, s.seenFeed)
	return items
}

// Forget drops an id from what has been announced, so the next look offers it again. It is how an
// announcement whose send failed gets a second chance instead of being lost: the item was marked as
// told before anyone was actually told.
func (s *State) Forget(id int64) {
	delete(s.seenProposals, id)
	delete(s.seenFeed, id)
}

// ForgetKill re-arms a kill-switch alert that could not be delivered. The next observation is
// compared against the opposite of what went undelivered, so the alert comes back around whichever
// way the switch was moved.
func (s *State) ForgetKill(undelivered bool) {
	opposite := !undelivered
	s.lastKill = &opposite
}

func (s *State) KillChanged(engaged bool) bool {
	if s.lastKill == nil {
		s.lastKill = &engaged
		return engaged
	}

	changed := *s.lastKill != engaged
	*s.lastKill = engaged
	return changed
}

func (s *State) BudgetChanged(budget map[string]any) bool {
	pausedValue := any("")
	if value, ok := budget["paused"]; ok {
		pausedValue = value
	}
	reasonValue := any("")
	if value, ok := budget["reason"]; ok {
		reasonValue = value
	}

	key := fmt.Sprintf("%v|%v", pausedValue, reasonValue)
	if s.lastBudgetKey == nil {
		s.lastBudgetKey = &key
		paused, _ := budget["paused"].(bool)
		return paused
	}

	changed := *s.lastBudgetKey != key
	*s.lastBudgetKey = key
	return changed
}

// newItems returns what is new since the last look, and the set to remember for the next one. The
// set is rebuilt rather than added to: an id the daemon no longer reports is a proposal it has
// decided or a feed entry that scrolled past, and remembering those forever is how this map grew
// without bound in a process meant to run for months.
func newItems(current []map[string]any, seen map[int64]bool) ([]map[string]any, map[int64]bool) {
	items := make([]map[string]any, 0)
	stillPresent := make(map[int64]bool, len(current))
	for _, item := range current {
		id, ok := idOf(item)
		if !ok {
			continue
		}
		if !seen[id] {
			items = append(items, item)
		}
		stillPresent[id] = true
	}
	return items, stillPresent
}

func idOf(m map[string]any) (int64, bool) {
	id, ok := m["id"]
	if !ok {
		return 0, false
	}

	switch value := id.(type) {
	case float64:
		return int64(value), true
	case json.Number:
		parsed, err := value.Int64()
		return parsed, err == nil
	default:
		return 0, false
	}
}
