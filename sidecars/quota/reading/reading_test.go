package reading

import (
	"testing"
	"time"
)

// Unavailable's whole reason to exist is an invariant that is easy to break by hand: Unmeasured
// must never carry a window a caller could mistake for a real reading.
func unavailable_never_carries_windows(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	got := Unavailable("codex", "no usable credential", now)

	if got.Fidelity != Unmeasured {
		t.Errorf("fidelity = %q, want %q", got.Fidelity, Unmeasured)
	}
	if got.Windows != nil {
		t.Errorf("windows = %+v, want nil", got.Windows)
	}
	if got.Detail != "no usable credential" {
		t.Errorf("detail = %q, want the reason passed in", got.Detail)
	}
	if got.ReadAt != now {
		t.Errorf("read_at = %v, want %v", got.ReadAt, now)
	}
}

// MarkStale is the one place both providers agree a past reset makes a reading meaningless,
// whoever reported it. A nil reset is a real answer (some vendor windows never say when they
// reopen) and must not be treated as if it had already passed.
func mark_stale_flags_only_a_reset_already_in_the_past(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	past := now.Add(-time.Hour)
	future := now.Add(time.Hour)

	cases := []struct {
		name      string
		resetsAt  *time.Time
		wantStale bool
	}{
		{"a reset already in the past", &past, true},
		{"a reset still in the future", &future, false},
		{"no reset at all", nil, false},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			windows := MarkStale([]Window{{Name: "5h", ResetsAt: tc.resetsAt}}, now)
			if windows[0].Stale != tc.wantStale {
				t.Errorf("stale = %v, want %v", windows[0].Stale, tc.wantStale)
			}
		})
	}
}

func TestReading(t *testing.T) {
	t.Run("Unavailable never carries windows", unavailable_never_carries_windows)
	t.Run("MarkStale flags only a reset already in the past", mark_stale_flags_only_a_reset_already_in_the_past)
}
