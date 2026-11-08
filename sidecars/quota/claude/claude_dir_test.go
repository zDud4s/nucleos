package claude

import (
	"context"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

// A reader built over an account's own config dir reads that dir's credential — not ~/.claude — and
// its message never repeats the path.
func TestNewForDirReadsThatDirsCredential(t *testing.T) {
	now := time.Now()
	dir := filepath.Join(t.TempDir(), "acct-work")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	expired := `{"claudeAiOauth":{"accessToken":"tok","expiresAt":` +
		strconv.FormatInt(now.Add(-time.Hour).UnixMilli(), 10) + `}}`
	if err := os.WriteFile(filepath.Join(dir, ".credentials.json"), []byte(expired), 0o600); err != nil {
		t.Fatal(err)
	}

	got := NewForDir(time.Second, dir).Read(context.Background(), now)

	if !strings.Contains(got.Detail, "expired") {
		t.Errorf("detail = %q, want the expired-token message (the file in dir was read)", got.Detail)
	}
	if strings.Contains(got.Detail, "acct-work") || strings.Contains(got.Detail, "tok") {
		t.Errorf("detail leaks path or token: %q", got.Detail)
	}

	missing := NewForDir(time.Second, filepath.Join(t.TempDir(), "none")).Read(context.Background(), now)
	if !strings.Contains(missing.Detail, "has not signed in") {
		t.Errorf("missing-dir detail = %q", missing.Detail)
	}
}
