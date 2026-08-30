//go:build browsergate

// §spec pilar-de-browser

package gate_test

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"nucleosbrowser/launch"
)

// The install group: spec §9.5, which is the one step of this pillar that cannot be proven without
// the internet.
//
// `launch.Install.Download` has been unit-tested since step C against a `Fetcher` that returns bytes
// from memory. That proves the verify-then-extract order, the digest refusal and the zip-slip guard —
// everything except the two facts that matter on a fresh machine: that the pinned URL still serves
// something, and that what it serves has the digest this repository claims. Both are statements about
// the world, so both live here.
//
// It is idempotent. `Download` returns immediately when the executable is already present, so running
// the gate twice does not re-fetch 350MB.

func installRoot(t *testing.T) string {
	t.Helper()
	if root := os.Getenv("LOCALAPPDATA"); root != "" {
		return filepath.Join(root, "NucleOS", "browser")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skipf("no install root on this platform: %v", err)
	}
	return filepath.Join(home, ".local", "share", "nucleos", "browser")
}

// TestThePinnedChromiumInstallsAndIsTheBrowserWeClaim.
//
// Three assertions in one motion, because they only mean anything together: the URL still serves, the
// bytes match the pinned digest, and what comes out of the archive is a whole Chromium tree.
//
// The digest is the load-bearing one. Fetching over TLS says who served the archive, not what it is —
// and this archive becomes the process that renders hostile HTML while holding the owner's logged-in
// profiles. A revision bump that skipped this test would install whatever the mirror had.
func TestThePinnedChromiumInstallsAndIsTheBrowserWeClaim(t *testing.T) {
	pin := launch.DefaultPin()
	if pin.URL == "" {
		t.Skipf("no pinned chromium for this platform: %v", launch.ErrNoPinForPlatform)
	}
	if pin.Sha256 == "" {
		t.Fatalf("chromium %s has a URL and no digest; Download refuses that, and so does this",
			pin.Revision)
	}

	install := launch.Install{Root: installRoot(t), Pin: pin}
	alreadyThere := install.Present()

	ctx, cancel := context.WithTimeout(context.Background(), launch.FetchTimeout)
	defer cancel()
	if err := install.Download(ctx, launch.HTTPFetcher{}); err != nil {
		t.Fatalf("downloading chromium %s: %v", pin.Revision, err)
	}
	if !install.Present() {
		t.Fatalf("the download reported success and %s is not there", install.ExecutablePath())
	}
	if !alreadyThere {
		t.Logf("installed chromium %s at %s", pin.Revision, install.Dir())
	}

	// The archive brings a tree and not a file, and a "success" that left only the executable would
	// produce a browser that starts and then cannot find its resources.
	entries, err := os.ReadDir(filepath.Dir(install.ExecutablePath()))
	if err != nil {
		t.Fatalf("reading the installed tree: %v", err)
	}
	if len(entries) < 10 {
		t.Fatalf("the install has %d entries; a Chromium tree has hundreds", len(entries))
	}

	// There is deliberately no `--version` here, and the absence is a finding rather than an
	// omission. On Windows a snapshot build's `chrome.exe --version` does not print and exit: it
	// STARTS THE BROWSER — the first attempt at this assertion opened a window, registered with GCM,
	// and was killed by its own timeout sixty seconds later. What proves the binary runs is the rest
	// of this group, which drives it for real, and `TestTheGateRunsAgainstThePinnedBuild` below is
	// what makes "the rest of this group" mean this binary and not the system's.
}

// TestTheGateRunsAgainstThePinnedBuild is the assertion this whole group exists to make possible.
//
// Until the download had a caller, every test in this package ran against whatever Chrome the machine
// happened to have — so the fence was proven against a browser nobody ships, which is a different
// claim from the one the spec makes. This pins that the rest of the gate is now measuring the pinned
// build, and fails rather than skipping if it is not: a silent fall back to the system browser is
// exactly the thing that made the earlier result weaker than it looked.
func TestTheGateRunsAgainstThePinnedBuild(t *testing.T) {
	if os.Getenv("NUCLEOS_BROWSER_CHROMIUM") != "" {
		t.Skip("NUCLEOS_BROWSER_CHROMIUM overrides the pinned build on purpose")
	}
	install := launch.Install{Root: installRoot(t), Pin: launch.DefaultPin()}
	if !install.Present() {
		t.Skip("the pinned chromium is not installed; the test above installs it")
	}
	if got := chromium(t); got != install.ExecutablePath() {
		t.Fatalf("the gate is driving %s, not the pinned %s", got, install.ExecutablePath())
	}
}
