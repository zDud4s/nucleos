package launch

import (
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"

	"nucleosbrowser/browser"
)

func agentOptions() Options {
	return Options{
		ExecutablePath: `C:\chromium\chrome.exe`,
		ProfileDir:     `C:\profiles\project-1`,
		Mode:           browser.ModeAgent,
		ProxyAddr:      "127.0.0.1:9999",
	}
}

func has(args []string, prefix string) bool {
	return slices.ContainsFunc(args, func(a string) bool { return strings.HasPrefix(a, prefix) })
}

// TestAgentModeWithoutAProxyIsRefused is the argv-level form of spec §6.2a. The spike measured that
// a WebSocket handshake is invisible to both Network.setBlockedURLs and Fetch, so the proxy is the
// only thing that sees it — an agent command line without one has a hole in the fence.
func TestAgentModeWithoutAProxyIsRefused(t *testing.T) {
	opts := agentOptions()
	opts.ProxyAddr = ""
	if _, err := Args(opts); !errors.Is(err, ErrNoProxy) {
		t.Fatalf("got %v, want ErrNoProxy", err)
	}
}

// TestTheFenceIsTheDiff reads the two modes side by side. Everything in this list is a measured
// mechanism, not a preference.
func TestTheFenceIsTheDiff(t *testing.T) {
	agent, err := Args(agentOptions())
	if err != nil {
		t.Fatalf("agent args: %v", err)
	}
	human := agentOptions()
	human.Mode = browser.ModeHuman
	humanArgs, err := Args(human)
	if err != nil {
		t.Fatalf("human args: %v", err)
	}

	fenceOnly := []string{
		"--headless=new",           // spec §4.1: no third rendering state
		"--proxy-server=",          // the only mechanism that sees a WebSocket handshake
		"--proxy-bypass-list=",     // without it loopback walks past the fence
		"--block-new-web-contents", // spec §5.4, measured: window.open returns null
	}
	for _, flag := range fenceOnly {
		if !has(agent, flag) {
			t.Errorf("agent mode is missing %s", flag)
		}
		if has(humanArgs, flag) {
			t.Errorf("human mode should not carry %s: the person is the one deciding", flag)
		}
	}
}

// TestHumanModeIsNotHeadless. Spec §4.1 forbids a hidden rendering state in both directions: agent
// mode must not be visible, and human mode must not be invisible.
func TestHumanModeIsNotHeadless(t *testing.T) {
	opts := agentOptions()
	opts.Mode = browser.ModeHuman
	args, err := Args(opts)
	if err != nil {
		t.Fatalf("args: %v", err)
	}
	if has(args, "--headless") {
		t.Fatal("human mode is headless: there would be nobody able to see the window they are driving")
	}
}

// TestBothModesCarryTheProfileTheNucleoChose. The profile is never the agent's choice (spec §5.3).
func TestBothModesCarryTheProfileTheNucleoChose(t *testing.T) {
	for _, mode := range []browser.Mode{browser.ModeAgent, browser.ModeHuman} {
		opts := agentOptions()
		opts.Mode = mode
		args, err := Args(opts)
		if err != nil {
			t.Fatalf("%s: %v", mode, err)
		}
		if !slices.Contains(args, `--user-data-dir=C:\profiles\project-1`) {
			t.Errorf("%s: profile dir missing from %v", mode, args)
		}
	}
}

// TestTheCacheIsCappedInBothModes — spec §5.6 and §8's cache_size_mb. Both modes, because the
// person's headful session is the half of a profile's life that visits a video site, and a cap that
// only applied while the agent drove would be a cap on the quiet half.
func TestTheCacheIsCappedInBothModes(t *testing.T) {
	for _, mode := range []browser.Mode{browser.ModeAgent, browser.ModeHuman} {
		opts := agentOptions()
		opts.Mode = mode
		opts.CacheMB = 7
		args, err := Args(opts)
		if err != nil {
			t.Fatalf("%s: %v", mode, err)
		}
		if !slices.Contains(args, "--disk-cache-size=7340032") {
			t.Errorf("%s: cache cap missing from %v", mode, args)
		}
	}
}

// TestAnUnsetCacheCapTakesTheDefaultRatherThanNone. Zero is what a caller that never heard of the
// setting passes, and reading it as "unlimited" would leave the one thing nothing else bounds — how
// big a SINGLE profile grows — silently uncapped.
func TestAnUnsetCacheCapTakesTheDefaultRatherThanNone(t *testing.T) {
	args, err := Args(agentOptions())
	if err != nil {
		t.Fatalf("args: %v", err)
	}
	want := fmt.Sprintf("--disk-cache-size=%d", int64(DefaultCacheMB)<<20)
	if !slices.Contains(args, want) {
		t.Errorf("want %s in %v", want, args)
	}
}

func TestMissingExecutableOrProfileIsRefused(t *testing.T) {
	empty := Options{Mode: browser.ModeHuman}
	if _, err := Args(empty); err == nil {
		t.Fatal("built a command line with no executable")
	}
	noProfile := Options{ExecutablePath: "chrome", Mode: browser.ModeHuman}
	if _, err := Args(noProfile); err == nil {
		t.Fatal("built a command line with no profile directory")
	}
}

// TestTheDebuggingPortIsChosenByTheOS. A fixed port collides the moment two sessions run at once,
// and the real port is read back from DevToolsActivePort.
func TestTheDebuggingPortIsChosenByTheOS(t *testing.T) {
	args, err := Args(agentOptions())
	if err != nil {
		t.Fatalf("args: %v", err)
	}
	if !slices.Contains(args, "--remote-debugging-port=0") {
		t.Fatalf("expected port 0, got %v", args)
	}
}

// TestCrashReporterFlagsArePresentAndHonestlyLabelled.
//
// The spike measured that these do NOT remove the crashpad-handler, which keeps running with a
// Google reporting url. They are passed anyway, and the constant below exists so nobody reads the
// flags and concludes the channel is shut.
func TestCrashReporterFlagsArePresentAndHonestlyLabelled(t *testing.T) {
	args, err := Args(agentOptions())
	if err != nil {
		t.Fatalf("args: %v", err)
	}
	for _, flag := range []string{"--disable-crash-reporter", "--disable-breakpad", "--no-report-upload"} {
		if !slices.Contains(args, flag) {
			t.Errorf("missing %s", flag)
		}
	}
}

// TestWebRTCIsDocumentedAsOpen. No flag closes it (spec §6.2b); this asserts nobody quietly added
// one and believed it worked, because the spike tried four and none did.
func TestWebRTCIsDocumentedAsOpen(t *testing.T) {
	args, err := Args(agentOptions())
	if err != nil {
		t.Fatalf("args: %v", err)
	}
	for _, wishful := range []string{
		"--disable-webrtc",
		"--disable-features=WebRtc",
		"--disable-blink-features=RTCPeerConnection",
		"--force-webrtc-ip-handling-policy",
	} {
		if has(args, wishful) {
			t.Errorf(
				"%s is in the command line, but the spike measured that it does not remove WebRTC. %s",
				wishful, WebRTCIsNotFencedHere,
			)
		}
	}
}
