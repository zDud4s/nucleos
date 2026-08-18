package launch

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"nucleosbrowser/browser"
)

// The WebRTC hole of spec §6.2b, closed — and closed by the only mechanism found that works.
//
// # What was tried and did not work
//
// The spike tried six: CSP `webrtc 'block'` (ignored), --disable-webrtc, --disable-features=WebRtc,
// --disable-blink-features=RTCPeerConnection, deleting the global on every new document, and
// --force-webrtc-ip-handling-policy=disable_non_proxied_udp. Two of those were re-measured on
// 2026-08-16 against the pinned build with a real instrument and still leaked. No command line
// closes this.
//
// What was left was `WebRtcIPHandlingPolicy` as an ENTERPRISE POLICY, and that turned out to need an
// elevated shell — `HKCU\SOFTWARE\Policies` grants write to SYSTEM and Administrators only, by
// design, so even the per-user path is not the user's to set. A pillar whose containment depended on
// the owner having run something as admin would be a pillar that is off on most machines and says it
// is on.
//
// # What does work
//
// That policy maps to an ordinary PROFILE PREFERENCE, and the profile is ours. Writing
// `webrtc.ip_handling_policy` into `<profile>/Default/Preferences` before the browser starts does
// what the switch would not. MEASURED 2026-08-19 against the pinned Chromium: without it the STUN
// binding request reaches a socket the page named, three runs out of three; with it, nothing leaves,
// three runs out of three, with the page proven to have run and reached setLocalDescription both
// times. See gate.TestWebRTCUDPDoesNotLeaveTheFence.
//
// # Agent mode only, and that is spec §6.4 rather than caution
//
// The person's window has no fence because they are the one acting. It is also the window that
// exists so a human can finish a login, and this is written at LAUNCH rather than kept as state, so
// the human launch simply writes the permissive value over it. Nothing has to be cleaned up after a
// crash: whatever the last launch wrote is replaced by what the next one writes, and neither reads
// the file to decide anything.
const ipHandlingPolicyPref = "ip_handling_policy"

// applyWebRTCPolicy merges the pref for `mode` into the profile, creating the file if it is absent.
//
// MERGES, and the difference is the whole profile. `Preferences` is where Chromium keeps everything
// about this profile that is not a cookie — zoom levels, permissions, the lot — so writing the file
// wholesale would throw away a profile's history every time it started. Read, set one key, write
// back.
func applyWebRTCPolicy(profileDir string, mode browser.Mode) error {
	path := filepath.Join(profileDir, "Default", "Preferences")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("preparing the profile's preference directory: %w", err)
	}

	prefs := map[string]any{}
	switch raw, err := os.ReadFile(path); {
	case err == nil:
		if err := json.Unmarshal(raw, &prefs); err != nil {
			// Not fatal, and deliberately so. A Preferences file we cannot parse is one Chromium is
			// about to rewrite anyway; refusing to launch over it would take the pillar down for a
			// reason that fixes itself. What must not happen is launching WITHOUT the policy, so the
			// map is reset and the key is written on its own.
			prefs = map[string]any{}
		}
	case os.IsNotExist(err):
	default:
		return fmt.Errorf("reading the profile's preferences: %w", err)
	}

	section, _ := prefs["webrtc"].(map[string]any)
	if section == nil {
		section = map[string]any{}
	}
	// `default` and not the empty string for human mode: the empty string is a value Chromium reads
	// as "no preference expressed", which is the same outcome by luck rather than by statement.
	if mode == browser.ModeAgent {
		section[ipHandlingPolicyPref] = "disable_non_proxied_udp"
	} else {
		section[ipHandlingPolicyPref] = "default"
	}
	prefs["webrtc"] = section

	encoded, err := json.Marshal(prefs)
	if err != nil {
		return fmt.Errorf("encoding the profile's preferences: %w", err)
	}
	if err := os.WriteFile(path, encoded, 0o644); err != nil {
		return fmt.Errorf("writing the profile's preferences: %w", err)
	}
	return nil
}
