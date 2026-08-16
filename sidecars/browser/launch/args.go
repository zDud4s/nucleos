// Package launch owns the Chromium on disk: where it lives, how it is fetched, how it is started,
// and how its absence is reported.
//
// # This file is where the spike's findings stop being prose
//
// The spike of 2026-08-15 (`.ai/local/spikes/2026-08-15-pinchtab.md`) measured which containment
// mechanisms actually work in Chrome 151. Three of the spec's original answers were wrong, and the
// corrections live here as flags rather than as paragraphs, because a paragraph cannot fail a test.
//
// The difference between agent mode and human mode IS the fence. Read Args as two lists and the
// security model is the diff between them.
package launch

import (
	"errors"
	"fmt"

	"nucleosbrowser/browser"
)

// ErrNoProxy is the argv-level expression of spec §6.2a — "se o interceptor não estiver atado, o
// separador não navega".
//
// A proxy is not an optimisation here. The spike measured that a WebSocket handshake ignores
// Network.setBlockedURLs entirely and is invisible to Fetch interception; the proxy is the only
// mechanism that sees it. So an agent-mode command line without one is a command line with a hole
// in the fence, and this package refuses to build it rather than returning something that looks
// fine and leaks.
var ErrNoProxy = errors.New("launch: agent mode requires a proxy address, refusing to build an unfenced command line")

// Options is everything the núcleo decides about one launch.
//
// ProfileDir is chosen by the núcleo, never by the agent (spec §5.3, §6.1) — see
// browser.OpenRequest, which has no profile field for the same reason.
type Options struct {
	ExecutablePath string
	ProfileDir     string
	Mode           browser.Mode
	// ProxyAddr is the loopback address of the fence's proxy, "127.0.0.1:port". Required in agent
	// mode; ignored in human mode, where the person is the one deciding what to click.
	ProxyAddr string
	// CacheMB caps this profile's disk cache (spec §5.6, §8's cache_size_mb). Zero takes
	// DefaultCacheMB rather than meaning "unlimited": profiles.Admit bounds how many profiles there
	// are and how much they hold together, and nothing else bounds how much ONE of them grows.
	CacheMB int
}

// DefaultCacheMB matches the value spec §8 ships in `.ai/browser.yaml`.
const DefaultCacheMB = 100

// Args builds the command line, or refuses to.
func Args(opts Options) ([]string, error) {
	if opts.ExecutablePath == "" {
		return nil, errors.New("launch: no chromium executable")
	}
	if opts.ProfileDir == "" {
		return nil, errors.New("launch: no profile directory")
	}

	cacheMB := opts.CacheMB
	if cacheMB <= 0 {
		cacheMB = DefaultCacheMB
	}

	args := []string{
		// Port 0 makes the OS choose; the real port is read back from DevToolsActivePort in the
		// profile directory. A fixed port would collide the moment two sessions run at once.
		"--remote-debugging-port=0",
		"--user-data-dir=" + opts.ProfileDir,
		// In bytes, and applied in BOTH modes. A cap that only held while the agent drove would be a
		// cap on the half of the profile's life that writes least: the person's headful session is
		// the one that visits a video site.
		fmt.Sprintf("--disk-cache-size=%d", int64(cacheMB)<<20),
		"--no-first-run",
		"--no-default-browser-check",
		"--disable-background-networking",
		"--disable-component-update",
		"--disable-search-engine-choice-screen",

		// The crash reporter. SPIKE FINDING, and an honest one: these three do NOT remove the
		// crashpad-handler process, which keeps running with
		// --url=https://clients2.google.com/cr/report under every combination tried. They are
		// passed anyway because they cost nothing and bear on upload consent, but nobody should
		// read this block and believe the channel is closed. Whether anything is ever uploaded was
		// not proven either way; it is an open item in spec §13.
		"--disable-crash-reporter",
		"--disable-breakpad",
		"--no-report-upload",
	}

	if opts.Mode == browser.ModeHuman {
		// A person is at the wheel. No fence, no headless, and deliberately no popup blocking:
		// this is the mode that exists so a human can complete the login or the OAuth popup the
		// agent is not allowed to (spec §5.4).
		return args, nil
	}

	if opts.ProxyAddr == "" {
		return nil, ErrNoProxy
	}

	return append(args,
		// Spec §4.1: there is no third rendering state. Agent mode is headless, full stop — a
		// window rendering where nobody can see it was rejected on both security and VRAM grounds.
		"--headless=new",

		// The fence's choke point. SPIKE FINDING: the proxy sees a WebSocket handshake as
		// `CONNECT host:port` and can refuse it, which is a channel CDP could not see at all.
		"--proxy-server=http://"+opts.ProxyAddr,
		// Loopback is bypassed unless this is set, and the fence has to see loopback too —
		// otherwise a page reaching 127.0.0.1 walks straight past it.
		"--proxy-bypass-list=<-loopback>",

		// Spec §5.4, measured: window.open returns null and no target is created. The cleaner of
		// the two mechanisms — the other one is auto-attach, which works but leaves a target to
		// reason about.
		"--block-new-web-contents",

		// The startup page, and it is here for a measured reason rather than for tidiness. Without
		// it Chrome opens the New Tab Page, whose OneGoogle module and logging reach
		// ogads-pa.clients6.google.com and play.google.com/log — MEASURED 2026-08-16 inside a gate
		// session that had visited nothing but 127.0.0.1, and NOT stopped by
		// --disable-background-networking above, which does not cover the NTP's own modules.
		//
		// The fence refused all of it, so nothing left the machine. The damage was elsewhere: a
		// refusal is recorded per session and the next act reports the head of that record (see
		// chrome.Driver.newRefusal), so the browser's own traffic was being handed to the agent as
		// the consequence of ITS click. Removing the source is the fix; there is no way for the
		// fence to tell whose request it stopped once it has stopped it.
		//
		// Agent mode only. A person's window starting on a blank page instead of their New Tab Page
		// would be taking something away from them to solve a problem that is not theirs.
		"about:blank",
	), nil
}

// WebRTCIsNotFencedHere documents, in a place a reader will actually reach, that no flag in Args
// closes the WebRTC hole (spec §6.2b).
//
// The spike tried: CSP `webrtc 'block'` (ignored by Chrome 151), --disable-features=WebRtc,
// --disable-blink-features=RTCPeerConnection and --disable-webrtc (none remove the global), deleting
// the global on every new document (survives in a cross-site iframe even with recursive auto-attach
// and the target paused before it runs), and --force-webrtc-ip-handling-policy=disable_non_proxied_udp
// together with a working proxy — a UDP packet still reached a page-chosen address.
//
// It is a constant rather than a comment so that a future change that believes it has fixed this has
// something to delete, and a reviewer has something to grep for.
const WebRTCIsNotFencedHere = "spec §6.2b: WebRTC egress is an open hole; no command-line flag closes it"

// String renders an argv for a log line without leaking the profile path's contents.
func String(args []string) string {
	return fmt.Sprintf("%v", args)
}
