// §spec pilar-de-browser

package launch

// Health is spec §9.4: three questions, reported separately.
//
// Collapsing them into one line was rejected for a concrete reason — the first run of a fresh
// installation has no Chromium yet, and a single "browser: unhealthy" makes that look like a broken
// system rather than a download that has not happened. Three causes with three different remedies:
// wait for a download, restart a process, look at why the browser is not answering.
type Health struct {
	// ChromiumPresent — is the pinned binary on disk?  Remedy: download it.
	ChromiumPresent bool
	// DriverRunning — is the process that drives it alive?  Remedy: the supervisor restarts it.
	DriverRunning bool
	// BrowserReachable — does the browser answer?  Remedy: look at the logs; something crashed or
	// is wedged.
	BrowserReachable bool
}

// State is the single word a readout shows, chosen from the FIRST unmet condition rather than from
// all of them, so the reported cause is the one worth acting on. Same argument `trust.rs` makes
// about reporting the first thing that was wrong.
type State string

const (
	// StateNotInstalled is the normal state of a fresh installation, not a fault.
	StateNotInstalled State = "not-installed"
	StateDriverDown   State = "driver-down"
	StateUnreachable  State = "unreachable"
	StateReady        State = "ready"
)

// State reduces the three flags to one word.
func (h Health) State() State {
	switch {
	case !h.ChromiumPresent:
		return StateNotInstalled
	case !h.DriverRunning:
		return StateDriverDown
	case !h.BrowserReachable:
		return StateUnreachable
	default:
		return StateReady
	}
}

// IsFault says whether this state deserves anyone's attention.
//
// StateNotInstalled is not a fault: it is what every installation looks like before the first
// download, and paging someone for it teaches them to ignore the readout.
func (h Health) IsFault() bool {
	state := h.State()
	return state == StateDriverDown || state == StateUnreachable
}

// Remedy is what a person can do about it, in one line, for the readout to show next to the state.
func (h Health) Remedy() string {
	switch h.State() {
	case StateNotInstalled:
		return "the pinned Chromium has not been downloaded yet"
	case StateDriverDown:
		return "the browser sidecar is not running; the supervisor restarts it"
	case StateUnreachable:
		return "the browser is not answering; check the sidecar log"
	default:
		return ""
	}
}
