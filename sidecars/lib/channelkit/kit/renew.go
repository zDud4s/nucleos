package kit

import (
	"context"
	"net/http"
	"strconv"
	"sync"
	"time"

	"nucleoschannelkit/backoff"
	"nucleoschannelkit/contract"
)

// RenewInterval is how often a lease is renewed (D-B6).
const RenewInterval = 30 * time.Second

// Renewer keeps the lease of in-flight actions alive.
type Renewer struct {
	client *Client
}

// NewRenewer builds a Renewer over c; it paces itself on c's clock.
func NewRenewer(c *Client) *Renewer { return &Renewer{client: c} }

// Start begins renewing the lease of action id every RenewInterval
// (POST /channels/{channel}/outbox/{id}/renew) until ctx ends, Stop is called, or the
// core says the action is no longer leased (Lost).
func (r *Renewer) Start(ctx context.Context, id uint64) *Lease {
	lctx, cancel := context.WithCancel(ctx)
	l := &Lease{r: r, id: id, ctx: lctx, cancel: cancel}
	l.mu.Lock()
	l.startLocked()
	l.mu.Unlock()
	return l
}

// Lease is a running renewal of one action.
type Lease struct {
	r      *Renewer
	id     uint64
	ctx    context.Context
	cancel context.CancelFunc
	wg     sync.WaitGroup

	mu      sync.Mutex
	active  bool
	stopped bool
	lost    chan struct{}
}

// Lost returns a channel closed when renewal stopped because the core no longer leases
// the action (409 not_leased, or a rejected renewal). After Resume it returns a fresh
// channel, so read it again.
func (l *Lease) Lost() <-chan struct{} {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.lost
}

// Resume restarts renewal after Lost (for an action redelivered while still in flight).
// It does nothing while renewal is active, or after Stop or cancellation of the context.
func (l *Lease) Resume() {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.stopped || l.active || l.ctx.Err() != nil {
		return
	}
	l.startLocked()
}

// Stop ends renewal and blocks until the goroutine has exited.
func (l *Lease) Stop() {
	l.mu.Lock()
	l.stopped = true
	l.mu.Unlock()
	l.cancel()
	l.wg.Wait()
}

// startLocked launches the renewal goroutine; the caller holds l.mu.
func (l *Lease) startLocked() {
	l.lost = make(chan struct{})
	l.active = true
	l.wg.Add(1)
	go l.run(l.lost)
}

func (l *Lease) run(lost chan struct{}) {
	defer l.wg.Done()
	clk := l.r.client.clock
	for {
		if !backoff.Sleep(l.ctx, clk, RenewInterval) {
			l.finish(nil)
			return
		}
		if l.renewOnce() {
			continue
		}
		if l.ctx.Err() != nil {
			l.finish(nil)
			return
		}
		l.finish(lost)
		return
	}
}

// finish marks the goroutine inactive and, for a lost lease, signals it.
func (l *Lease) finish(lost chan struct{}) {
	l.mu.Lock()
	l.active = false
	if lost != nil {
		close(lost)
	}
	l.mu.Unlock()
}

// renewOnce sends one renewal and reports whether renewal should go on: true after a
// success or a transient failure (tried again next cycle), false when the lease is lost.
func (l *Lease) renewOnce() bool {
	c := l.r.client
	path := c.channelPath("/outbox/" + strconv.FormatUint(l.id, 10) + "/renew")
	body, status, err := c.do(l.ctx, http.MethodPost, path, nil, "")
	if err != nil {
		return true
	}
	switch Classify(EndpointRenew, status, body) {
	case OK:
		if _, derr := contract.DecodeRenewResponse(body); derr != nil {
			logf("channelkit: renew %d: unreadable response: %v", l.id, derr)
		}
		return true
	case Retry:
		return true
	default: // StopRenewing, GiveUp, Fatal
		return false
	}
}
