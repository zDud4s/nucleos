package codex

import (
	"path/filepath"
	"testing"
	"time"
)

// Two accounts, two CODEX_HOME dirs, two different readings: each reader sees only its own.
func TestNewForDirReadsOnlyThatCodexHome(t *testing.T) {
	now := time.Unix(1_800_000_000, 0).UTC()
	homeA, homeB := t.TempDir(), t.TempDir()
	writeRollout(t, homeA, now, `{"rate_limits":{"primary":{"used_percent":10,"window_minutes":300,"resets_at":1800003600}}}`)
	writeRollout(t, homeB, now, `{"rate_limits":{"primary":{"used_percent":70,"window_minutes":300,"resets_at":1800003600}}}`)

	a := NewForDir(filepath.Join(homeA, ".codex")).Read(now)
	b := NewForDir(filepath.Join(homeB, ".codex")).Read(now)

	if len(a.Windows) != 1 || a.Windows[0].UsedFraction != 0.10 {
		t.Errorf("account A = %+v, want 0.10", a.Windows)
	}
	if len(b.Windows) != 1 || b.Windows[0].UsedFraction != 0.70 {
		t.Errorf("account B = %+v, want 0.70", b.Windows)
	}
}
