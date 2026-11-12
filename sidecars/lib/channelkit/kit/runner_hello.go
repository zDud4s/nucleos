package kit

import (
	"context"
	"errors"
	"sync"
	"time"

	"nucleoschannelkit/backoff"
	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
)

// ReconnectAfterErrors is how many consecutive network or 5xx outbox failures make
// the Runner treat the core as having been away: the first poll after them is
// preceded by a fresh hello (D-B8). One or two failures followed by a success
// do not (the core never noticed anything).
const ReconnectAfterErrors = 3

// defaultKind is the hello kind of a Runner whose Kind is empty.
const defaultKind = "adapter"

// helloBody is the hello this Runner presents: its kind, the contract version it
// speaks and what the adapter declares it can do.
func (r *Runner) helloBody(caps contract.Capabilities) contract.Hello {
	kind := r.Kind
	if kind == "" {
		kind = defaultKind
	}
	return contract.Hello{Kind: kind, Version: contract.Version, Capabilities: &caps}
}

// handshake sends the hello and repeats it with backoff on a transient failure
// (network, 5xx), sharing delay with the poll loop so the pace keeps doubling
// across both. It returns ok=false with no error when ctx ended first, and an
// error (ErrIncompatible, ErrFatal) when the core rejects the adapter for good:
// the process must then terminate.
func (r *Runner) handshake(ctx context.Context, clk clock.Clock, client *Client, caps contract.Capabilities, delay *time.Duration) (ok bool, err error) {
	for {
		_, err := client.Hello(ctx, r.helloBody(caps))
		if err == nil {
			return true, nil
		}
		if ctx.Err() != nil {
			return false, nil
		}
		if !errors.Is(err, ErrTransient) {
			return false, err
		}
		logf("channelkit: hello failed, retrying in %s: %v", *delay, err)
		if !backoff.Sleep(ctx, clk, *delay) {
			return false, nil
		}
		*delay = backoff.Next(*delay, backoff.Outbox.Base, backoff.Outbox.Max)
	}
}

// fatalState records the first fatal error met outside the poll loop (an ack
// answered 401/403/409) and stops the run so Run can return it.
type fatalState struct {
	mu     sync.Mutex
	err    error
	cancel context.CancelFunc
}

// beginRun readies the fatal bookkeeping for one Run.
func (r *Runner) beginRun(cancel context.CancelFunc) {
	r.fatal.mu.Lock()
	defer r.fatal.mu.Unlock()
	r.fatal.err = nil
	r.fatal.cancel = cancel
}

// failFatal keeps the first fatal error and cancels the run.
func (r *Runner) failFatal(err error) {
	r.fatal.mu.Lock()
	if r.fatal.err == nil {
		r.fatal.err = err
	}
	cancel := r.fatal.cancel
	r.fatal.mu.Unlock()
	if cancel != nil {
		cancel()
	}
}

// fatalErr returns the fatal error recorded by failFatal, if any.
func (r *Runner) fatalErr() error {
	r.fatal.mu.Lock()
	defer r.fatal.mu.Unlock()
	return r.fatal.err
}
