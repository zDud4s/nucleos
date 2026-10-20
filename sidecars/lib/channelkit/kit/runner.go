package kit

import (
	"context"
	"errors"
	"strconv"
	"sync"
	"time"

	"nucleoschannelkit/backoff"
	"nucleoschannelkit/clock"
	"nucleoschannelkit/contract"
)

// outboxWaitSecs is the server-side wait of every outbox long-poll (spec 6.1).
const outboxWaitSecs = 30

// Runner drives the outbox: it keeps a long-poll open and dispatches what the
// core leases to the Adapter, each action in its own goroutine (D-B6).
type Runner struct {
	Client   *Client
	Adapter  Adapter
	Clock    clock.Clock // nil means the client's clock
	Inflight *Inflight   // nil means a fresh set
	Stats    *Stats      // nil means a private set of counters
	// Guard may veto an action before Execute by returning the ack to send
	// instead (capability guard, D-B7). Nil means no guard.
	Guard func(contract.Capabilities, contract.Action) *contract.Ack
	// ShutdownGrace bounds how long Run, once its context is cancelled, waits for
	// in-flight executions and their final acks. Zero means DefaultShutdownGrace.
	ShutdownGrace time.Duration
}

// Run polls the outbox until ctx ends or the core answers with a fatal status.
// A network or 5xx failure backs off (backoff.Outbox) and resumes. Run returns
// only after every goroutine it started has finished or, once ctx is cancelled,
// after ShutdownGrace at the latest (a stuck adapter is abandoned then); a
// cancelled ctx returns nil. In-flight executions keep their lease renewal and
// settle their ack, with retries, within that grace (see runAction).
func (r *Runner) Run(ctx context.Context) error {
	clk := r.Clock
	if clk == nil {
		clk = r.Client.clock
	}
	if r.Inflight == nil {
		r.Inflight = NewInflight()
	}
	if r.Stats == nil {
		r.Stats = &Stats{}
	}
	client := r.Client.WithStats(r.Stats)
	caps := r.Adapter.Capabilities()

	// hard outlives ctx by the shutdown grace: renewals and acks run on it.
	hard, hardCancel := context.WithCancel(context.WithoutCancel(ctx))
	defer hardCancel()

	var wg sync.WaitGroup
	err := r.loop(ctx, hard, &wg, clk, client, caps)
	done := make(chan struct{})
	go func() { wg.Wait(); close(done) }()
	r.drain(ctx, clk, done)
	return err
}

// loop polls the outbox until ctx ends or a fatal error, dispatching what it gets.
func (r *Runner) loop(ctx, hard context.Context, wg *sync.WaitGroup, clk clock.Clock, client *Client, caps contract.Capabilities) error {
	delay := backoff.Outbox.Base
	for {
		batch, err := client.Poll(ctx, outboxWaitSecs)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			if !errors.Is(err, ErrTransient) {
				return err
			}
			logf("channelkit: outbox poll failed, retrying in %s: %v", delay, err)
			if !backoff.Sleep(ctx, clk, delay) {
				return nil
			}
			delay = backoff.Next(delay, backoff.Outbox.Base, backoff.Outbox.Max)
			continue
		}
		delay = backoff.Outbox.Base
		for _, it := range batch.Items {
			r.dispatch(ctx, hard, wg, client, caps, it)
		}
		for _, conv := range batch.Typing {
			wg.Add(1)
			go func() {
				defer wg.Done()
				if err := r.Adapter.Typing(ctx, conv); err != nil && ctx.Err() == nil {
					logf("channelkit: typing in %q: %v", conv, err)
				}
			}()
		}
	}
}

// dispatch starts the goroutine that handles one outbox item.
func (r *Runner) dispatch(ctx, hard context.Context, wg *sync.WaitGroup, client *Client, caps contract.Capabilities, it Item) {
	if it.Rescue != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			r.ackLogged(hard, client, it.ID, *it.Rescue)
		}()
		return
	}
	if it.Action == nil {
		return
	}
	key := strconv.FormatUint(it.ID, 10)
	if !r.Inflight.TryAdd(key) {
		// Redelivered while still running: the execution in flight settles it.
		return
	}
	wg.Add(1)
	go func() {
		defer wg.Done()
		defer r.Inflight.Done(key)
		r.runAction(ctx, hard, client, caps, it.Action)
	}()
}
