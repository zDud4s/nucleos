package fakecore

import (
	"encoding/json"
	"net/http"
	"strconv"
	"time"

	"nucleoschannelkit/contract"
)

// mountOutbox registers the outbox long-poll and the lease renewal routes.
func (c *Core) mountOutbox() {
	c.outbox = newQueue(c.clk.Now)
	c.mux.HandleFunc("GET /channels/{channel}/outbox", c.authed(RouteOutbox, c.handleOutbox))
	c.mux.HandleFunc("POST /channels/{channel}/outbox/{id}/renew", c.authed(RouteRenew, c.handleRenew))
}

// Enqueue stores a new pending action and wakes the long-poll waiters. It
// returns the outbox id the core assigned (it replaces the action's own ID).
func (c *Core) Enqueue(a contract.Action) uint64 {
	c.mu.Lock()
	defer c.mu.Unlock()
	id := c.outbox.Enqueue(a)
	c.notifyLocked()
	return id
}

// EnqueueTyping queues an ephemeral typing hint for the next poll.
func (c *Core) EnqueueTyping(conv string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.outbox.EnqueueTyping(conv)
	c.notifyLocked()
}

// Delivery reports the state and delivery count of an outbox entry (ok is false
// for an unknown id). It applies no expiry: it shows what the core has recorded.
func (c *Core) Delivery(id uint64) (state State, attempts int, ok bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	e, found := c.outbox.Get(id)
	return e.State, e.Attempts, found
}

// handleOutbox is the long-poll: it answers as soon as Take yields something,
// otherwise it waits for a change (insertion, ack, renew), for the next instant
// at which the queue may change by itself (lease deadline, due_at, immediate
// expiry), or for the end of wait. All waiting is on the fake clock through a
// single timer, so a parked poll accounts for exactly one pending timer.
func (c *Core) handleOutbox(w http.ResponseWriter, r *http.Request) {
	wait := 0
	if raw := r.URL.Query().Get("wait"); raw != "" {
		n, err := strconv.Atoi(raw)
		if err != nil || n < 0 {
			c.writeFixture(w, http.StatusBadRequest, "errors/bad-request.json")
			return
		}
		wait = n
	}
	deadline := c.clk.Now().Add(time.Duration(wait) * time.Second)
	for {
		now := c.clk.Now()
		c.mu.Lock()
		taken := c.outbox.Take(now)
		changed := c.changed
		wake, scheduled := c.outbox.NextWake()
		c.mu.Unlock()
		if len(taken) > 0 || !now.Before(deadline) {
			writeOutbox(w, taken)
			return
		}
		target := deadline
		// A wake instant that is not in the future was already considered by
		// Take (e.g. a retry blocked behind a live lease); it would only spin.
		if scheduled && wake.After(now) && wake.Before(target) {
			target = wake
		}
		timer := c.clk.NewTimer(target.Sub(now))
		select {
		case <-changed:
		case <-timer.C():
		case <-r.Context().Done():
			timer.Stop()
			return
		}
		timer.Stop()
	}
}

func writeOutbox(w http.ResponseWriter, taken []entry) {
	actions := []json.RawMessage{}
	ephemeral := []contract.Typing{}
	for _, e := range taken {
		if e.Ephemeral {
			ephemeral = append(ephemeral, contract.Typing{Type: contract.EphemeralTyping, Conversation: e.Conversation})
			continue
		}
		b, _ := json.Marshal(e.Action)
		actions = append(actions, b)
	}
	writeJSON(w, http.StatusOK, contract.OutboxResponse{Actions: actions, Ephemeral: ephemeral})
}

func (c *Core) handleRenew(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.ParseUint(r.PathValue("id"), 10, 64)
	if err != nil {
		c.writeFixture(w, http.StatusBadRequest, "errors/bad-request.json")
		return
	}
	c.mu.Lock()
	until, err := c.outbox.Renew(id, c.clk.Now())
	if err == nil {
		// The next wake instant moved; let parked polls recompute it.
		c.notifyLocked()
	}
	c.mu.Unlock()
	if err != nil {
		c.writeFixture(w, http.StatusConflict, "errors/not-leased.json")
		return
	}
	writeJSON(w, http.StatusOK, contract.RenewResponse{LeaseUntil: until.UTC().Format(time.RFC3339)})
}
