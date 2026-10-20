package kit

import (
	"context"
	"errors"
	"strconv"
	"sync"

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
}

// Run polls the outbox until ctx ends or the core answers with a fatal status.
// A network or 5xx failure backs off (backoff.Outbox) and resumes. Run returns
// only after every goroutine it started has finished; a cancelled ctx returns nil.
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

	var wg sync.WaitGroup
	defer wg.Wait()

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
			r.dispatch(ctx, &wg, client, caps, it)
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
func (r *Runner) dispatch(ctx context.Context, wg *sync.WaitGroup, client *Client, caps contract.Capabilities, it Item) {
	if it.Rescue != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			r.ackLogged(ctx, client, it.ID, *it.Rescue)
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
		r.runAction(ctx, client, caps, it.Action)
	}()
}

// runAction executes one leased action and acknowledges the outcome, renewing
// the lease meanwhile. B30b replaces this body with the full version.
func (r *Runner) runAction(ctx context.Context, client *Client, caps contract.Capabilities, a contract.Action) {
	id := a.ActionID()
	if r.Guard != nil {
		if ack := r.Guard(caps, a); ack != nil {
			r.ackLogged(ctx, client, id, *ack)
			return
		}
	}
	lease := NewRenewer(client).Start(ctx, id)
	defer lease.Stop()

	r.Stats.Executions.Add(1)
	refs, err := r.Adapter.Execute(ctx, a)
	ack, send := AckFromResult(refs, err)
	if !send {
		return
	}
	r.ackLogged(ctx, client, id, ack)
}

// ackLogged sends an ack with retry and logs a failure to settle it.
func (r *Runner) ackLogged(ctx context.Context, client *Client, id uint64, ack contract.Ack) {
	if _, err := client.AckWithRetry(ctx, id, ack); err != nil && ctx.Err() == nil {
		logf("channelkit: ack %d: %v", id, err)
	}
}
