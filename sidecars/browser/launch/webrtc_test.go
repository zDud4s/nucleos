package launch

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"nucleosbrowser/browser"
)

// TestTheProfileKeepsEverythingElseWhenThePolicyIsWritten.
//
// The merge is the load-bearing part. `Preferences` holds everything Chromium knows about a profile
// that is not a cookie, so a version of this that wrote the file wholesale would silently reset a
// profile on every launch — and the profiles this touches are the ones holding the owner's logins,
// where "silently reset" is the worst available outcome.
func TestTheProfileKeepsEverythingElseWhenThePolicyIsWritten(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "Default", "Preferences")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	existing := `{"profile":{"name":"work"},"webrtc":{"something_else":true},"zoom":3}`
	if err := os.WriteFile(path, []byte(existing), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := applyWebRTCPolicy(dir, browser.ModeAgent); err != nil {
		t.Fatalf("applying: %v", err)
	}

	var after map[string]any
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(raw, &after); err != nil {
		t.Fatalf("the file is no longer json: %v", err)
	}
	if after["zoom"] != float64(3) {
		t.Errorf("an unrelated top-level key was lost: %v", after["zoom"])
	}
	if profile, _ := after["profile"].(map[string]any); profile["name"] != "work" {
		t.Errorf("an unrelated section was lost: %v", after["profile"])
	}
	webrtc, _ := after["webrtc"].(map[string]any)
	if webrtc["something_else"] != true {
		t.Errorf("a sibling key inside webrtc was lost: %v", webrtc)
	}
	if webrtc[ipHandlingPolicyPref] != "disable_non_proxied_udp" {
		t.Errorf("the policy was not written: %v", webrtc)
	}
}

// TestTheModesGetOppositePolicies. Spec §6.4: the person's window has no fence, and this is part of
// the fence. Written at every launch rather than kept as state, so the two modes cannot drift.
func TestTheModesGetOppositePolicies(t *testing.T) {
	dir := t.TempDir()

	read := func() string {
		raw, err := os.ReadFile(filepath.Join(dir, "Default", "Preferences"))
		if err != nil {
			t.Fatal(err)
		}
		var prefs map[string]any
		if err := json.Unmarshal(raw, &prefs); err != nil {
			t.Fatal(err)
		}
		webrtc, _ := prefs["webrtc"].(map[string]any)
		value, _ := webrtc[ipHandlingPolicyPref].(string)
		return value
	}

	if err := applyWebRTCPolicy(dir, browser.ModeAgent); err != nil {
		t.Fatal(err)
	}
	if got := read(); got != "disable_non_proxied_udp" {
		t.Errorf("agent mode: %q", got)
	}

	// The same directory, because it IS the same directory: the handover of §4.2 swaps the process
	// over one profile. A human launch must lift the restriction rather than inherit it.
	if err := applyWebRTCPolicy(dir, browser.ModeHuman); err != nil {
		t.Fatal(err)
	}
	if got := read(); got != "default" {
		t.Errorf("human mode did not lift it: %q", got)
	}

	// And back, because a crash in between must not leave the agent unfenced. Nothing here reads the
	// file to decide anything, so there is no state to be stale.
	if err := applyWebRTCPolicy(dir, browser.ModeAgent); err != nil {
		t.Fatal(err)
	}
	if got := read(); got != "disable_non_proxied_udp" {
		t.Errorf("agent mode did not reapply: %q", got)
	}
}

// TestAPreferencesFileNobodyCanParseStillGetsThePolicy.
//
// The choice recorded here is that unparseable preferences are not fatal. Chromium rewrites that
// file anyway, so refusing to launch would take the pillar down for something that fixes itself —
// but launching WITHOUT the policy is the one outcome that must not happen, so the key is written on
// its own rather than skipped.
func TestAPreferencesFileNobodyCanParseStillGetsThePolicy(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "Default", "Preferences")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("{not json at all"), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := applyWebRTCPolicy(dir, browser.ModeAgent); err != nil {
		t.Fatalf("applying over rubbish: %v", err)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var prefs map[string]any
	if err := json.Unmarshal(raw, &prefs); err != nil {
		t.Fatalf("still not json: %v", err)
	}
	webrtc, _ := prefs["webrtc"].(map[string]any)
	if webrtc[ipHandlingPolicyPref] != "disable_non_proxied_udp" {
		t.Errorf("the policy was not written: %v", prefs)
	}
}
