package kit

import (
	"context"
	"runtime/debug"
	"time"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
)

// DefaultShutdownGrace is how long Run keeps waiting for in-flight executions, and
// how long their final acks may keep retrying, once its context is cancelled.
const DefaultShutdownGrace = 10 * time.Second

// ReasonAdapterPanic is the failure reason of an ack sent for an Execute that panicked.
const ReasonAdapterPanic = "adapter_panic"

// shutdownGrace returns the configured grace period or the default.
func (r *Runner) shutdownGrace() time.Duration {
	if r.ShutdownGrace > 0 {
		return r.ShutdownGrace
	}
	return DefaultShutdownGrace
}

// runAction executes one leased action and acknowledges the outcome.
//
// ctx is the Run context: it is the one Execute sees, and it is cancelled on
// shutdown. hard outlives it by the shutdown grace (Run cancels it when the grace
// elapses): the lease renewal and the ack, with its retries, run on hard, so an
// action that finishes while the kit is stopping is still settled. A cancelled
// Execute sends no ack at all: the lease expires and the core redelivers (D-B6).
// The caller releases the Inflight entry when runAction returns, i.e. after the
// ack settled, whatever the outcome (panic included).
func (r *Runner) runAction(ctx, hard context.Context, client *Client, caps contract.Capabilities, a contract.Action) {
	id := a.ActionID()
	if r.Guard != nil {
		if ack := r.Guard(caps, a); ack != nil {
			r.ackLogged(hard, client, id, *ack)
			return
		}
	}
	lease := NewRenewer(client).Start(hard, id)
	defer lease.Stop()

	r.Stats.Executions.Add(1)
	refs, err := r.execute(ctx, a)
	ack, send := AckFromResult(refs, err)
	if !send {
		return
	}
	r.ackLogged(hard, client, id, ack)
}

// execute calls the adapter and turns a panic into a retryable adapter_panic failure.
func (r *Runner) execute(ctx context.Context, a contract.Action) (refs []string, err error) {
	defer func() {
		if p := recover(); p != nil {
			r.Stats.Panics.Add(1)
			logf("channelkit: adapter panic executing action %d: %v\n%s", a.ActionID(), p, debug.Stack())
			refs = nil
			err = Retryable(ReasonAdapterPanic, 0)
		}
	}()
	return r.Adapter.Execute(ctx, a)
}

// ackLogged sends an ack with retry and logs a failure to settle it.
func (r *Runner) ackLogged(ctx context.Context, client *Client, id uint64, ack contract.Ack) {
	if _, err := client.AckWithRetry(ctx, id, ack); err != nil && ctx.Err() == nil {
		logf("channelkit: ack %d: %v", id, err)
	}
}

// drain waits until done is closed. Once ctx is cancelled it waits at most the
// shutdown grace, measured on clk; the caller then cancels the hard context, which
// aborts every pending ack and renewal, and Run returns.
func (r *Runner) drain(ctx context.Context, clk clock.Clock, done <-chan struct{}) {
	select {
	case <-done:
		return
	case <-ctx.Done():
	}
	timer := clk.NewTimer(r.shutdownGrace())
	defer timer.Stop()
	select {
	case <-done:
	case <-timer.C():
		logf("channelkit: shutdown grace %s elapsed with executions still in flight", r.shutdownGrace())
	}
}
