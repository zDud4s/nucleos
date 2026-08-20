package launch

import "testing"

func TestStateReportsTheFirstUnmetCondition(t *testing.T) {
	cases := []struct {
		name   string
		health Health
		want   State
	}{
		{"fresh installation", Health{}, StateNotInstalled},
		{
			"downloaded, sidecar down",
			Health{ChromiumPresent: true},
			StateDriverDown,
		},
		{
			"sidecar up, browser silent",
			Health{ChromiumPresent: true, DriverRunning: true},
			StateUnreachable,
		},
		{
			"all three",
			Health{ChromiumPresent: true, DriverRunning: true, BrowserReachable: true},
			StateReady,
		},
		{
			// The reason the three are reported separately: a machine with no Chromium but a live
			// sidecar must still say "not installed", not "unreachable", or someone goes looking
			// at logs for a download that never happened.
			"nothing downloaded but everything else up",
			Health{DriverRunning: true, BrowserReachable: true},
			StateNotInstalled,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.health.State(); got != tc.want {
				t.Fatalf("got %q, want %q", got, tc.want)
			}
		})
	}
}

// TestAFreshInstallationIsNotAFault. Paging someone because a download has not happened yet teaches
// them to ignore the readout.
func TestAFreshInstallationIsNotAFault(t *testing.T) {
	if (Health{}).IsFault() {
		t.Error("a fresh installation was reported as a fault")
	}
	if !(Health{ChromiumPresent: true}).IsFault() {
		t.Error("a dead sidecar should be a fault")
	}
	if !(Health{ChromiumPresent: true, DriverRunning: true}).IsFault() {
		t.Error("an unreachable browser should be a fault")
	}
	if (Health{ChromiumPresent: true, DriverRunning: true, BrowserReachable: true}).IsFault() {
		t.Error("a ready browser was reported as a fault")
	}
}

func TestEveryUnreadyStateSaysWhatToDo(t *testing.T) {
	for _, health := range []Health{
		{},
		{ChromiumPresent: true},
		{ChromiumPresent: true, DriverRunning: true},
	} {
		if health.Remedy() == "" {
			t.Errorf("%v has no remedy line", health.State())
		}
	}
	ready := Health{ChromiumPresent: true, DriverRunning: true, BrowserReachable: true}
	if ready.Remedy() != "" {
		t.Errorf("ready should have no remedy, got %q", ready.Remedy())
	}
}
