package config

import (
	"os"
	"path/filepath"
	"testing"
)

// The update offset lives on disk so a crash cannot redeliver a batch that was already executed:
// `/kill off` running a second time re-arms an autonomous agent nobody asked to re-arm.
func TestTheUpdateOffsetSurvivesARestart(t *testing.T) {
	path := filepath.Join(t.TempDir(), "telegram-offset")

	if got := LoadOffset(path); got != 0 {
		t.Errorf("LoadOffset(missing) = %d, want 0", got)
	}
	if err := SaveOffset(path, 4242); err != nil {
		t.Fatalf("SaveOffset() error = %v", err)
	}
	if got := LoadOffset(path); got != 4242 {
		t.Errorf("LoadOffset() = %d, want 4242", got)
	}

	if err := os.WriteFile(path, []byte("not a number"), 0o600); err != nil {
		t.Fatalf("write offset: %v", err)
	}
	if got := LoadOffset(path); got != 0 {
		t.Errorf("LoadOffset(corrupt) = %d, want 0 so the bot still starts", got)
	}
}

// withoutLocalAppData makes a machine where LOCALAPPDATA is not set, which is every macOS and Linux
// one. It moves the user config directory into a temp dir on each OS (APPDATA on Windows,
// XDG_CONFIG_HOME on Linux, HOME on macOS) and returns it as the standard library resolves it, so
// no test hard-codes a platform's layout or touches the real one.
func withoutLocalAppData(t *testing.T) string {
	t.Helper()
	base := t.TempDir()
	t.Setenv("LOCALAPPDATA", "")
	t.Setenv("APPDATA", base)
	t.Setenv("XDG_CONFIG_HOME", base)
	t.Setenv("HOME", base)
	dir, err := os.UserConfigDir()
	if err != nil {
		t.Fatalf("os.UserConfigDir() error = %v", err)
	}
	return dir
}

// On Windows LOCALAPPDATA is always set, and the offset must stay exactly where it has always been.
func TestTheOffsetLivesUnderLocalAppDataWhenItIsSet(t *testing.T) {
	local := t.TempDir()
	t.Setenv("NUCLEOS_TELEGRAM_OFFSET", "")
	t.Setenv("LOCALAPPDATA", local)

	want := filepath.Join(local, "nucleos", "telegram-offset")
	if got := OffsetPath(); got != want {
		t.Errorf("OffsetPath() = %q, want %q", got, want)
	}
}

// Without LOCALAPPDATA the offset used to be the RELATIVE path nucleos/telegram-offset, so a sidecar
// started from another directory lost its place and read Telegram's backlog again.
func TestWithoutLocalAppDataTheOffsetLivesInTheUserConfigDir(t *testing.T) {
	dir := withoutLocalAppData(t)
	t.Setenv("NUCLEOS_TELEGRAM_OFFSET", "")

	got := OffsetPath()
	if want := filepath.Join(dir, "nucleos", "telegram-offset"); got != want {
		t.Errorf("OffsetPath() = %q, want %q", got, want)
	}
	if !filepath.IsAbs(got) {
		t.Errorf("OffsetPath() = %q, want an absolute path", got)
	}
}
