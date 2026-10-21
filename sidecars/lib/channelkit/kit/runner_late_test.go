package kit

import (
	"context"
	"net/http"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/fakecore"
)

// lateCycles is the number of deliveries after which the core gives an action up
// (fakecore.MaxAttempts, spec 6.2).
const lateCycles = fakecore.MaxAttempts

func TestRunnerLateAckAfterCoreFailedTheActionIsIgnored(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "slow"))
	// Every renewal fails with 409, so the lease is never extended: the core lets
	// it expire and redelivers on each cycle.
	g.core.FailNext(fakecore.RouteRenew, lateCycles, http.StatusConflict, `{"error":"not_leased"}`)
	stop := g.start()

	// 10 deliveries, each one cycle of: BlockUntil(2) (renewal timer + parked
	// long-poll); Advance(30s) (the renewal gets 409); BlockUntil(1) (only the
	// long-poll is left); Advance(30s) (lease expired: 60s per delivery). The 10th
	// expiry exhausts the attempts and the core marks the action failed.
	for k := 1; k <= lateCycles; k++ {
		if k == 1 {
			g.waitEntered(entered)
		} else {
			g.waitSkipped(int64(k - 1)) // redelivered while still running
		}
		g.clk.BlockUntil(2)
		polls := g.core.Calls(fakecore.RouteOutbox)
		g.clk.Advance(30 * time.Second)
		g.waitCalls(fakecore.RouteRenew, k)
		g.waitCalls(fakecore.RouteOutbox, polls+1)
		g.clk.BlockUntil(1)
		g.clk.Advance(30 * time.Second)
	}
	g.waitState(id, fakecore.StateFailed)
	if _, attempts, _ := g.core.Delivery(id); attempts != lateCycles {
		t.Fatalf("attempts = %d, want %d", attempts, lateCycles)
	}

	release() // the execution finishes long after the core gave up
	g.waitCalls(fakecore.RouteAck, 1)
	g.waitInflight(0)
	stop()

	if st, _, _ := g.core.Delivery(id); st != fakecore.StateFailed {
		t.Fatalf("state = %q after the late ok, want %q", st, fakecore.StateFailed)
	}
	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one (no re-execution)", got)
	}
	snap := g.runner.Stats.Snapshot()
	if snap.LateAcks != 1 {
		t.Fatalf("late acks = %d, want 1", snap.LateAcks)
	}
	if snap.AckRejected != 0 {
		t.Fatalf("ack rejected = %d, want 0 (an ignored ack is not an error)", snap.AckRejected)
	}
}

func TestRunnerLateOkAckAfterFallbackDoesNotResurrectTheAction(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	reasons := make(chan string, 1)
	g.core.Fallback(func(reason string, failed contract.Action) {
		reasons <- reason
		g.core.Enqueue(loopSend("home", "fallback")) // same action for the home conversation
	})
	id := g.core.Enqueue(loopSend("thread", "slow"))
	stop := g.start()
	g.waitEntered(entered)

	// The core hears a non-retryable thread_not_found for the action while the
	// execution is still running: it fails it and runs the fallback.
	res, err := g.runner.Client.Ack(context.Background(), id, contract.Ack{
		Failed: &contract.Failed{Retryable: false, Reason: "thread_not_found"},
	})
	if err != nil || res.Ignored {
		t.Fatalf("failing ack = %+v, %v, want applied", res, err)
	}
	g.waitState(id, fakecore.StateFailed)
	select {
	case r := <-reasons:
		if r != "thread_not_found" {
			t.Fatalf("fallback reason = %q, want thread_not_found", r)
		}
	default:
		t.Fatal("the fallback hook did not run")
	}

	release() // the original execution now finishes and acks ok, late
	g.waitInflight(0)
	g.waitState(2, fakecore.StateDone) // the fallback copy ran too
	stop()

	if st, _, _ := g.core.Delivery(id); st != fakecore.StateFailed {
		t.Fatalf("state = %q after the late ok, want %q (not resurrected)", st, fakecore.StateFailed)
	}
	if refs := g.core.PlatformRefs(id); len(refs) != 0 {
		t.Fatalf("platform refs = %v, want none: the late ok must not be stored", refs)
	}
	if got := g.runner.Stats.Snapshot().LateAcks; got != 1 {
		t.Fatalf("late acks = %d, want 1", got)
	}
	if got := ad.executions(); len(got) != 2 || got[0] != id {
		t.Fatalf("executions = %v, want the original then its fallback copy", got)
	}
}

