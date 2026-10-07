// §spec browser-volante

package chrome

import "sync"

// chainRecorder is where a person's navigation is written down, in order: the destination and every
// host a login crossed on the way (spec §5.3a). Shared by the headful [Human] recorder and the
// person's turn in the agent's own browser, so the two cannot disagree about what a chain is.
type chainRecorder struct {
	mu      sync.Mutex
	chain   []string
	stopped bool
}

// record appends a url unless it is the one already at the end.
//
// Consecutive duplicates only. A chain that returns to where it started — jira, google, jira — is
// what a real login looks like, and collapsing that to a set here would take from `grant` the one
// piece of information it uses to tell the destination from the identity provider.
func (r *chainRecorder) record(url string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.stopped {
		return
	}
	if len(r.chain) > 0 && r.chain[len(r.chain)-1] == url {
		return
	}
	r.chain = append(r.chain, url)
}

// Chain is what the person's navigation produced, in order.
func (r *chainRecorder) Chain() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]string(nil), r.chain...)
}

// stop ends the recording: nothing after it is part of the person's login.
func (r *chainRecorder) stop() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.stopped = true
}
