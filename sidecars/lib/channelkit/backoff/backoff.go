// Package backoff is the exponential backoff shared by the channel sidecars:
// the delay step and an interruptible wait on the injectable clock.
package backoff

import (
	"context"
	"time"

	"nucleoschannelkit/clock"
)

// Policy bounds a retry delay: it starts at Base and doubles up to Max.
type Policy struct {
	Base time.Duration
	Max  time.Duration
}

var (
	// Updates paces retries of a failed long-poll for inbound updates.
	Updates = Policy{Base: 2 * time.Second, Max: 60 * time.Second}
	// Outbox paces retries of a failed outbound action.
	Outbox = Policy{Base: 2 * time.Second, Max: 5 * time.Minute}
)

// Next doubles cur, kept within [base, max].
func Next(cur, base, max time.Duration) time.Duration {
	next := cur * 2
	if next > max {
		next = max
	}
	if next < base {
		next = base
	}
	return next
}

// Sleep waits d on clk. It reports false if ctx ends first.
func Sleep(ctx context.Context, clk clock.Clock, d time.Duration) bool {
	timer := clk.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C():
		return true
	}
}
