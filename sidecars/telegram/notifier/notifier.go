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
	return newItems(current, s.seenProposals)
}

func (s *State) NewFeedItems(current []map[string]any) []map[string]any {
	return newItems(current, s.seenFeed)
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

func newItems(current []map[string]any, seen map[int64]bool) []map[string]any {
	items := make([]map[string]any, 0)
	for _, item := range current {
		id, ok := idOf(item)
		if !ok {
			continue
		}
		if !seen[id] {
			items = append(items, item)
		}
		seen[id] = true
	}
	return items
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
