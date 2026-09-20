package codex

import (
	"os"
	"path/filepath"
	"strings"
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

// F2: a `rate_limits` object that parses but carries neither bucket (the vendor's own empty
// exhausted-quota snapshot) must not clobber the last usable reading in the same transcript — the
// same rule usage.py applies by skipping such a snapshot outright.
func an_empty_rate_limits_snapshot_does_not_clobber_the_last_good_reading(t *testing.T) {
	for _, empty := range []string{`{}`, `null`} {
		dir := t.TempDir()
		now := time.Unix(1_800_000_000, 0).UTC()
		writeRollout(t, dir, now,
			`{"rate_limits":{"primary":{"used_percent":42,"window_minutes":300,"resets_at":1800003600}}}`,
			`{"rate_limits":`+empty+`}`,
		)

		got := New(dir).Read(now)

		if got.Fidelity != reading.Derived {
			t.Fatalf("rate_limits=%s: fidelity = %q, want %q", empty, got.Fidelity, reading.Derived)
		}
		if len(got.Windows) != 1 || got.Windows[0].UsedFraction != 0.42 {
			t.Fatalf("rate_limits=%s: expected the earlier 0.42 reading preserved, got %+v", empty, got.Windows)
		}
	}
}

// A session started on an earlier day but still being appended has a newer mtime than a session
// that was started and abandoned this morning: day-major order would visit today's directory first
// and report the abandoned file's reading. Ordering candidates by mtime across days is what this
// test pins down; under the old day-major walk today's single (older-mtime) file would already have
// been treated as the answer, without ever looking at yesterday's directory.
func a_newer_mtime_file_in_an_older_day_wins_over_an_older_mtime_file_in_todays_dir(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	yesterday := now.AddDate(0, 0, -1)

	yesterdayDir := filepath.Join(dir, ".codex", "sessions", yesterday.Format("2006"), yesterday.Format("01"), yesterday.Format("02"))
	if err := os.MkdirAll(yesterdayDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	todayDir := filepath.Join(dir, ".codex", "sessions", now.Format("2006"), now.Format("01"), now.Format("02"))
	if err := os.MkdirAll(todayDir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	// Started yesterday, still being appended: its mtime is the newest of the two files.
	writeNamedRollout(t, yesterdayDir, "live.jsonl", now,
		`{"rate_limits":{"primary":{"used_percent":55,"window_minutes":300,"resets_at":1800003600}}}`)
	// Started and abandoned this morning: sits in today's directory, but was last written an hour
	// before the still-live session above.
	writeNamedRollout(t, todayDir, "abandoned.jsonl", now.Add(-time.Hour),
		`{"rate_limits":{"primary":{"used_percent":11,"window_minutes":300,"resets_at":1800003600}}}`)

	got := New(dir).Read(now)

	if len(got.Windows) != 1 || got.Windows[0].UsedFraction != 0.55 {
		t.Fatalf("expected the newer-mtime reading from yesterday's dir (0.55), got %+v", got.Windows)
	}
}

// F3: a session that has only just started writes session_meta and nothing else. The newest file
// carrying no reading must not shadow an older file that has one.
func an_older_file_wins_when_the_newest_file_has_no_reading_yet(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	daydir := filepath.Join(dir, ".codex", "sessions", now.Format("2006"), now.Format("01"), now.Format("02"))
	if err := os.MkdirAll(daydir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	writeNamedRollout(t, daydir, "a-older.jsonl", now.Add(-time.Hour),
		`{"rate_limits":{"primary":{"used_percent":33,"window_minutes":300,"resets_at":1800003600}}}`)
	writeNamedRollout(t, daydir, "b-newer.jsonl", now,
		`{"type":"session_meta"}`)

	got := New(dir).Read(now)

	if got.Fidelity != reading.Derived {
		t.Fatalf("fidelity = %q, want %q — the older file's reading should have been used", got.Fidelity, reading.Derived)
	}
	if len(got.Windows) != 1 || got.Windows[0].UsedFraction != 0.33 {
		t.Fatalf("expected the older file's 0.33 reading, got %+v", got.Windows)
	}
}

// newReaderWithShrunkBuffer builds a Reader exactly like New, except with the scan limits shrunk to
// max — so a "scan failed" fixture can be a few hundred bytes instead of the real 8MiB default.
// tailBytes stays untouched: the fixture files here are well under it, so no seek into a giant file
// is needed either. A Reader field, not a package var: nothing shared is mutated, so this is safe
// even if a future test adds t.Parallel().
func newReaderWithShrunkBuffer(home string, max int) *Reader {
	r := New(home)
	r.scanBufferCap, r.maxLineBytes = max, max
	return r
}

// S3: a line longer than the scan buffer must not silently end the scan — it must be treated as an
// unreadable file so the walk falls back to an older candidate, rather than either crashing or
// quietly reporting whatever was found before the oversized line.
func a_line_over_the_scan_buffer_falls_back_to_an_older_file(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	daydir := filepath.Join(dir, ".codex", "sessions", now.Format("2006"), now.Format("01"), now.Format("02"))
	if err := os.MkdirAll(daydir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	// Comfortably under the 150-byte shrunk limit.
	writeNamedRollout(t, daydir, "a-older.jsonl", now.Add(-time.Hour),
		`{"rate_limits":{"primary":{"used_percent":21,"window_minutes":300,"resets_at":1800003600}}}`)
	// Padded well past the 150-byte shrunk limit — this is what makes the file "corrupt" only for
	// the duration of this test.
	writeNamedRollout(t, daydir, "b-newer.jsonl", now,
		`{"rate_limits":{"primary":{"used_percent":99,"window_minutes":300,"resets_at":1800003600},"padding":"`+strings.Repeat("x", 200)+`"}}`)

	got := newReaderWithShrunkBuffer(dir, 150).Read(now)

	if got.Fidelity != reading.Derived {
		t.Fatalf("fidelity = %q, want %q — the oversized file should have been skipped, not fatal", got.Fidelity, reading.Derived)
	}
	if len(got.Windows) != 1 || got.Windows[0].UsedFraction != 0.21 {
		t.Fatalf("expected the older file's 0.21 reading, got %+v", got.Windows)
	}
}

// lastLimits itself must report a scan failure as an error rather than as success with the last
// good reading found before the failure — the distinction the caller relies on to tell "nothing here
// yet" from "this file broke". Without a valid line before the oversized one, both the fixed code
// and the pre-fix code (scanner.Err() unchecked) return an error here, since `found` would be nil
// either way — which is why the original version of this test passed against the very bug it was
// meant to catch. Putting a short VALID reading first closes that gap: the pre-fix code would have
// returned it successfully (found != nil, no err check), while the fix must still error, because the
// scan never reached the true end of the file and a newer line could have superseded it.
func lastLimits_reports_an_oversized_line_as_an_error(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "oversized.jsonl")
	valid := `{"rate_limits":{"primary":{"used_percent":1,"window_minutes":300}}}`
	oversized := `{"rate_limits":{"primary":{"used_percent":99,"window_minutes":300}},"padding":"` + strings.Repeat("z", 100) + `"}`
	body := valid + "\n" + oversized + "\n"
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}

	r := newReaderWithShrunkBuffer(dir, 64)
	_, err := r.lastLimits(path)
	if err == nil {
		t.Fatal("expected an error for a line over the scan buffer, got nil")
	}
	if strings.Contains(err.Error(), "no rate-limit reading yet") {
		t.Errorf("error = %q, reads like an empty file rather than a scan that failed midway", err.Error())
	}
	if !strings.Contains(err.Error(), "could not be fully scanned") {
		t.Errorf("error = %q, want it to identify a scan failure", err.Error())
	}
}

// Some Codex builds spelled a window's reset as an offset from the event rather than an absolute
// instant. It is only usable together with the rollout line's own timestamp.
func the_older_reset_spelling_resolves_relative_to_the_lines_own_timestamp(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now,
		`{"timestamp":"2026-01-01T00:00:00Z","rate_limits":{"primary":{"used_percent":5,"window_minutes":300,"resets_in_seconds":3600}}}`)

	got := New(dir).Read(now)

	want := time.Date(2026, 1, 1, 1, 0, 0, 0, time.UTC)
	if len(got.Windows) != 1 || got.Windows[0].ResetsAt == nil {
		t.Fatalf("expected one window carrying a reset, got %+v", got.Windows)
	}
	if !got.Windows[0].ResetsAt.Equal(want) {
		t.Errorf("reset = %v, want %v (line timestamp + resets_in_seconds)", got.Windows[0].ResetsAt, want)
	}
}

// Without a base timestamp to add the offset to, the older spelling cannot be resolved reliably and
// must be skipped rather than guessed at — consistent with the design's rule that an unmeasured fact
// is reported as absent.
func the_older_reset_spelling_is_skipped_without_a_base_timestamp(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now,
		`{"rate_limits":{"primary":{"used_percent":5,"window_minutes":300,"resets_in_seconds":3600}}}`)

	got := New(dir).Read(now)

	if len(got.Windows) != 1 {
		t.Fatalf("expected one window, got %+v", got.Windows)
	}
	if got.Windows[0].ResetsAt != nil {
		t.Errorf("expected no reset without a base timestamp, got %v", got.Windows[0].ResetsAt)
	}
}

// used_percent in the same struct is already float64, so a build that spells this fact with a
// fraction (17999.5) is plausible; an int64 field would fail to unmarshal the whole limits object
// over it, discarding an otherwise good reading rather than merely losing half a second.
func the_older_reset_spelling_accepts_a_fractional_offset(t *testing.T) {
	dir := t.TempDir()
	now := time.Unix(1_800_000_000, 0).UTC()
	writeRollout(t, dir, now,
		`{"timestamp":"2026-01-01T00:00:00Z","rate_limits":{"primary":{"used_percent":5,"window_minutes":300,"resets_in_seconds":17999.5}}}`)

	got := New(dir).Read(now)

	want := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC).Add(17999*time.Second + 500*time.Millisecond)
	if len(got.Windows) != 1 || got.Windows[0].ResetsAt == nil {
		t.Fatalf("expected one window carrying a reset, got %+v", got.Windows)
	}
	if !got.Windows[0].ResetsAt.Equal(want) {
		t.Errorf("reset = %v, want %v (fractional resets_in_seconds honored)", got.Windows[0].ResetsAt, want)
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
	t.Run("an empty rate_limits snapshot does not clobber the last good reading", an_empty_rate_limits_snapshot_does_not_clobber_the_last_good_reading)
	t.Run("a newer-mtime file in an older day wins over an older-mtime file in today's dir", a_newer_mtime_file_in_an_older_day_wins_over_an_older_mtime_file_in_todays_dir)
	t.Run("an older file wins when the newest file has no reading yet", an_older_file_wins_when_the_newest_file_has_no_reading_yet)
	t.Run("a line over the scan buffer falls back to an older file", a_line_over_the_scan_buffer_falls_back_to_an_older_file)
	t.Run("lastLimits reports an oversized line as an error", lastLimits_reports_an_oversized_line_as_an_error)
	t.Run("the older reset spelling resolves relative to the line's own timestamp", the_older_reset_spelling_resolves_relative_to_the_lines_own_timestamp)
	t.Run("the older reset spelling is skipped without a base timestamp", the_older_reset_spelling_is_skipped_without_a_base_timestamp)
	t.Run("the older reset spelling accepts a fractional offset", the_older_reset_spelling_accepts_a_fractional_offset)
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

// writeNamedRollout writes one file directly into an already-created day directory and pins its
// mtime, so tests can control which of several files in the same day is "newest" without
// depending on filesystem write-time granularity.
func writeNamedRollout(t *testing.T, dir, name string, mtime time.Time, lines ...string) {
	t.Helper()
	body := ""
	for _, l := range lines {
		body += l + "\n"
	}
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
	if err := os.Chtimes(path, mtime, mtime); err != nil {
		t.Fatalf("chtimes: %v", err)
	}
}
