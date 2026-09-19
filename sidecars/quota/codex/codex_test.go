package codex

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"nucleosquota/reading"
)

// The trap this whole package exists around: Codex reports a PERCENTAGE in a float field, and the
// Anthropic endpoint reports a FRACTION in a field of the same type. A reader that forwards either
// one untouched draws a ring a hundred times too full or a hundredth as full, and both look
// plausible enough to ship.
func a_percentage_becomes_a_fraction(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now, `{"rate_limits":{"primary":{"used_percent":6.0,"window_minutes":300,"resets_at":1800003600},"secondary":{"used_percent":84.0,"window_minutes":10080,"resets_at":1800600000},"plan_type":"plus"}}`)

	got := New(dir).Read(now)

	if got.Fidelity != reading.Derived {
		t.Fatalf("fidelity = %q, want %q", got.Fidelity, reading.Derived)
	}
	if len(got.Windows) != 2 {
		t.Fatalf("windows = %d, want 2", len(got.Windows))
	}
	if got.Windows[0].UsedFraction != 0.06 {
		t.Errorf("5h used = %v, want 0.06 — a percentage was forwarded as a fraction", got.Windows[0].UsedFraction)
	}
	if got.Windows[1].UsedFraction != 0.84 {
		t.Errorf("7d used = %v, want 0.84", got.Windows[1].UsedFraction)
	}
	if got.Windows[0].Name != "5h" || got.Windows[1].Name != "7d" {
		t.Errorf("windows named %q/%q, want 5h/7d", got.Windows[0].Name, got.Windows[1].Name)
	}
}

// resets_at is epoch SECONDS here and an ISO string at the other provider. Read as milliseconds it
// lands in 1970 and every window is permanently stale; read as anything else it is in the far
// future and no window is ever stale.
func the_reset_is_read_as_epoch_seconds(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now, `{"rate_limits":{"primary":{"used_percent":1,"window_minutes":300,"resets_at":1800003600}}}`)

	got := New(dir).Read(now)

	if len(got.Windows) != 1 || got.Windows[0].ResetsAt == nil {
		t.Fatalf("expected one window carrying a reset, got %+v", got.Windows)
	}
	if want := time.Unix(1800003600, 0).UTC(); !got.Windows[0].ResetsAt.Equal(want) {
		t.Errorf("reset = %v, want %v", got.Windows[0].ResetsAt, want)
	}
	if got.Windows[0].Stale {
		t.Error("a window resetting an hour from now is not stale")
	}
}

// A reset already in the past means the reading describes a window that has since rolled over. It
// is reported, marked, and never silently refreshed into looking current.
func a_reset_in_the_past_marks_the_window_stale(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now, `{"rate_limits":{"primary":{"used_percent":50,"window_minutes":300,"resets_at":1799000000}}}`)

	got := New(dir).Read(now)

	if len(got.Windows) != 1 || !got.Windows[0].Stale {
		t.Fatalf("expected the window marked stale, got %+v", got.Windows)
	}
}

// The rollout format is an undocumented de facto contract (design R5). When it changes, the answer
// must fall to Unmeasured — which draws a dashed ring and, later, never brakes — rather than to a
// number nobody can justify.
func an_unknown_window_is_skipped_rather_than_guessed(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now, `{"rate_limits":{"primary":{"used_percent":9,"window_minutes":301,"resets_at":1800003600}}}`)

	got := New(dir).Read(now)

	if got.Fidelity != reading.Unmeasured {
		t.Fatalf("fidelity = %q, want %q for a window this design cannot name", got.Fidelity, reading.Unmeasured)
	}
	if len(got.Windows) != 0 {
		t.Errorf("an unmeasured reading must carry no windows, got %+v", got.Windows)
	}
}

// The last reading in the file is the freshest, and it is the one that counts.
func the_last_reading_in_the_transcript_wins(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now,
		`{"rate_limits":{"primary":{"used_percent":1,"window_minutes":300,"resets_at":1800003600}}}`,
		`{"unrelated":"line"}`,
		`{"rate_limits":{"primary":{"used_percent":77,"window_minutes":300,"resets_at":1800003600}}}`,
	)

	got := New(dir).Read(now)

	if len(got.Windows) != 1 || got.Windows[0].UsedFraction != 0.77 {
		t.Fatalf("expected the final reading (0.77), got %+v", got.Windows)
	}
}

// rate_limits has moved inside the envelope before, so it is looked for by key at any depth.
func the_reading_is_found_however_deeply_it_is_nested(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now, `{"type":"token_count","payload":{"info":{"rate_limits":{"primary":{"used_percent":12,"window_minutes":300,"resets_at":1800003600}}}}}`)

	got := New(dir).Read(now)

	if len(got.Windows) != 1 || got.Windows[0].UsedFraction != 0.12 {
		t.Fatalf("expected the nested reading (0.12), got %+v", got.Windows)
	}
}

// No Codex on this machine is a fact about the machine, not a failure of this process.
func an_absent_sessions_directory_is_unmeasured_not_an_error(t *testing.T) {
	got := New(filepath.Join(t.TempDir(), "nothing-here")).Read(time.Now())

	if got.Fidelity != reading.Unmeasured {
		t.Fatalf("fidelity = %q, want %q", got.Fidelity, reading.Unmeasured)
	}
	if got.Detail == "" {
		t.Error("an unmeasured reading must say why")
	}
}

func TestCodex(t *testing.T) {
	t.Run("a percentage becomes a fraction", a_percentage_becomes_a_fraction)
	t.Run("the reset is read as epoch seconds", the_reset_is_read_as_epoch_seconds)
	t.Run("a reset in the past marks the window stale", a_reset_in_the_past_marks_the_window_stale)
	t.Run("an unknown window is skipped rather than guessed", an_unknown_window_is_skipped_rather_than_guessed)
	t.Run("the last reading in the transcript wins", the_last_reading_in_the_transcript_wins)
	t.Run("the reading is found however deeply it is nested", the_reading_is_found_however_deeply_it_is_nested)
	t.Run("an absent sessions directory is unmeasured, not an error", an_absent_sessions_directory_is_unmeasured_not_an_error)
}

// writeRollout lays a transcript down where the reader's date walk will find it.
func writeRollout(t *testing.T, home string, day time.Time, lines ...string) {
	t.Helper()
	dir := filepath.Join(home, ".codex", "sessions", day.Format("2006"), day.Format("01"), day.Format("02"))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	body := ""
	for _, l := range lines {
		body += l + "\n"
	}
	if err := os.WriteFile(filepath.Join(dir, "rollout-test.jsonl"), []byte(body), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
}
