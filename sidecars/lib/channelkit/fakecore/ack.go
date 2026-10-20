package fakecore

import (
	"net/http"
	"strconv"
	"time"

	"nucleoschannelkit/contract"
)

// Retry backoff of the fake core (spec section 6.2): 2 s doubling per delivery,
// capped at 5 min. A retry_after_ms from the adapter wins when it is larger and
// is never truncated (its own ceiling is contract.MaxRetryAfterMS).
const (
	backoffBase = 2 * time.Second
	backoffCap  = 5 * time.Minute
)

// Backoff is the exponential delay after the n-th delivery ended in a retryable
// failure: 2 s * 2^(n-1), at most 5 min. n < 1 is treated as 1.
func Backoff(n int) time.Duration {
	d := backoffBase
	for i := 1; i < n && d < backoffCap; i++ {
		d *= 2
	}
	if d > backoffCap {
		d = backoffCap
	}
	return d
}

// mountAck registers the ack route (renew is mounted with the outbox).
func (c *Core) mountAck() {
	c.mux.HandleFunc("POST /channels/{channel}/outbox/{id}/ack", c.authed(RouteAck, c.handleAck))
}

// Fallback sets the hook called (outside the lock, before the ack is answered)
// after a non-retryable ack failed an action, with the failure reason and the
// action. The real core uses it to re-send a thread_not_found action to the home
// conversation; the hook may call Enqueue. It is not called for retryable
// failures, nor for late acks that were ignored.
func (c *Core) Fallback(fn func(reason string, failed contract.Action)) {
	c.mu.Lock()
	c.fallback = fn
	c.mu.Unlock()
}

// PlatformRefs returns the platform refs stored by the ok ack of an action.
func (c *Core) PlatformRefs(id uint64) []string {
	c.mu.Lock()
	defer c.mu.Unlock()
	e, _ := c.outbox.Get(id)
	return e.PlatformRefs
}

// DueAt returns the instant before which a pending action is not redelivered
// (zero when it is due now; ok is false for an unknown id).
func (c *Core) DueAt(id uint64) (time.Time, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	e, ok := c.outbox.Get(id)
	return e.DueAt, ok
}

func (c *Core) handleAck(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseUint(r.PathValue("id"), 10, 64)
	if err != nil {
		c.writeFixture(w, http.StatusBadRequest, "errors/bad-request.json")
		return
	}
	body, err := readBody(r, 1<<20)
	if err != nil {
		w.WriteHeader(http.StatusBadRequest)
		return
	}
	ack, err := contract.DecodeAck(body)
	if err != nil {
		writeContractError(w, err)
		return
	}

	c.mu.Lock()
	cur, found := c.outbox.Get(id)
	if !found {
		c.mu.Unlock()
		writeJSON(w, http.StatusNotFound, map[string]string{"error": "not_found"})
		return
	}
	var (
		state    State
		applied  bool
		fallback func(string, contract.Action)
		reason   string
	)
	switch {
	case ack.OK:
		state, applied = c.outbox.Complete(id, ack.PlatformRefs)
	case ack.Failed.Retryable:
		delay := Backoff(cur.Attempts)
		if ms := ack.Failed.RetryAfterMS; ms != nil {
			if d := time.Duration(*ms) * time.Millisecond; d > delay {
				delay = d
			}
		}
		state, applied = c.outbox.Retry(id, c.clk.Now().Add(delay))
	default:
		state, applied = c.outbox.FailPermanent(id)
		if applied {
			fallback, reason = c.fallback, ack.Failed.Reason
		}
	}
	if applied {
		// The next wake instant changed (due_at, unblocked conversation).
		c.notifyLocked()
	}
	c.mu.Unlock()

	if fallback != nil {
		fallback(reason, cur.Action)
	}
	writeJSON(w, http.StatusOK, contract.AckResponse{State: contract.AckState(state), Ignored: !applied})
}
