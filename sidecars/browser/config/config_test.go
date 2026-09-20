// §spec pilar-de-browser

package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRequireLoopback(t *testing.T) {
	cases := []struct {
		addr string
		ok   bool
	}{
		{"127.0.0.1:8795", true},
		{"localhost:8795", true},
		{"[::1]:8795", true},
		{"0.0.0.0:8795", false},
		{"192.168.1.135:8795", false},
		{"example.com:8795", false},
		{"127.0.0.1", false}, // no port
	}
	for _, tc := range cases {
		t.Run(tc.addr, func(t *testing.T) {
			err := requireLoopback(tc.addr)
			if tc.ok && err != nil {
				t.Fatalf("expected %q to be allowed, got %v", tc.addr, err)
			}
			if !tc.ok && err == nil {
				t.Fatalf("expected %q to be refused", tc.addr)
			}
		})
	}
}

// TestLoadRequiresAToken. This sidecar drives logged-in profiles; starting it with no token would
// leave those reachable by any local process that guessed the port.
func TestLoadRequiresAToken(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "")
	if _, err := Load(); err == nil {
		t.Fatal("Load succeeded with no token")
	}
}

func TestLoadDefaults(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	cfg, err := Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Addr != DefaultAddr {
		t.Errorf("addr: got %q, want %q", cfg.Addr, DefaultAddr)
	}
	if cfg.DaemonURL != "http://127.0.0.1:8791" {
		t.Errorf("daemon url: got %q", cfg.DaemonURL)
	}
	if cfg.MaxSessions < 1 {
		t.Errorf("max sessions must leave room to browse, got %d", cfg.MaxSessions)
	}
	if cfg.OpenTimeout <= 0 {
		t.Errorf("open timeout: got %v", cfg.OpenTimeout)
	}
}

func TestLoadRefusesANonLoopbackAddr(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("BROWSER_ADDR", "0.0.0.0:8795")
	_, err := Load()
	if err == nil {
		t.Fatal("Load accepted a public listener")
	}
	if !strings.Contains(err.Error(), "loopback") {
		t.Fatalf("the error should say why: %v", err)
	}
}

func TestLoadRejectsNonsenseNumbers(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("BROWSER_MAX_SESSIONS", "0")
	if _, err := Load(); err == nil {
		t.Fatal("zero sessions should be refused, not silently clamped")
	}
}

// TestTheCeilingsDefaultToTheOnesTheSpecShips. A sidecar started with nothing but a token must
// behave the way the documented `.ai/browser.yaml` says it does — otherwise the file people read is
// not the configuration people run.
func TestTheCeilingsDefaultToTheOnesTheSpecShips(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	cfg, err := Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.CacheMB != 100 || cfg.MaxProfiles != 20 || cfg.DiskBudgetMB != 3000 {
		t.Errorf("ceilings: cache %d, profiles %d, disk %d", cfg.CacheMB, cfg.MaxProfiles, cfg.DiskBudgetMB)
	}
	if cfg.Root == "" {
		t.Error("no install root, so the profiles would land wherever the daemon was started from")
	}
	if cfg.ExecutablePath != "" {
		t.Errorf("an executable override appeared from nowhere: %q", cfg.ExecutablePath)
	}
}

// TestACeilingOfZeroIsRefused. profile.Limits reads zero as "no ceiling", so a config that let a
// zero through would turn a typo into an unbounded disk — the one failure spec §8 says this project
// does not commit anywhere else.
func TestACeilingOfZeroIsRefused(t *testing.T) {
	for _, name := range []string{"BROWSER_CACHE_MB", "BROWSER_MAX_PROFILES", "BROWSER_DISK_BUDGET_MB"} {
		t.Run(name, func(t *testing.T) {
			t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
			t.Setenv(name, "0")
			if _, err := Load(); err == nil {
				t.Fatalf("%s=0 was accepted", name)
			}
			t.Setenv(name, "not a number")
			if _, err := Load(); err == nil {
				t.Fatalf("%s=nonsense was accepted", name)
			}
		})
	}
}

// TestTheRootCanBeMoved, because the tests and the gate need somewhere that is not the owner's real
// profile directory — and because a machine with a small system drive is a real thing.
func TestTheRootCanBeMoved(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("BROWSER_ROOT", filepath.Join(t.TempDir(), "elsewhere"))
	cfg, err := Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if filepath.Base(cfg.Root) != "elsewhere" {
		t.Errorf("root: got %q", cfg.Root)
	}
}

// On Windows LOCALAPPDATA is always set, and the root must stay exactly where it has always been.
func TestTheRootLivesUnderLocalAppDataWhenItIsSet(t *testing.T) {
	local := t.TempDir()
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("BROWSER_ROOT", "")
	t.Setenv("LOCALAPPDATA", local)

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if want := filepath.Join(local, "NucleOS", "browser"); cfg.Root != want {
		t.Errorf("root: got %q, want %q", cfg.Root, want)
	}
}

// Without LOCALAPPDATA (every macOS and Linux machine) the root is the user CACHE directory, moved
// into a temp dir here (XDG_CACHE_HOME on Linux, HOME on macOS). On Windows the standard library
// reads the cache directory from LocalAppData itself, so there it has none and the named relative
// fallback is the answer.
func TestWithoutLocalAppDataTheRootIsInTheUserCacheDir(t *testing.T) {
	base := t.TempDir()
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "tok")
	t.Setenv("BROWSER_ROOT", "")
	t.Setenv("LOCALAPPDATA", "")
	t.Setenv("XDG_CACHE_HOME", base)
	t.Setenv("HOME", base)

	want := "nucleos-browser-root"
	if cache, err := os.UserCacheDir(); err == nil {
		want = filepath.Join(cache, "nucleos", "browser")
	}
	cfg, err := Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Root != want {
		t.Errorf("root: got %q, want %q", cfg.Root, want)
	}
}