func TestRunnerLateRenew409DoesNotKillTheExecution(t *testing.T) {
	ad, entered, release := blockingAdapter()
	g := newLoopRig(t, ad)
	id := g.core.Enqueue(loopSend("c1", "slow"))
	g.core.FailNext(fakecore.RouteRenew, 1, http.StatusConflict, `{"error":"not_leased"}`)
	stop := g.start()
	g.waitEntered(entered)
	g.clk.BlockUntil(2)
	g.clk.Advance(30 * time.Second) // the renewal gets 409
	g.waitCalls(fakecore.RouteRenew, 1)
	g.waitCalls(fakecore.RouteOutbox, 2)
	g.clk.BlockUntil(1) // only the long-poll is left: the renewal gave up
	g.clk.Advance(29 * time.Second)
	if n := g.core.Calls(fakecore.RouteRenew); n != 1 {
		t.Fatalf("renew calls = %d, want 1 (renewal stopped after the 409)", n)
	}

	release() // the execution is still alive and finishes inside the real lease
	g.waitState(id, fakecore.StateDone)
	g.waitInflight(0)
	stop()

	if got := ad.executions(); len(got) != 1 {
		t.Fatalf("executions = %v, want exactly one", got)
	}
	if refs := g.core.PlatformRefs(id); len(refs) != 1 || refs[0] != "m1" {
		t.Fatalf("platform refs = %v, want [m1]", refs)
	}
	snap := g.runner.Stats.Snapshot()
	if snap.LateAcks != 0 || snap.AckRejected != 0 {
		t.Fatalf("late acks = %d, ack rejected = %d, want 0 and 0 (a normal ack)", snap.LateAcks, snap.AckRejected)
	}
}

func TestRunnerLateCrashBetweenSendAndAckReexecutesAfterHello(t *testing.T) {
	// Process one executes (the message reaches the platform) and dies before it can
	// ack: cancelling its context makes the blocked Execute return without an ack,
	// which leaves the lease with the core exactly as a crash would.
	ad1, entered, _ := blockingAdapter()
	g := newLoopRig(t, ad1)
	id := g.core.Enqueue(loopSend("c1", "sent but never acked"))
	stop1 := g.start()
	g.waitEntered(entered)
	stop1()
	if st, _, _ := g.core.Delivery(id); st != fakecore.StateLeased {
		t.Fatalf("state = %q after the crash, want %q (no ack was sent)", st, fakecore.StateLeased)
	}
	if n := g.core.Calls(fakecore.RouteAck); n != 0 {
		t.Fatalf("ack calls = %d after the crash, want 0", n)
	}

	// Process two: an empty Inflight, its own adapter, and a hello that tells the
	// core the previous process's leases are gone (the Runner does not own the hello yet).
	ad2 := &loopAdapter{exec: func(context.Context, contract.Action) ([]string, error) {
		return []string{"m9"}, nil
	}}
	r2 := &Runner{Client: g.runner.Client, Adapter: ad2, Clock: g.clk, Inflight: NewInflight(), Stats: &Stats{}}
	if _, err := r2.Client.Hello(context.Background(), testHello()); err != nil {
		t.Fatalf("hello of the new process: %v", err)
	}
	if st, _, _ := g.core.Delivery(id); st != fakecore.StatePending {
		t.Fatalf("state = %q after the hello, want %q (lease released)", st, fakecore.StatePending)
	}

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- r2.Run(ctx) }()
	defer func() {
		cancel()
		select {
		case err := <-done:
			if err != nil {
				t.Errorf("second Run returned %v after cancel", err)
			}
		case <-time.After(10 * time.Second):
			t.Error("second Run did not return after cancel")
		}
	}()
	g.waitState(id, fakecore.StateDone)

	// A duplicate send is the accepted price of a crash between send and ack (spec 6.2).
	if a, b := ad1.executions(), ad2.executions(); len(a) != 1 || len(b) != 1 || a[0] != id || b[0] != id {
		t.Fatalf("executions = %v then %v, want the action once in each process", a, b)
	}
	if refs := g.core.PlatformRefs(id); len(refs) != 1 || refs[0] != "m9" {
		t.Fatalf("platform refs = %v, want [m9]", refs)
	}
	if _, attempts, _ := g.core.Delivery(id); attempts != 2 {
		t.Fatalf("attempts = %d, want 2", attempts)
	}
}
